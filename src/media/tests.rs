use crate::{
    api::{self, Access, Context},
    config::{MediaConfig, MediaConnection},
    media::{model::*, service::MediaService, upstream::*},
    status::Status,
};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, watch},
    task::JoinHandle,
};

const READER: &str = "reader-test-token-000000000000000000000000";
const REQUESTER: &str = "requester-test-token-00000000000000000000";
const ADMIN: &str = "administrator-test-token-0000000000000000";
const INTEGRATION: &str = "integration-test-token-000000000000000000";
#[derive(Default)]
struct NativeState {
    movie: Option<UpMovie>,
    series: Option<UpSeries>,
    episodes: Vec<UpEpisode>,
    monitored: Vec<Vec<u32>>,
    add_count: u32,
    commands: Vec<UpCommand>,
    lose_add: bool,
    unavailable: bool,
    queue_unavailable: bool,
    import_on_monitor: bool,
    unmonitor_on_import: bool,
}
struct Native {
    url: String,
    state: Arc<Mutex<NativeState>>,
    task: JoinHandle<()>,
}
impl Native {
    async fn start() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(NativeState::default()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let state = shared.clone();
                tokio::spawn(async move {
                    handle_native(stream, state).await;
                });
            }
        });
        Self { url, state, task }
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceivedAdd {
    tmdb_id: u32,
    quality_profile_id: u32,
    root_folder_path: String,
    monitored: bool,
    add_options: ReceivedAddOptions,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceivedAddOptions {
    search_for_movie: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceivedMonitoring {
    episode_ids: Vec<u32>,
    monitored: bool,
}
#[derive(Serialize)]
struct EmptyQueue {
    records: Vec<()>,
    #[serde(rename = "totalRecords")]
    total_records: u32,
}
async fn handle_native(mut stream: TcpStream, state: Arc<Mutex<NativeState>>) {
    let mut input = vec![];
    let mut chunk = [0u8; 4096];
    let header_end;
    loop {
        let Ok(n) = stream.read(&mut chunk).await else {
            return;
        };
        if n == 0 {
            return;
        }
        input.extend_from_slice(&chunk[..n]);
        if let Some(end) = input.windows(4).position(|p| p == b"\r\n\r\n") {
            header_end = end + 4;
            break;
        }
        if input.len() > 1024 * 1024 {
            return;
        }
    }
    let headers = String::from_utf8_lossy(&input[..header_end]).into_owned();
    let line = headers.lines().next().unwrap();
    let mut words = line.split_whitespace();
    let method = words.next().unwrap();
    let target = words.next().unwrap();
    let path = target.split('?').next().unwrap();
    let size = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .map_or(0, |(_, v)| v.trim().parse::<usize>().unwrap());
    while input.len() < header_end + size {
        let n = stream.read(&mut chunk).await.unwrap();
        if n == 0 {
            return;
        }
        input.extend_from_slice(&chunk[..n]);
    }
    let mut guard = state.lock().await;
    let mut status = 200;
    if guard.unavailable || (guard.queue_unavailable && path == "/api/v3/queue") {
        drop(guard);
        send_native(&mut stream, 503, b"{}").await;
        return;
    }
    let bytes = match (method, path) {
        ("GET", "/api/v3/system/status") => serde_json::to_vec(&UpStatus {
            version: "fixture-1".into(),
        })
        .unwrap(),
        ("GET", "/api/v3/health") => serde_json::to_vec(&Vec::<UpHealth>::new()).unwrap(),
        ("GET", "/api/v3/qualityprofile") => serde_json::to_vec(&vec![
            UpNamed {
                id: 1,
                name: "HD".into(),
            },
            UpNamed {
                id: 2,
                name: "UHD".into(),
            },
        ])
        .unwrap(),
        ("GET", "/api/v3/rootfolder") => serde_json::to_vec(&vec![
            UpRoot {
                id: 1,
                path: "/media/movies".into(),
                accessible: true,
                free_space: Some(1000000000),
            },
            UpRoot {
                id: 2,
                path: "/media/alternate".into(),
                accessible: true,
                free_space: Some(1000000000),
            },
        ])
        .unwrap(),
        ("GET", "/api/v3/tag") => serde_json::to_vec(&Vec::<UpTag>::new()).unwrap(),
        ("GET", "/api/v3/movie") => {
            serde_json::to_vec(&guard.movie.iter().cloned().collect::<Vec<_>>()).unwrap()
        }
        ("GET", "/api/v3/movie/lookup") => serde_json::to_vec(&vec![native_movie(0, 1)]).unwrap(),
        ("GET", "/api/v3/series") => {
            serde_json::to_vec(&guard.series.iter().cloned().collect::<Vec<_>>()).unwrap()
        }
        ("GET", "/api/v3/series/lookup") => serde_json::to_vec(&vec![native_series(0)]).unwrap(),
        ("GET", "/api/v3/episode") => serde_json::to_vec(&guard.episodes).unwrap(),
        ("PUT", "/api/v3/episode/monitor") => {
            let body: ReceivedMonitoring =
                serde_json::from_slice(&input[header_end..header_end + size]).unwrap();
            assert!(body.monitored);
            let import_on_monitor = guard.import_on_monitor;
            let unmonitor_on_import = guard.unmonitor_on_import;
            for episode in &mut guard.episodes {
                if body.episode_ids.contains(&episode.id) {
                    episode.monitored = true;
                    if import_on_monitor {
                        episode.has_file = true;
                        episode.monitored = !unmonitor_on_import;
                    }
                }
            }
            guard.monitored.push(body.episode_ids);
            b"[]".to_vec()
        }
        ("GET", "/api/v3/queue") => serde_json::to_vec(&EmptyQueue {
            records: vec![],
            total_records: 0,
        })
        .unwrap(),
        ("POST", "/api/v3/movie") => {
            let add: ReceivedAdd =
                serde_json::from_slice(&input[header_end..header_end + size]).unwrap();
            assert_eq!(add.tmdb_id, 14337);
            assert_eq!(add.root_folder_path, "/media/movies");
            assert!(add.monitored);
            assert!(!add.add_options.search_for_movie);
            guard.add_count += 1;
            guard.movie = Some(native_movie(7, add.quality_profile_id));
            if guard.lose_add {
                guard.lose_add = false;
                return;
            }
            status = 201;
            serde_json::to_vec(guard.movie.as_ref().unwrap()).unwrap()
        }
        ("POST", "/api/v3/command") => {
            let body: Command =
                serde_json::from_slice(&input[header_end..header_end + size]).unwrap();
            assert!(
                matches!(&body,Command::MoviesSearch{movie_ids} if movie_ids==&vec![7])
                    || matches!(&body,Command::EpisodeSearch{episode_ids} if episode_ids==&vec![22])
            );
            let queued = UpCommand {
                id: guard.commands.len() as u32 + 1,
                body: Some(body),
                queued: Some(
                    crate::media::requests::utc_timestamp(crate::monitor::unix_ms()).unwrap(),
                ),
            };
            guard.commands.push(queued.clone());
            status = 201;
            serde_json::to_vec(&queued).unwrap()
        }
        ("GET", "/api/v3/command") => serde_json::to_vec(&guard.commands).unwrap(),
        ("GET", path) if path.starts_with("/api/v3/command/") => {
            let id = path.rsplit('/').next().unwrap().parse::<u32>().unwrap();
            serde_json::to_vec(guard.commands.iter().find(|c| c.id == id).unwrap()).unwrap()
        }
        _ => {
            status = 404;
            b"{}".to_vec()
        }
    };
    drop(guard);
    send_native(&mut stream, status, &bytes).await;
}
async fn send_native(stream: &mut TcpStream, status: u16, body: &[u8]) {
    let header = format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes()).await;
    let _ = stream.write_all(body).await;
}
fn native_movie(id: u32, quality: u32) -> UpMovie {
    UpMovie {
        id,
        tmdb_id: 14337,
        title: "Primer".into(),
        title_slug: "14337".into(),
        year: 2004,
        overview: "Fixture".into(),
        images: vec![],
        quality_profile_id: quality,
        root_folder_path: if id > 0 {
            "/media/movies".into()
        } else {
            String::new()
        },
        path: if id > 0 {
            "/media/movies/Primer".into()
        } else {
            String::new()
        },
        monitored: true,
        has_file: false,
        movie_file_id: 0,
        tags: vec![],
    }
}
fn native_series(id: u32) -> UpSeries {
    UpSeries {
        id,
        tvdb_id: 78804,
        title: "Example series".into(),
        title_slug: "78804".into(),
        year: 2005,
        overview: String::new(),
        images: vec![],
        quality_profile_id: 1,
        root_folder_path: "/media/movies".into(),
        path: String::new(),
        monitored: true,
        season_folder: true,
        monitor_new_items: "none".into(),
        series_type: "standard".into(),
        tags: vec![],
        seasons: (0..=3)
            .map(|number| UpSeason {
                season_number: number,
                monitored: false,
                statistics: None,
            })
            .collect(),
    }
}
fn native_episode(id: u32, season: u16, date: &str, has_file: bool) -> UpEpisode {
    UpEpisode {
        id,
        season_number: season,
        episode_number: id as u16,
        title: "Example episode".into(),
        air_date: None,
        air_date_utc: Some(date.into()),
        monitored: false,
        has_file,
    }
}

struct Harness {
    directory: tempfile::TempDir,
    native: Native,
    config: MediaConfig,
    status: watch::Sender<Status>,
    service: Arc<MediaService>,
    api: Option<dropshot::HttpServer<Arc<Context>>>,
    url: String,
    client: reqwest::Client,
    worker: Option<(watch::Sender<bool>, JoinHandle<()>)>,
}
impl Harness {
    async fn new() -> Self {
        let native = Native::start().await;
        let directory = tempfile::tempdir().unwrap();
        let config = MediaConfig {
            radarr: MediaConnection {
                url: native.url.clone(),
                api_key: "fixture-upstream-key".to_owned().into(),
            },
            sonarr: MediaConnection {
                url: native.url.clone(),
                api_key: "fixture-upstream-key".to_owned().into(),
            },
            request_token: REQUESTER.to_owned().into(),
            admin_token: ADMIN.to_owned().into(),
            integration_token: INTEGRATION.to_owned().into(),
            ..MediaConfig::default()
        };
        let (status, rx) = watch::channel(Status::needs_configuration("fixture"));
        let service =
            MediaService::open(config.clone(), directory.path().join("media"), rx, vec![])
                .await
                .unwrap();
        let api = start_api(service.clone(), status.subscribe());
        let url = format!("http://{}", api.local_addr());
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        Self {
            directory,
            native,
            config,
            status,
            service,
            api: Some(api),
            url,
            client,
            worker: None,
        }
    }
    fn start_worker(&mut self) {
        let (tx, rx) = watch::channel(false);
        self.worker = Some((tx, tokio::spawn(self.service.clone().run(rx))));
    }
    async fn stop_worker(&mut self) {
        if let Some((stop, task)) = self.worker.take() {
            let _ = stop.send(true);
            task.await.unwrap();
        }
    }
    async fn restart(&mut self) {
        self.stop_worker().await;
        self.api.take().unwrap().close().await.unwrap();
        self.service = MediaService::open(
            self.config.clone(),
            self.directory.path().join("media"),
            self.status.subscribe(),
            vec![],
        )
        .await
        .unwrap();
        let api = start_api(self.service.clone(), self.status.subscribe());
        self.url = format!("http://{}", api.local_addr());
        self.api = Some(api);
        self.start_worker();
    }
    async fn close(mut self) {
        self.stop_worker().await;
        self.api.take().unwrap().close().await.unwrap();
    }
    async fn profile(&self) -> DownloadProfile {
        let response = self
            .client
            .post(format!("{}/api/v1/profiles", self.url))
            .bearer_auth(ADMIN)
            .header("idempotency-key", "profile-test-1")
            .json(&profile_input(1))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201);
        assert_eq!(response.headers().get("etag").unwrap(), "\"1\"");
        response.json().await.unwrap()
    }
    async fn submit(&self, input: &CreateRequest, key: &str, token: &str) -> reqwest::Response {
        self.client
            .post(format!("{}/api/v1/requests", self.url))
            .bearer_auth(token)
            .header("idempotency-key", key)
            .json(input)
            .send()
            .await
            .unwrap()
    }
    async fn record(&self, id: &RequestId) -> RequestRecord {
        self.client
            .get(format!("{}/api/v1/requests/{id}", self.url))
            .bearer_auth(REQUESTER)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn applied(&self, id: &RequestId) -> RequestRecord {
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let record = self.record(id).await;
                if matches!(record.submission, Submission::Applied { .. }) {
                    return record;
                }
                assert!(
                    !matches!(record.submission, Submission::Failed { .. }),
                    "request failed"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap()
    }
}
fn start_api(
    service: Arc<MediaService>,
    status: watch::Receiver<Status>,
) -> dropshot::HttpServer<Arc<Context>> {
    let context = Context::new(
        Access::Direct {
            token_hash: Context::token_hash(READER),
        },
        status,
    )
    .unwrap()
    .with_media(service);
    api::start(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        Arc::new(context),
    )
    .unwrap()
}
fn profile_input(quality: u32) -> ProfileInput {
    ProfileInput::Movie {
        name: "Movies".parse().unwrap(),
        is_default: true,
        settings: MovieSettings {
            quality_profile_id: QualityProfileId::try_from(quality).unwrap(),
            root_folder_id: RootFolderId::try_from(1).unwrap(),
            tags: vec![],
            minimum_availability: MinimumAvailability::Released,
        },
    }
}
fn movie_request(profile: &DownloadProfile) -> CreateRequest {
    CreateRequest::Movie {
        tmdb_id: TmdbId::try_from(14337).unwrap(),
        profile: MovieProfileRef {
            id: profile.id().clone(),
            revision: profile.revision(),
        },
        intent: MovieIntent::SearchNow {},
    }
}

#[tokio::test]
async fn read_credentials_cannot_mutate_or_delegate_and_integration_cannot_administer() {
    let harness = Harness::new().await;
    for token in [None, Some("invalid-fixture-token")] {
        let mut request = harness.client.get(format!("{}/api/v1/status", harness.url));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        assert_eq!(request.send().await.unwrap().status(), 401);
    }
    let profile = harness.profile().await;
    let input = movie_request(&profile);
    let reader = harness
        .client
        .get(format!("{}/api/v1/system", harness.url))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap()
        .json::<SystemResponse>()
        .await
        .unwrap();
    assert_eq!(reader.credential_role, CredentialRole::ReadOnly);
    assert!(!reader.capabilities.request);
    assert_eq!(
        harness
            .submit(&input, "read-attempt", READER)
            .await
            .status(),
        403
    );
    let forged = harness
        .client
        .get(format!("{}/api/v1/system", harness.url))
        .bearer_auth(REQUESTER)
        .header("x-downloads-actor-id", "someone")
        .header("x-downloads-actor-can-request", "true")
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status(), 403);
    let handshake = harness
        .client
        .get(format!("{}/api/v1/system", harness.url))
        .bearer_auth(INTEGRATION)
        .send()
        .await
        .unwrap()
        .json::<SystemResponse>()
        .await
        .unwrap();
    assert_eq!(handshake.credential_role, CredentialRole::Integration);
    assert!(!handshake.capabilities.request);
    let profile_write = harness
        .client
        .post(format!("{}/api/v1/profiles", harness.url))
        .bearer_auth(INTEGRATION)
        .header("x-downloads-actor-id", "someone")
        .header("x-downloads-actor-can-request", "true")
        .header("idempotency-key", "cannot-admin")
        .json(&profile_input(1))
        .send()
        .await
        .unwrap();
    assert_eq!(profile_write.status(), 403);
    let no_delegate = harness
        .client
        .get(format!("{}/api/v1/profiles", harness.url))
        .bearer_auth(INTEGRATION)
        .send()
        .await
        .unwrap();
    assert_eq!(no_delegate.status(), 403);
    assert_eq!(harness.native.state.lock().await.add_count, 0);
    harness.close().await;
}
#[tokio::test]
async fn idempotency_pins_the_accepted_profile_and_recovers_after_revision_changes() {
    let mut harness = Harness::new().await;
    let profile = harness.profile().await;
    let input = movie_request(&profile);
    let response = harness.submit(&input, "durable-movie", REQUESTER).await;
    assert_eq!(response.status(), 202);
    let record = response.json::<RequestRecord>().await.unwrap();
    let nonmatching = harness
        .client
        .put(format!("{}/api/v1/profiles/{}", harness.url, profile.id()))
        .bearer_auth(ADMIN)
        .header("if-match", "\"01\"")
        .json(&profile_input(2))
        .send()
        .await
        .unwrap();
    assert_eq!(nonmatching.status(), 412);
    let replace = harness
        .client
        .put(format!("{}/api/v1/profiles/{}", harness.url, profile.id()))
        .bearer_auth(ADMIN)
        .header("if-match", "\"1\"")
        .json(&profile_input(2))
        .send()
        .await
        .unwrap();
    assert_eq!(replace.status(), 200);
    let replay = harness.submit(&input, "durable-movie", REQUESTER).await;
    assert_eq!(replay.status(), 202);
    assert_eq!(replay.json::<RequestRecord>().await.unwrap().id, record.id);
    let new_key = harness.submit(&input, "new-intent", REQUESTER).await;
    assert_eq!(new_key.status(), 409);
    let lookup = harness
        .client
        .get(format!("{}/api/v1/request-keys/durable-movie", harness.url))
        .bearer_auth(REQUESTER)
        .send()
        .await
        .unwrap();
    assert_eq!(lookup.json::<RequestRecord>().await.unwrap().id, record.id);
    let foreign = harness
        .client
        .get(format!("{}/api/v1/request-keys/durable-movie", harness.url))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 404);
    harness.start_worker();
    harness.applied(&record.id).await;
    {
        let native = harness.native.state.lock().await;
        assert_eq!(native.add_count, 1);
        assert_eq!(native.movie.as_ref().unwrap().quality_profile_id, 1);
        assert_eq!(native.commands.len(), 1);
    }
    harness.close().await;
}
#[tokio::test]
async fn lost_add_response_survives_restart_without_duplicate_add_or_search() {
    let mut harness = Harness::new().await;
    let profile = harness.profile().await;
    harness.native.state.lock().await.lose_add = true;
    let record = harness
        .submit(&movie_request(&profile), "lost-response", REQUESTER)
        .await
        .json::<RequestRecord>()
        .await
        .unwrap();
    harness.start_worker();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                harness.record(&record.id).await.submission,
                Submission::Reconciling { .. }
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    harness.restart().await;
    harness.applied(&record.id).await;
    {
        let native = harness.native.state.lock().await;
        assert_eq!(native.add_count, 1);
        assert_eq!(native.commands.len(), 1);
    }
    harness.close().await;
}

#[tokio::test]
async fn disconnected_http_caller_recovers_the_original_durable_receipt() {
    let harness = Harness::new().await;
    let profile = harness.profile().await;
    let input = movie_request(&profile);
    let body = serde_json::to_vec(&input).unwrap();
    let mut socket = TcpStream::connect(harness.api.as_ref().unwrap().local_addr())
        .await
        .unwrap();
    let headers = format!(
        "POST /api/v1/requests HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {REQUESTER}\r\nIdempotency-Key: disconnected-caller\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(headers.as_bytes()).await.unwrap();
    socket.write_all(&body).await.unwrap();
    drop(socket); // Deliberately lose the acknowledgement, not the request key.
    let recovered = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = harness
                .client
                .get(format!(
                    "{}/api/v1/request-keys/disconnected-caller",
                    harness.url
                ))
                .bearer_auth(REQUESTER)
                .send()
                .await
                .unwrap();
            if response.status() == 200 {
                break response.json::<RequestRecord>().await.unwrap();
            }
            assert_eq!(response.status(), 404);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let replay = harness
        .submit(&input, "disconnected-caller", REQUESTER)
        .await
        .json::<RequestRecord>()
        .await
        .unwrap();
    assert_eq!(replay.id, recovered.id);
    assert_eq!(harness.service.store.snapshot().await.requests.len(), 1);
    assert_eq!(harness.native.state.lock().await.add_count, 0);
    harness.close().await;
}
#[tokio::test]
async fn existing_quality_is_not_overwritten_and_source_failure_is_not_empty() {
    let harness = Harness::new().await;
    let profile = harness.profile().await;
    harness.native.state.lock().await.movie = Some(native_movie(7, 2));
    let response = harness
        .submit(&movie_request(&profile), "conflict", REQUESTER)
        .await;
    assert_eq!(response.status(), 409);
    let error = response
        .json::<dropshot::HttpErrorResponseBody>()
        .await
        .unwrap();
    assert_eq!(
        error.error_code.as_deref(),
        Some("existing_settings_conflict")
    );
    let before = harness
        .client
        .get(format!("{}/api/v1/movies", harness.url))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap()
        .json::<Page<MediaItem>>()
        .await
        .unwrap();
    assert_eq!(before.items.len(), 1);
    harness.native.state.lock().await.unavailable = true;
    let after = harness
        .client
        .get(format!("{}/api/v1/movies", harness.url))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap()
        .json::<Page<MediaItem>>()
        .await
        .unwrap();
    assert_eq!(after.items.len(), 1);
    assert_eq!(after.coverage[0].state, CoverageState::Unavailable);
    assert_eq!(
        after.coverage[0].observed_at_unix_ms,
        before.coverage[0].observed_at_unix_ms
    );
    assert_eq!(harness.native.state.lock().await.add_count, 0);
    harness.close().await;
}

#[tokio::test]
async fn unavailable_episode_queue_is_unknown_not_zero_or_waiting() {
    let harness = Harness::new().await;
    {
        let mut native = harness.native.state.lock().await;
        native.series = Some(native_series(8));
        native.episodes = vec![
            native_episode(21, 2, "2005-01-01T00:00:00Z", true),
            native_episode(22, 2, "2005-01-01T00:00:00Z", false),
        ];
        native.queue_unavailable = true;
    }
    let response = harness
        .client
        .get(format!("{}/api/v1/series/78804", harness.url))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let item = response.json::<MediaItem>().await.unwrap();
    let MediaItem::Series {
        seasons,
        acquisition,
        ..
    } = item
    else {
        panic!("expected series")
    };
    assert!(matches!(
        acquisition,
        Acquisition::Unavailable {
            component: Component::Sonarr
        }
    ));
    let season = seasons.iter().find(|season| season.number == 2).unwrap();
    assert_eq!(season.imported_count, Some(1));
    assert_eq!(season.active_count, None);
    let episodes = harness
        .client
        .get(format!(
            "{}/api/v1/series/78804/episodes?season=2",
            harness.url
        ))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap()
        .json::<Page<Episode>>()
        .await
        .unwrap();
    assert_eq!(episodes.coverage[0].state, CoverageState::Partial);
    assert!(matches!(
        episodes.items[0].acquisition,
        Acquisition::Imported
    ));
    assert!(matches!(
        episodes.items[1].acquisition,
        Acquisition::Unavailable {
            component: Component::Sonarr
        }
    ));
    harness.close().await;
}

#[tokio::test]
async fn series_request_monitors_and_searches_only_missing_aired_selected_episodes() {
    for (imported_during_monitoring, unmonitor_on_import) in
        [(false, false), (true, false), (true, true)]
    {
        let mut harness = Harness::new().await;
        let profile = harness
            .service
            .create_profile(
                "test-admin",
                "series-profile",
                ProfileInput::Series {
                    name: "TV".parse().unwrap(),
                    is_default: true,
                    settings: SeriesSettings {
                        quality_profile_id: QualityProfileId::try_from(1).unwrap(),
                        root_folder_id: RootFolderId::try_from(1).unwrap(),
                        tags: vec![],
                        series_type: SeriesType::Standard,
                        season_folders: true,
                        future_monitoring: FutureMonitoring::Off,
                    },
                },
            )
            .await
            .unwrap();
        {
            let mut native = harness.native.state.lock().await;
            native.series = Some(native_series(8));
            native.import_on_monitor = imported_during_monitoring;
            native.unmonitor_on_import = unmonitor_on_import;
            native.episodes = vec![
                native_episode(1, 0, "2005-01-01T00:00:00Z", false),
                native_episode(11, 1, "2005-01-01T00:00:00Z", false),
                native_episode(21, 2, "2005-01-01T00:00:00Z", true),
                native_episode(22, 2, "2005-01-01T00:00:00Z", false),
                native_episode(23, 2, "2099-01-01T00:00:00Z", false),
                native_episode(24, 2, "2005-02-30T00:00:00Z", false),
                native_episode(31, 3, "2005-01-01T00:00:00Z", false),
            ];
        }
        let input = CreateRequest::Series {
            tvdb_id: TvdbId::try_from(78804).unwrap(),
            profile: SeriesProfileRef {
                id: profile.id().clone(),
                revision: profile.revision(),
            },
            intent: SeriesIntent::SearchNow {
                scope: EpisodeScope::Seasons {
                    numbers: SeasonSet::try_from(vec![2]).unwrap(),
                },
            },
        };
        let response = harness.submit(&input, "exact-season", REQUESTER).await;
        assert_eq!(response.status(), 202);
        let record = response.json::<RequestRecord>().await.unwrap();
        harness.start_worker();
        harness.applied(&record.id).await;
        {
            let native = harness.native.state.lock().await;
            assert_eq!(native.add_count, 0);
            assert_eq!(native.monitored, vec![vec![22]]);
            if imported_during_monitoring {
                assert!(native.commands.is_empty());
            } else {
                assert_eq!(
                    native.commands[0].body,
                    Some(Command::EpisodeSearch {
                        episode_ids: vec![22]
                    })
                );
            }
            assert!(
                native
                    .episodes
                    .iter()
                    .filter(|e| e.id != 22)
                    .all(|e| !e.monitored)
            );
        }
        harness.close().await;
    }
}

#[tokio::test]
async fn health_readiness_expires_and_optional_services_do_not_block_requests() {
    let harness = Harness::new().await;
    let profile = harness.profile().await;
    let now = crate::monitor::unix_ms();
    *harness.service.health.write().await = vec![ComponentHealth {
        component: Component::Radarr,
        observation: Observation::Observed {
            outcome: HealthOutcome::Healthy,
            freshness: Freshness::Fresh,
            observed_at_unix_ms: now,
            expires_at_unix_ms: now + 30_000,
        },
    }];
    harness.service.cache.write().await.roots.insert(
        profile.id().clone(),
        crate::media::service::RootObservation {
            root: RootFolderId::try_from(1).unwrap(),
            accessible: true,
            observed_at_unix_ms: now,
        },
    );
    let snapshot = harness
        .service
        .health_snapshot(Some(Operation::RequestMovie), Some(profile.id()))
        .await
        .unwrap();
    assert_eq!(snapshot.readiness[0].state, ReadinessState::Ready);
    // Import additionally needs current staging evidence, but a request does not
    // claim that the transfer/import stack is ready or require optional Plex.
    let import = harness
        .service
        .health_snapshot(Some(Operation::ImportMovie), Some(profile.id()))
        .await
        .unwrap();
    assert_eq!(import.readiness[0].state, ReadinessState::Blocked);
    harness.service.cache.write().await.download_storage = Some((true, now));
    assert_eq!(
        harness
            .service
            .health_snapshot(Some(Operation::ImportMovie), Some(profile.id()))
            .await
            .unwrap()
            .readiness[0]
            .state,
        ReadinessState::Ready
    );
    if let Observation::Observed {
        expires_at_unix_ms, ..
    } = &mut harness.service.health.write().await[0].observation
    {
        *expires_at_unix_ms = now - 1;
    }
    let stale = harness
        .service
        .health_snapshot(Some(Operation::RequestMovie), Some(profile.id()))
        .await
        .unwrap();
    assert_eq!(stale.readiness[0].state, ReadinessState::Blocked);
    assert!(matches!(
        stale.components[0].observation,
        Observation::Observed {
            freshness: Freshness::Stale,
            ..
        }
    ));
    harness.service.stop_accepting();
    assert_eq!(
        harness
            .submit(&movie_request(&profile), "shutdown", REQUESTER)
            .await
            .status(),
        503
    );
    assert!(harness.service.store.snapshot().await.requests.is_empty());
    harness.close().await;
}

#[tokio::test]
async fn changing_a_saved_root_cannot_reuse_the_previous_roots_health_proof() {
    let mut harness = Harness::new().await;
    let profile = harness.profile().await;
    let now = crate::monitor::unix_ms();
    *harness.service.health.write().await = vec![ComponentHealth {
        component: Component::Radarr,
        observation: Observation::Observed {
            outcome: HealthOutcome::Healthy,
            freshness: Freshness::Fresh,
            observed_at_unix_ms: now,
            expires_at_unix_ms: now + 30_000,
        },
    }];
    harness.service.cache.write().await.roots.insert(
        profile.id().clone(),
        crate::media::service::RootObservation {
            root: RootFolderId::try_from(1).unwrap(),
            accessible: true,
            observed_at_unix_ms: now,
        },
    );
    assert_eq!(
        harness
            .service
            .health_snapshot(Some(Operation::RequestMovie), Some(profile.id()))
            .await
            .unwrap()
            .readiness[0]
            .state,
        ReadinessState::Ready
    );
    let mut changed = profile_input(1);
    let ProfileInput::Movie { settings, .. } = &mut changed else {
        unreachable!()
    };
    settings.root_folder_id = RootFolderId::try_from(2).unwrap();
    let response = harness
        .client
        .put(format!("{}/api/v1/profiles/{}", harness.url, profile.id()))
        .bearer_auth(ADMIN)
        .header("if-match", "\"1\"")
        .json(&changed)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let snapshot = harness
        .service
        .health_snapshot(Some(Operation::RequestMovie), Some(profile.id()))
        .await
        .unwrap();
    assert_eq!(
        snapshot.readiness[0].state,
        ReadinessState::Blocked,
        "old library proof must not validate a different saved root"
    );
    harness.start_worker();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if harness
                .service
                .health_snapshot(Some(Operation::RequestMovie), Some(profile.id()))
                .await
                .unwrap()
                .readiness[0]
                .state
                == ReadinessState::Ready
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(harness.native.state.lock().await.add_count, 0);
    harness.close().await;
}

#[tokio::test]
#[ignore = "requires HA_DOWNLOADS_TEST_PYTHON pointing to the HA test environment"]
async fn real_home_assistant_bridge_uses_live_rust_contract() {
    let python = std::env::var("HA_DOWNLOADS_TEST_PYTHON").expect("HA test Python is required");
    let mut harness = Harness::new().await;
    let profile = harness.profile().await;
    harness.start_worker();
    let status = tokio::process::Command::new(python)
        .args([
            "-m",
            "pytest",
            "-q",
            "-c",
            "integration/pyproject.toml",
            "tests_ha/test_rust_contract.py",
        ])
        .env("DOWNLOADS_INTEROP_URL", &harness.url)
        .env("DOWNLOADS_INTEROP_TOKEN", INTEGRATION)
        .env("DOWNLOADS_INTEROP_PROFILE", profile.id().to_string())
        .status()
        .await
        .unwrap();
    assert!(status.success());
    harness.close().await;
}
