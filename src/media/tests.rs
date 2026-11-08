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
    health: Vec<NativeHealthCheck>,
    quality_profiles: Vec<UpNamed>,
    roots: Vec<UpRoot>,
    calls: Vec<(String, String)>,
    movie: Option<UpMovie>,
    series: Option<UpSeries>,
    episodes: Vec<UpEpisode>,
    monitored: Vec<Vec<u32>>,
    add_count: u32,
    commands: Vec<UpCommand>,
    lose_add: bool,
    unavailable: bool,
    library_unavailable: bool,
    queue_unavailable: bool,
    movie_queue: Vec<UpQueue>,
    series_queue: Vec<UpQueue>,
    prowlarr_health: Vec<UpHealth>,
    supervisor_update: Option<bool>,
    supervisor_unrelated: bool,
    native_port: u16,
    movie_imports: Vec<crate::media::imports::NativeImport>,
    series_imports: Vec<crate::media::imports::NativeImport>,
    movie_import_total: Option<u32>,
    series_import_total: Option<u32>,
    history_unavailable: bool,
    queries: Vec<(String, String)>,
    plex_status: u16,
    plex_requires_key: bool,
    plex_metadata: Vec<PlexFixtureMetadata>,
    import_on_monitor: bool,
    unmonitor_on_import: bool,
}
#[derive(Clone, Serialize)]
struct NativeHealthCheck {
    source: String,
    #[serde(rename = "type")]
    severity: String,
    message: String,
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
        let state = Arc::new(Mutex::new(NativeState {
            native_port: listener.local_addr().unwrap().port(),
            quality_profiles: vec![UpNamed {
                id: 1,
                name: "Native quality".into(),
            }],
            roots: vec![UpRoot {
                id: 1,
                path: "/media/movies".into(),
                accessible: true,
                free_space: Some(1_000_000_000),
            }],
            ..NativeState::default()
        }));
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
    minimum_availability: String,
    tags: Vec<u32>,
    add_options: ReceivedAddOptions,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceivedAddOptions {
    search_for_movie: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceivedSeriesAdd {
    tvdb_id: u32,
    quality_profile_id: u32,
    root_folder_path: String,
    monitored: bool,
    season_folder: bool,
    series_type: String,
    monitor_new_items: String,
    seasons: Vec<UpSeason>,
    tags: Vec<u32>,
    add_options: ReceivedSeriesAddOptions,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceivedSeriesAddOptions {
    monitor: String,
    search_for_missing_episodes: bool,
    search_for_cutoff_unmet_episodes: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceivedMonitoring {
    episode_ids: Vec<u32>,
    monitored: bool,
}
#[derive(Serialize)]
struct EmptyQueue {
    records: Vec<UpQueue>,
    #[serde(rename = "totalRecords")]
    total_records: u32,
}
#[derive(Serialize)]
struct SupervisorEnvelope<T> {
    result: &'static str,
    data: T,
}
#[derive(Serialize)]
struct SupervisorInventory {
    addons: Vec<SupervisorSummary>,
}
#[derive(Serialize)]
struct SupervisorSummary {
    slug: &'static str,
    name: &'static str,
}
#[derive(Serialize)]
struct SupervisorApp {
    hostname: &'static str,
    ip_address: &'static str,
    network: std::collections::BTreeMap<&'static str, Option<u16>>,
    host_network: bool,
    version: &'static str,
    version_latest: &'static str,
    update_available: bool,
}
#[derive(Serialize)]
struct SupervisorCore {
    ip_address: &'static str,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryFixture {
    records: Vec<crate::media::imports::NativeImport>,
    total_records: u32,
}
#[derive(Serialize)]
struct PlexFixtureEnvelope<T> {
    #[serde(rename = "MediaContainer")]
    container: T,
}
#[derive(Clone, Serialize)]
struct PlexFixtureContainer {
    #[serde(rename = "Metadata")]
    metadata: Vec<PlexFixtureMetadata>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlexFixtureMetadata {
    rating_key: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "Guid")]
    guids: Vec<PlexFixtureGuid>,
    updated_at: u64,
    parent_index: Option<u16>,
    index: Option<u16>,
    #[serde(rename = "Media")]
    media: Vec<PlexFixtureMedia>,
}
#[derive(Clone, Serialize)]
struct PlexFixtureGuid {
    id: String,
}
#[derive(Clone, Serialize)]
struct PlexFixtureMedia {
    #[serde(rename = "Part")]
    parts: Vec<PlexFixturePart>,
}
#[derive(Clone, Serialize)]
struct PlexFixturePart {
    file: String,
    size: u64,
    exists: Option<bool>,
    accessible: Option<bool>,
    #[serde(rename = "Stream")]
    streams: Option<Vec<PlexFixtureStream>>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlexFixtureStream {
    stream_type: u8,
    language_code: Option<String>,
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
    guard.calls.push((method.into(), path.into()));
    if let Some((_, query)) = target.split_once('?') {
        guard.queries.push((path.into(), query.into()));
    }
    let mut status = 200;
    if guard.unavailable
        || (guard.library_unavailable && matches!(path, "/api/v3/movie" | "/api/v3/series"))
        || (guard.queue_unavailable && matches!(path, "/api/v3/queue" | "/api/v3/queue/details"))
        || (guard.history_unavailable && path == "/api/v3/history")
    {
        drop(guard);
        send_native(&mut stream, 503, b"{}").await;
        return;
    }
    if (path.starts_with("/supervisor/") || path.starts_with("/addons/") || path == "/core/info")
        && guard.supervisor_update.is_none()
    {
        drop(guard);
        send_native(&mut stream, 503, b"{}").await;
        return;
    }
    if path.starts_with("/library/")
        && (guard.plex_status != 200
            || (guard.plex_requires_key
                && !headers
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("x-plex-token: fixture-plex-key"))))
    {
        let status = if guard.plex_status != 200 && guard.plex_status != 0 {
            guard.plex_status
        } else {
            401
        };
        drop(guard);
        send_native(&mut stream, status, b"{}").await;
        return;
    }
    let bytes = match (method, path) {
        ("GET", "/api/v3/system/status" | "/api/v1/system/status") => {
            serde_json::to_vec(&UpStatus {
                version: "fixture-1".into(),
            })
            .unwrap()
        }
        ("GET", "/api/v3/health") => serde_json::to_vec(&guard.health).unwrap(),
        ("GET", "/api/v3/history") => {
            let sonarr = headers
                .lines()
                .any(|line| line.eq_ignore_ascii_case("x-api-key: fixture-sonarr-key"));
            let (records, total) = if sonarr {
                (&guard.series_imports, guard.series_import_total)
            } else {
                (&guard.movie_imports, guard.movie_import_total)
            };
            serde_json::to_vec(&HistoryFixture {
                records: records.clone(),
                total_records: total.unwrap_or(records.len() as u32),
            })
            .unwrap()
        }
        ("GET", "/library/sections") => b"{\"MediaContainer\":{}}".to_vec(),
        ("GET", "/library/all") => serde_json::to_vec(&PlexFixtureEnvelope {
            container: PlexFixtureContainer {
                metadata: guard.plex_metadata.clone(),
            },
        })
        .unwrap(),
        ("GET", path) if path.starts_with("/library/metadata/") => {
            let key = path.trim_start_matches("/library/metadata/");
            serde_json::to_vec(&PlexFixtureEnvelope {
                container: PlexFixtureContainer {
                    metadata: guard
                        .plex_metadata
                        .iter()
                        .filter(|metadata| metadata.rating_key == key)
                        .cloned()
                        .collect(),
                },
            })
            .unwrap()
        }
        ("GET", "/api/v1/health") => {
            if guard.prowlarr_health.is_empty() {
                serde_json::to_vec(&guard.health).unwrap()
            } else {
                serde_json::to_vec(&guard.prowlarr_health).unwrap()
            }
        }
        ("GET", "/supervisor/info") => serde_json::to_vec(&SupervisorEnvelope {
            result: "ok",
            data: SupervisorInventory {
                addons: vec![SupervisorSummary {
                    slug: "fixture_prowlarr",
                    name: "Prowlarr",
                }],
            },
        })
        .unwrap(),
        ("GET", "/core/info") => serde_json::to_vec(&SupervisorEnvelope {
            result: "ok",
            data: SupervisorCore {
                ip_address: if guard.supervisor_unrelated {
                    "192.0.2.10"
                } else {
                    "127.0.0.1"
                },
            },
        })
        .unwrap(),
        ("GET", "/addons/fixture_prowlarr/info") => serde_json::to_vec(&SupervisorEnvelope {
            result: "ok",
            data: SupervisorApp {
                hostname: "fixture-prowlarr",
                ip_address: "192.0.2.20",
                network: std::collections::BTreeMap::from([("9696/tcp", Some(guard.native_port))]),
                host_network: false,
                version: "1.0",
                version_latest: if guard.supervisor_update == Some(true) {
                    "2.0"
                } else {
                    "1.0"
                },
                update_available: guard.supervisor_update == Some(true),
            },
        })
        .unwrap(),
        ("GET", "/api/v3/qualityprofile") => serde_json::to_vec(&guard.quality_profiles).unwrap(),
        ("GET", "/api/v3/rootfolder") => serde_json::to_vec(&guard.roots).unwrap(),
        ("GET", "/api/v3/tag") => serde_json::to_vec(&Vec::<UpTag>::new()).unwrap(),
        ("GET", "/api/v3/movie") => {
            serde_json::to_vec(&guard.movie.iter().cloned().collect::<Vec<_>>()).unwrap()
        }
        ("GET", "/api/v3/movie/lookup") => {
            let mut movie = native_movie(0, 1);
            movie.minimum_availability = "tba".into();
            serde_json::to_vec(&vec![movie]).unwrap()
        }
        ("GET", "/api/v3/series") => {
            serde_json::to_vec(&guard.series.iter().cloned().collect::<Vec<_>>()).unwrap()
        }
        ("GET", "/api/v3/series/lookup") => {
            let mut series = native_series(0);
            series.season_folder = false;
            serde_json::to_vec(&vec![series]).unwrap()
        }
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
        ("GET", "/api/v3/queue") => {
            let records = if headers
                .lines()
                .any(|line| line.eq_ignore_ascii_case("x-api-key: fixture-sonarr-key"))
            {
                guard.series_queue.clone()
            } else {
                guard.movie_queue.clone()
            };
            serde_json::to_vec(&EmptyQueue {
                total_records: records.len() as u32,
                records,
            })
            .unwrap()
        }
        ("GET", "/api/v3/queue/details") => {
            let records = if headers
                .lines()
                .any(|line| line.eq_ignore_ascii_case("x-api-key: fixture-sonarr-key"))
            {
                &guard.series_queue
            } else {
                &guard.movie_queue
            };
            serde_json::to_vec(records).unwrap()
        }
        ("POST", "/api/v3/movie") => {
            let add: ReceivedAdd =
                serde_json::from_slice(&input[header_end..header_end + size]).unwrap();
            assert_eq!(add.tmdb_id, 14337);
            assert!(add.monitored);
            assert!(!add.add_options.search_for_movie);
            guard.add_count += 1;
            let mut movie = native_movie(7, add.quality_profile_id);
            movie.root_folder_path = add.root_folder_path;
            movie.path = format!("{}/Primer", movie.root_folder_path);
            movie.minimum_availability = add.minimum_availability;
            movie.tags = add.tags;
            guard.movie = Some(movie);
            if guard.lose_add {
                guard.lose_add = false;
                return;
            }
            status = 201;
            serde_json::to_vec(guard.movie.as_ref().unwrap()).unwrap()
        }
        ("POST", "/api/v3/series") => {
            let add: ReceivedSeriesAdd =
                serde_json::from_slice(&input[header_end..header_end + size]).unwrap();
            assert_eq!(add.tvdb_id, 78804);
            assert!(add.monitored);
            assert_eq!(add.add_options.monitor, "skip");
            assert!(!add.add_options.search_for_missing_episodes);
            assert!(!add.add_options.search_for_cutoff_unmet_episodes);
            guard.add_count += 1;
            let mut series = native_series(8);
            series.quality_profile_id = add.quality_profile_id;
            series.root_folder_path = add.root_folder_path;
            series.path = format!("{}/Example", series.root_folder_path);
            series.season_folder = add.season_folder;
            series.series_type = add.series_type;
            series.monitor_new_items = add.monitor_new_items;
            series.seasons = add.seasons;
            series.tags = add.tags;
            guard.series = Some(series);
            status = 201;
            serde_json::to_vec(guard.series.as_ref().unwrap()).unwrap()
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
        minimum_availability: "released".into(),
        monitored: true,
        has_file: false,
        movie_file_id: 0,
        tags: vec![],
        metadata: UpMovieMetadata::default(),
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
        metadata: UpSeriesMetadata::default(),
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
        Self::with_supervisor(false).await
    }
    async fn with_supervisor(supervised: bool) -> Self {
        Self::with_options(supervised, None).await
    }
    async fn with_plex(key: bool) -> Self {
        Self::with_options(false, Some(key)).await
    }
    async fn with_options(supervised: bool, plex: Option<bool>) -> Self {
        let native = Native::start().await;
        let directory = tempfile::tempdir().unwrap();
        let config = MediaConfig {
            radarr: MediaConnection {
                url: native.url.clone(),
                api_key: "fixture-upstream-key".to_owned().into(),
            },
            sonarr: MediaConnection {
                url: native.url.clone(),
                api_key: "fixture-sonarr-key".to_owned().into(),
            },
            prowlarr: MediaConnection {
                url: native.url.clone(),
                api_key: "fixture-upstream-key".to_owned().into(),
            },
            plex: match plex {
                Some(key) => MediaConnection {
                    url: native.url.clone(),
                    api_key: if key { "fixture-plex-key" } else { "" }.to_owned().into(),
                },
                None => MediaConnection::default(),
            },
            request_token: REQUESTER.to_owned().into(),
            admin_token: ADMIN.to_owned().into(),
            integration_token: INTEGRATION.to_owned().into(),
            ..MediaConfig::default()
        };
        let (status, rx) = watch::channel(Status::needs_configuration("fixture"));
        let mut service =
            MediaService::open(config.clone(), directory.path().join("media"), rx, vec![])
                .await
                .unwrap();
        if supervised {
            Arc::get_mut(&mut service).unwrap().updates =
                crate::media::supervisor::UpdateSource::for_test(&format!("{}/", native.url));
        }
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
    async fn bind_native_defaults(&self, quality: u32, root: u32) {
        self.service
            .store
            .update(|state| {
                state.defaults.movie = Some(crate::media::store::NativeBinding {
                    quality_profile_id: quality.try_into().unwrap(),
                    root_folder_id: root.try_into().unwrap(),
                });
                state.defaults.series = Some(crate::media::store::NativeBinding {
                    quality_profile_id: quality.try_into().unwrap(),
                    root_folder_id: root.try_into().unwrap(),
                });
                Ok(())
            })
            .await
            .unwrap();
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
                    "request failed: {:?}",
                    record.submission
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
fn movie_request() -> CreateRequest {
    CreateRequest::Movie {
        tmdb_id: TmdbId::try_from(14337).unwrap(),
        intent: MovieIntent::SearchNow {},
    }
}
fn alternate_root() -> UpRoot {
    UpRoot {
        id: 2,
        path: "/media/alternate".into(),
        accessible: true,
        free_space: Some(1_000_000_000),
    }
}

#[derive(Serialize)]
struct RetiredProfile {
    id: &'static str,
    revision: u64,
}
#[derive(Serialize)]
struct RequestWithRetiredProfile {
    kind: MediaKind,
    tmdb_id: TmdbId,
    profile: RetiredProfile,
    intent: MovieIntent,
}

#[tokio::test]
async fn removed_settings_routes_are_not_available_to_any_role() {
    let harness = Harness::new().await;
    for token in [READER, REQUESTER, ADMIN, INTEGRATION] {
        for path in [
            "profiles",
            "profiles/276319ec-c797-460c-a49e-6b90fc1f6ae8",
            "managers/radarr/options",
            "managers/sonarr/options",
        ] {
            for method in [
                reqwest::Method::GET,
                reqwest::Method::POST,
                reqwest::Method::PUT,
            ] {
                let mut request = harness
                    .client
                    .request(method.clone(), format!("{}/api/v1/{path}", harness.url))
                    .bearer_auth(token);
                if token == INTEGRATION {
                    request = request
                        .header("x-downloads-actor-id", "fixture-user")
                        .header("x-downloads-actor-can-request", "true");
                }
                let response = request.send().await.unwrap();
                assert_eq!(response.status(), 404, "removed {method} {path}");
            }
        }
    }
    assert!(harness.native.state.lock().await.calls.is_empty());
    assert!(harness.service.store.snapshot().await.requests.is_empty());
    harness.close().await;
}

#[tokio::test]
async fn retired_profile_input_and_health_target_are_rejected_without_native_io() {
    let harness = Harness::new().await;
    let input = RequestWithRetiredProfile {
        kind: MediaKind::Movie,
        tmdb_id: TmdbId::try_from(14337).unwrap(),
        profile: RetiredProfile {
            id: "276319ec-c797-460c-a49e-6b90fc1f6ae8",
            revision: 1,
        },
        intent: MovieIntent::SearchNow {},
    };
    let response = harness
        .client
        .post(format!("{}/api/v1/requests", harness.url))
        .bearer_auth(REQUESTER)
        .header("idempotency-key", "retired-profile")
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let response = harness
        .client
        .get(format!(
            "{}/api/v1/health/stack?profile_id=276319ec-c797-460c-a49e-6b90fc1f6ae8",
            harness.url
        ))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert!(harness.service.store.snapshot().await.requests.is_empty());
    assert!(harness.native.state.lock().await.calls.is_empty());
    harness.close().await;
}

#[tokio::test]
async fn ambiguous_new_titles_require_parent_setup_without_receipts_or_native_writes() {
    for extra_quality in [true, false] {
        let harness = Harness::new().await;
        {
            let mut native = harness.native.state.lock().await;
            if extra_quality {
                native.quality_profiles.extend((2..=6).map(|id| UpNamed {
                    id,
                    name: format!("Native quality {id}"),
                }));
            } else {
                native.roots.push(alternate_root());
            }
        }
        let candidates = harness
            .client
            .get(format!("{}/api/v1/search?query=Example", harness.url))
            .bearer_auth(READER)
            .send()
            .await
            .unwrap()
            .json::<Page<MediaItem>>()
            .await
            .unwrap();
        assert_eq!(candidates.items.len(), 2);
        for item in candidates.items {
            let request = match item {
                MediaItem::Movie { request, .. } | MediaItem::Series { request, .. } => request,
            };
            assert!(matches!(request, RequestAvailability::ParentSetupRequired));
        }
        for input in [
            movie_request(),
            CreateRequest::Series {
                tvdb_id: TvdbId::try_from(78804).unwrap(),
                intent: SeriesIntent::SearchNow {
                    scope: EpisodeScope::AllAired {},
                },
            },
        ] {
            let response = harness
                .submit(&input, "needs-native-setup", REQUESTER)
                .await;
            assert_eq!(response.status(), 409);
            assert_eq!(
                response
                    .json::<dropshot::HttpErrorResponseBody>()
                    .await
                    .unwrap()
                    .error_code
                    .as_deref(),
                Some("native_settings_required")
            );
        }
        assert!(harness.service.store.snapshot().await.requests.is_empty());
        assert!(
            harness
                .native
                .state
                .lock()
                .await
                .calls
                .iter()
                .all(|(method, _)| method == "GET")
        );
        harness.close().await;
    }
}

#[tokio::test]
async fn new_movie_uses_native_add_defaults_and_native_choice_ids() {
    let mut harness = Harness::new().await;
    {
        let mut native = harness.native.state.lock().await;
        native.quality_profiles[0].id = 17;
        native.roots[0] = UpRoot {
            id: 23,
            ..alternate_root()
        };
    }
    let response = harness
        .submit(&movie_request(), "only-native-choice", REQUESTER)
        .await;
    assert_eq!(response.status(), 202);
    let record = response.json::<RequestRecord>().await.unwrap();
    harness.start_worker();
    harness.applied(&record.id).await;
    {
        let native = harness.native.state.lock().await;
        let movie = native.movie.as_ref().unwrap();
        assert_eq!(movie.quality_profile_id, 17);
        assert_eq!(movie.root_folder_path, "/media/alternate");
        assert_eq!(movie.minimum_availability, "released");
        assert_eq!(native.add_count, 1);
        assert_eq!(native.commands.len(), 1);
    }
    harness.close().await;
}

#[tokio::test]
async fn configured_native_bindings_keep_new_movie_and_tv_requests_ready_with_multiple_choices() {
    let mut harness = Harness::new().await;
    {
        let mut native = harness.native.state.lock().await;
        native.quality_profiles.extend((2..=6).map(|id| UpNamed {
            id,
            name: format!("Native quality {id}"),
        }));
        native.roots.push(alternate_root());
        native.episodes = vec![native_episode(22, 2, "2005-01-01T00:00:00Z", false)];
    }
    harness.bind_native_defaults(1, 1).await;
    let candidates = harness
        .client
        .get(format!("{}/api/v1/search?query=Example", harness.url))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap()
        .json::<Page<MediaItem>>()
        .await
        .unwrap();
    assert_eq!(candidates.items.len(), 2);
    for item in candidates.items {
        let request = match item {
            MediaItem::Movie { request, .. } | MediaItem::Series { request, .. } => request,
        };
        assert!(matches!(request, RequestAvailability::Ready));
    }
    let movie = harness
        .submit(&movie_request(), "configured-movie", REQUESTER)
        .await;
    assert_eq!(movie.status(), 202);
    let movie = movie.json::<RequestRecord>().await.unwrap();
    let input = CreateRequest::Series {
        tvdb_id: TvdbId::try_from(78804).unwrap(),
        intent: SeriesIntent::SearchNow {
            scope: EpisodeScope::Seasons {
                numbers: SeasonSet::try_from(vec![2]).unwrap(),
            },
        },
    };
    let series = harness.submit(&input, "configured-tv", REQUESTER).await;
    assert_eq!(series.status(), 202);
    let series = series.json::<RequestRecord>().await.unwrap();
    harness.start_worker();
    harness.applied(&movie.id).await;
    harness.applied(&series.id).await;
    {
        let native = harness.native.state.lock().await;
        assert_eq!(native.add_count, 2);
        assert_eq!(native.movie.as_ref().unwrap().quality_profile_id, 1);
        assert_eq!(native.series.as_ref().unwrap().quality_profile_id, 1);
        assert_eq!(
            native.movie.as_ref().unwrap().root_folder_path,
            "/media/movies"
        );
        assert_eq!(
            native.series.as_ref().unwrap().root_folder_path,
            "/media/movies"
        );
        assert_eq!(native.commands.len(), 2);
        assert!(native.calls.iter().all(|(method, path)| method == "GET"
            || matches!(
                path.as_str(),
                "/api/v3/movie" | "/api/v3/series" | "/api/v3/command" | "/api/v3/episode/monitor"
            )));
    }
    harness.close().await;
}

#[tokio::test]
async fn configured_binding_is_validated_against_fresh_native_choices_without_fallback() {
    for missing_quality in [true, false] {
        let harness = Harness::new().await;
        harness.bind_native_defaults(1, 1).await;
        {
            let mut native = harness.native.state.lock().await;
            if missing_quality {
                native.quality_profiles[0].id = 2;
            } else {
                native.roots[0].id = 2;
            }
        }
        let response = harness
            .submit(&movie_request(), "stale-binding", REQUESTER)
            .await;
        assert_eq!(response.status(), 409);
        assert_eq!(
            response
                .json::<dropshot::HttpErrorResponseBody>()
                .await
                .unwrap()
                .error_code
                .as_deref(),
            Some("native_settings_required")
        );
        assert!(harness.service.store.snapshot().await.requests.is_empty());
        assert!(
            harness
                .native
                .state
                .lock()
                .await
                .calls
                .iter()
                .all(|(method, _)| method == "GET")
        );
        harness.close().await;
    }
}

#[tokio::test]
async fn new_series_uses_native_choices_and_searches_only_requested_aired_scope() {
    let mut harness = Harness::new().await;
    {
        let mut native = harness.native.state.lock().await;
        native.quality_profiles[0].id = 17;
        native.roots[0] = UpRoot {
            id: 23,
            ..alternate_root()
        };
        native.episodes = vec![
            native_episode(11, 1, "2005-01-01T00:00:00Z", false),
            native_episode(22, 2, "2005-01-01T00:00:00Z", false),
            native_episode(23, 2, "2099-01-01T00:00:00Z", false),
        ];
    }
    let input = CreateRequest::Series {
        tvdb_id: TvdbId::try_from(78804).unwrap(),
        intent: SeriesIntent::SearchNow {
            scope: EpisodeScope::Seasons {
                numbers: SeasonSet::try_from(vec![2]).unwrap(),
            },
        },
    };
    let response = harness.submit(&input, "native-new-series", REQUESTER).await;
    assert_eq!(response.status(), 202);
    let record = response.json::<RequestRecord>().await.unwrap();
    harness.start_worker();
    harness.applied(&record.id).await;
    {
        let native = harness.native.state.lock().await;
        let series = native.series.as_ref().unwrap();
        assert_eq!(series.quality_profile_id, 17);
        assert_eq!(series.root_folder_path, "/media/alternate");
        assert!(
            series.season_folder,
            "new series must use Sonarr Add's season folders default"
        );
        assert_eq!(series.monitor_new_items, "none");
        assert_eq!(native.add_count, 1);
        assert_eq!(native.monitored, vec![vec![22]]);
        assert_eq!(native.commands.len(), 1);
        assert_eq!(
            native.commands[0].body,
            Some(Command::EpisodeSearch {
                episode_ids: vec![22]
            })
        );
    }
    harness.close().await;
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
    let input = movie_request();
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
    let delegated = harness
        .client
        .get(format!("{}/api/v1/system", harness.url))
        .bearer_auth(INTEGRATION)
        .header("x-downloads-actor-id", "someone")
        .header("x-downloads-actor-can-request", "true")
        .send()
        .await
        .unwrap()
        .json::<SystemResponse>()
        .await
        .unwrap();
    assert_eq!(delegated.credential_role, CredentialRole::Integration);
    assert!(delegated.capabilities.request);
    assert!(!delegated.capabilities.admin);
    let no_delegate = harness
        .client
        .get(format!("{}/api/v1/requests", harness.url))
        .bearer_auth(INTEGRATION)
        .send()
        .await
        .unwrap();
    assert_eq!(no_delegate.status(), 403);
    assert_eq!(harness.native.state.lock().await.add_count, 0);
    harness.close().await;
}
#[tokio::test]
async fn idempotency_pins_native_settings_when_new_choices_become_ambiguous() {
    let mut harness = Harness::new().await;
    let input = movie_request();
    let response = harness.submit(&input, "durable-movie", REQUESTER).await;
    assert_eq!(response.status(), 202);
    let record = response.json::<RequestRecord>().await.unwrap();
    {
        let mut native = harness.native.state.lock().await;
        native.quality_profiles.push(UpNamed {
            id: 2,
            name: "Another quality".into(),
        });
        native.roots.push(alternate_root());
    }
    let replay = harness.submit(&input, "durable-movie", REQUESTER).await;
    assert_eq!(replay.status(), 202);
    assert_eq!(replay.json::<RequestRecord>().await.unwrap().id, record.id);
    let new_key = harness.submit(&input, "new-intent", REQUESTER).await;
    assert_eq!(new_key.status(), 409);
    assert_eq!(
        new_key
            .json::<dropshot::HttpErrorResponseBody>()
            .await
            .unwrap()
            .error_code
            .as_deref(),
        Some("native_settings_required")
    );
    let different = CreateRequest::Movie {
        tmdb_id: TmdbId::try_from(14337).unwrap(),
        intent: MovieIntent::MonitorOnly {},
    };
    let mismatch = harness.submit(&different, "durable-movie", REQUESTER).await;
    assert_eq!(mismatch.status(), 409);
    assert_eq!(
        mismatch
            .json::<dropshot::HttpErrorResponseBody>()
            .await
            .unwrap()
            .error_code
            .as_deref(),
        Some("idempotency_conflict")
    );
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
    harness.restart().await;
    harness.applied(&record.id).await;
    {
        let native = harness.native.state.lock().await;
        assert_eq!(native.add_count, 1);
        let movie = native.movie.as_ref().unwrap();
        assert_eq!(movie.quality_profile_id, 1);
        assert_eq!(movie.root_folder_path, "/media/movies");
        assert_eq!(native.commands.len(), 1);
    }
    assert_eq!(harness.service.store.snapshot().await.requests.len(), 1);
    harness.close().await;
}

#[tokio::test]
async fn lost_add_response_survives_restart_without_duplicate_add_or_search() {
    let mut harness = Harness::new().await;
    harness.native.state.lock().await.lose_add = true;
    let record = harness
        .submit(&movie_request(), "lost-response", REQUESTER)
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
    let input = movie_request();
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
async fn existing_native_settings_are_preserved_and_source_failure_is_not_empty() {
    let mut harness = Harness::new().await;
    let mut movie = native_movie(7, 2);
    movie.root_folder_path = "/media/alternate".into();
    movie.path = "/media/alternate/Primer".into();
    movie.tags = vec![7, 9];
    movie.minimum_availability = "announced".into();
    movie.monitored = false;
    let original = serde_json::to_vec(&movie).unwrap();
    {
        let mut native = harness.native.state.lock().await;
        native.movie = Some(movie);
        native.quality_profiles.push(UpNamed {
            id: 2,
            name: "Native custom quality".into(),
        });
        native.roots.push(alternate_root());
    }
    // A binding for new titles must never override an existing title's policy.
    harness.bind_native_defaults(1, 1).await;
    let response = harness
        .submit(&movie_request(), "parent-owned", REQUESTER)
        .await;
    assert_eq!(response.status(), 202);
    let record = response.json::<RequestRecord>().await.unwrap();
    let accepted = harness.service.store.snapshot().await;
    let crate::media::store::Plan::Movie { settings, root, .. } = &accepted.requests[0].plan else {
        panic!("expected movie plan")
    };
    assert_eq!(settings.quality_profile_id.get(), 2);
    assert_eq!(settings.root_folder_id.get(), 2);
    assert_eq!(
        settings
            .tags
            .iter()
            .map(|tag| tag.get())
            .collect::<Vec<_>>(),
        vec![7, 9]
    );
    assert_eq!(
        settings.minimum_availability,
        MinimumAvailability::Announced
    );
    assert_eq!(root, "/media/alternate");
    harness.start_worker();
    harness.applied(&record.id).await;
    harness.stop_worker().await;
    {
        let native = harness.native.state.lock().await;
        assert_eq!(
            serde_json::to_vec(native.movie.as_ref().unwrap()).unwrap(),
            original
        );
        assert_eq!(native.add_count, 0);
        assert!(
            !native
                .calls
                .iter()
                .any(|(method, path)| method != "GET" && path == "/api/v3/movie")
        );
    }
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
    harness.native.state.lock().await.library_unavailable = true;
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
    harness.close().await;
}

#[tokio::test]
async fn existing_movie_without_root_folder_path_preserves_native_settings_after_restart() {
    let mut harness = Harness::new().await;
    let mut movie = native_movie(7, 1);
    movie.root_folder_path.clear();
    let original = serde_json::to_vec(&movie).unwrap();
    harness.native.state.lock().await.movie = Some(movie);
    let response = harness
        .submit(&movie_request(), "movie-derived-root", REQUESTER)
        .await;
    assert_eq!(response.status(), 202);
    let record = response.json::<RequestRecord>().await.unwrap();
    harness.restart().await;
    harness.applied(&record.id).await;
    {
        let native = harness.native.state.lock().await;
        assert_eq!(
            serde_json::to_vec(native.movie.as_ref().unwrap()).unwrap(),
            original
        );
        assert_eq!(native.add_count, 0);
        assert_eq!(native.commands.len(), 1);
        assert_eq!(
            native.commands[0].body,
            Some(Command::MoviesSearch { movie_ids: vec![7] })
        );
    }
    harness.close().await;
}

#[tokio::test]
async fn existing_series_without_root_folder_path_preserves_native_settings_after_restart() {
    let mut harness = Harness::new().await;
    let mut series = native_series(8);
    series.root_folder_path.clear();
    series.path = "/media/movies/Example".into();
    let original = serde_json::to_vec(&series).unwrap();
    {
        let mut native = harness.native.state.lock().await;
        native.series = Some(series);
        native.episodes = vec![native_episode(22, 2, "2005-01-01T00:00:00Z", false)];
    }
    let input = CreateRequest::Series {
        tvdb_id: TvdbId::try_from(78804).unwrap(),
        intent: SeriesIntent::SearchNow {
            scope: EpisodeScope::Seasons {
                numbers: SeasonSet::try_from(vec![2]).unwrap(),
            },
        },
    };
    let response = harness
        .submit(&input, "series-derived-root", REQUESTER)
        .await;
    assert_eq!(response.status(), 202);
    let record = response.json::<RequestRecord>().await.unwrap();
    harness.restart().await;
    harness.applied(&record.id).await;
    {
        let native = harness.native.state.lock().await;
        assert_eq!(
            serde_json::to_vec(native.series.as_ref().unwrap()).unwrap(),
            original
        );
        assert_eq!(native.add_count, 0);
        assert_eq!(native.monitored, vec![vec![22]]);
        assert_eq!(native.commands.len(), 1);
        assert_eq!(
            native.commands[0].body,
            Some(Command::EpisodeSearch {
                episode_ids: vec![22]
            })
        );
    }
    harness.close().await;
}

#[tokio::test]
async fn existing_media_root_derivation_rejects_sibling_path_prefixes_before_acceptance() {
    for is_series in [false, true] {
        let harness = Harness::new().await;
        let input;
        {
            let mut native = harness.native.state.lock().await;
            input = if is_series {
                let mut series = native_series(8);
                series.root_folder_path.clear();
                series.path = "/media/movies-extra/Example".into();
                native.series = Some(series);
                CreateRequest::Series {
                    tvdb_id: TvdbId::try_from(78804).unwrap(),
                    intent: SeriesIntent::SearchNow {
                        scope: EpisodeScope::AllAired {},
                    },
                }
            } else {
                let mut movie = native_movie(7, 1);
                movie.root_folder_path.clear();
                movie.path = "/media/movies-extra/Primer".into();
                native.movie = Some(movie);
                movie_request()
            };
        }
        let response = harness.submit(&input, "sibling-root", REQUESTER).await;
        assert_eq!(response.status(), 409);
        assert_eq!(
            response
                .json::<dropshot::HttpErrorResponseBody>()
                .await
                .unwrap()
                .error_code
                .as_deref(),
            Some("native_settings_required")
        );
        assert!(harness.service.store.snapshot().await.requests.is_empty());
        assert!(
            harness
                .native
                .state
                .lock()
                .await
                .calls
                .iter()
                .all(|(method, _)| method == "GET")
        );
        harness.close().await;
    }
}

#[tokio::test]
async fn existing_media_settings_changes_before_dispatch_fail_without_native_writes() {
    enum Change {
        Quality,
        ExplicitRoot,
        DerivedRoot,
    }
    for is_series in [false, true] {
        for change in [Change::Quality, Change::ExplicitRoot, Change::DerivedRoot] {
            let mut harness = Harness::new().await;
            let input;
            {
                let mut native = harness.native.state.lock().await;
                input = if is_series {
                    let mut series = native_series(8);
                    series.root_folder_path.clear();
                    series.path = "/media/movies/Example".into();
                    native.series = Some(series);
                    native.episodes = vec![native_episode(22, 2, "2005-01-01T00:00:00Z", false)];
                    CreateRequest::Series {
                        tvdb_id: TvdbId::try_from(78804).unwrap(),
                        intent: SeriesIntent::SearchNow {
                            scope: EpisodeScope::AllAired {},
                        },
                    }
                } else {
                    let mut movie = native_movie(7, 1);
                    movie.root_folder_path.clear();
                    native.movie = Some(movie);
                    movie_request()
                };
            }
            let response = harness.submit(&input, "changed-settings", REQUESTER).await;
            assert_eq!(response.status(), 202);
            let record = response.json::<RequestRecord>().await.unwrap();
            {
                let mut native = harness.native.state.lock().await;
                if is_series {
                    let series = native.series.as_mut().unwrap();
                    match change {
                        Change::Quality => series.quality_profile_id = 2,
                        Change::ExplicitRoot => series.root_folder_path = "/media/alternate".into(),
                        Change::DerivedRoot => series.path = "/media/movies-extra/Example".into(),
                    }
                } else {
                    let movie = native.movie.as_mut().unwrap();
                    match change {
                        Change::Quality => movie.quality_profile_id = 2,
                        Change::ExplicitRoot => movie.root_folder_path = "/media/alternate".into(),
                        Change::DerivedRoot => movie.path = "/media/movies-extra/Primer".into(),
                    }
                }
            }
            harness.start_worker();
            let error = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match harness.record(&record.id).await.submission {
                        Submission::Failed { error } => break error,
                        Submission::Applied { .. } => {
                            panic!("changed native settings were accepted")
                        }
                        _ => tokio::time::sleep(Duration::from_millis(20)).await,
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(error.code, ApiErrorCode::ExistingSettingsConflict);
            {
                let native = harness.native.state.lock().await;
                assert_eq!(native.add_count, 0);
                assert!(native.monitored.is_empty());
                assert!(native.commands.is_empty());
                assert!(native.calls.iter().all(|(method, _)| method == "GET"));
            }
            harness.close().await;
        }
    }
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
        harness.bind_native_defaults(1, 1).await;
        let original;
        {
            let mut native = harness.native.state.lock().await;
            let mut series = native_series(8);
            series.quality_profile_id = 2;
            series.root_folder_path = "/media/alternate".into();
            series.path = "/media/alternate/Example".into();
            series.tags = vec![9];
            series.series_type = "anime".into();
            series.season_folder = false;
            original = serde_json::to_vec(&series).unwrap();
            native.series = Some(series);
            native.quality_profiles.push(UpNamed {
                id: 2,
                name: "Native TV quality".into(),
            });
            native.roots.push(alternate_root());
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
            intent: SeriesIntent::SearchNow {
                scope: EpisodeScope::Seasons {
                    numbers: SeasonSet::try_from(vec![2]).unwrap(),
                },
            },
        };
        let response = harness.submit(&input, "exact-season", REQUESTER).await;
        assert_eq!(response.status(), 202);
        let record = response.json::<RequestRecord>().await.unwrap();
        let accepted = harness.service.store.snapshot().await;
        let crate::media::store::Plan::Series { settings, root, .. } = &accepted.requests[0].plan
        else {
            panic!("expected series plan")
        };
        assert_eq!(settings.quality_profile_id.get(), 2);
        assert_eq!(settings.root_folder_id.get(), 2);
        assert_eq!(
            settings
                .tags
                .iter()
                .map(|tag| tag.get())
                .collect::<Vec<_>>(),
            vec![9]
        );
        assert_eq!(settings.series_type, SeriesType::Anime);
        assert!(!settings.season_folders);
        assert_eq!(root, "/media/alternate");
        harness.start_worker();
        harness.applied(&record.id).await;
        {
            let native = harness.native.state.lock().await;
            assert_eq!(
                serde_json::to_vec(native.series.as_ref().unwrap()).unwrap(),
                original
            );
            assert_eq!(native.add_count, 0);
            assert!(
                !native
                    .calls
                    .iter()
                    .any(|(method, path)| method != "GET" && path == "/api/v3/series")
            );
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
        ManagerId::Radarr,
        crate::media::service::RootObservation {
            accessible: true,
            observed_at_unix_ms: now,
        },
    );
    let snapshot = harness
        .service
        .health_snapshot(Some(Operation::RequestMovie))
        .await
        .unwrap();
    assert_eq!(snapshot.readiness[0].state, ReadinessState::Ready);
    // Import additionally needs current staging evidence, but a request does not
    // claim that the transfer/import stack is ready or require optional Plex.
    let import = harness
        .service
        .health_snapshot(Some(Operation::ImportMovie))
        .await
        .unwrap();
    assert_eq!(import.readiness[0].state, ReadinessState::Blocked);
    harness.service.cache.write().await.download_storage = Some((true, now));
    assert_eq!(
        harness
            .service
            .health_snapshot(Some(Operation::ImportMovie))
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
        .health_snapshot(Some(Operation::RequestMovie))
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
            .submit(&movie_request(), "shutdown", REQUESTER)
            .await
            .status(),
        503
    );
    assert!(harness.service.store.snapshot().await.requests.is_empty());
    harness.close().await;
}

#[tokio::test]
async fn operation_readiness_needs_its_own_fresh_native_root_proof() {
    let mut harness = Harness::new().await;
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
        ManagerId::Sonarr,
        crate::media::service::RootObservation {
            accessible: true,
            observed_at_unix_ms: now,
        },
    );
    let missing = harness
        .service
        .health_snapshot(Some(Operation::RequestMovie))
        .await
        .unwrap();
    assert_eq!(missing.readiness[0].state, ReadinessState::Blocked);
    assert!(
        missing.readiness[0]
            .reasons
            .contains(&HealthIssueCode::ChecksMissing)
    );
    assert_eq!(missing.readiness[0].expires_at_unix_ms, 0);
    for (accessible, at) in [(true, now - 45_001), (true, now + 60_000), (false, now)] {
        harness.service.cache.write().await.roots.insert(
            ManagerId::Radarr,
            crate::media::service::RootObservation {
                accessible,
                observed_at_unix_ms: at,
            },
        );
        let invalid = harness
            .service
            .health_snapshot(Some(Operation::RequestMovie))
            .await
            .unwrap();
        assert_eq!(invalid.readiness[0].state, ReadinessState::Blocked);
        assert!(
            invalid.readiness[0]
                .reasons
                .contains(&HealthIssueCode::StorageUnavailable)
        );
        if at > now {
            assert_eq!(invalid.readiness[0].expires_at_unix_ms, 0);
        }
    }
    harness.start_worker();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = harness
                .client
                .get(format!(
                    "{}/api/v1/health/stack?operation=request_movie",
                    harness.url
                ))
                .bearer_auth(READER)
                .send()
                .await
                .unwrap()
                .json::<HealthSnapshot>()
                .await
                .unwrap();
            if snapshot.readiness[0].state == ReadinessState::Ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        harness
            .native
            .state
            .lock()
            .await
            .calls
            .iter()
            .all(|(method, _)| method == "GET")
    );
    assert!(harness.service.store.snapshot().await.requests.is_empty());
    harness.close().await;
}

#[tokio::test]
#[ignore = "requires HA_DOWNLOADS_TEST_PYTHON pointing to the HA test environment"]
async fn real_home_assistant_bridge_uses_live_rust_contract() {
    let python = std::env::var("HA_DOWNLOADS_TEST_PYTHON").expect("HA test Python is required");
    let mut harness = Harness::new().await;
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
        .status()
        .await
        .unwrap();
    assert!(status.success());
    harness.close().await;
}

#[tokio::test]
#[ignore = "requires HA_DOWNLOADS_TEST_PYTHON pointing to the HA test environment"]
async fn real_home_assistant_repairs_use_native_manager_health() {
    let python = std::env::var("HA_DOWNLOADS_TEST_PYTHON").expect("HA test Python is required");
    fn check(source: &str, severity: &str, message: &str) -> NativeHealthCheck {
        NativeHealthCheck {
            source: source.into(),
            severity: severity.into(),
            message: message.into(),
        }
    }
    for (checks, expected) in [
        (
            vec![check(
                "UpdateCheck",
                "warning",
                "New update is available: v2.7.0.5723",
            )],
            None,
        ),
        (
            vec![check(
                "UpdateCheck",
                "warning",
                "Unable to check for updates",
            )],
            Some(HealthIssueCode::UpstreamHealth),
        ),
        (
            vec![check(
                "UpdateCheck",
                "error",
                "New update is available: v2.7.0.5723",
            )],
            Some(HealthIssueCode::UpstreamHealth),
        ),
        (
            vec![check(
                "UpdateCheck",
                "warning",
                "New update is available: v2.7.0.5723 (unknown)",
            )],
            Some(HealthIssueCode::UpstreamHealth),
        ),
        (
            vec![
                check(
                    "UpdateCheck",
                    "warning",
                    "New update is available: v2.7.0.5723",
                ),
                check("DownloadClientCheck", "error", "Private native details"),
            ],
            Some(HealthIssueCode::DownloadClientUnavailable),
        ),
    ] {
        let healthy = expected.is_none();
        let mut harness = Harness::new().await;
        harness.native.state.lock().await.health = checks;
        harness.config.prowlarr = harness.config.radarr.clone();
        harness.restart().await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = harness.service.health_snapshot(None).await.unwrap();
            if let Some(component) = snapshot.components.iter().find(|item| {
                item.component == Component::Prowlarr
                    && matches!(item.observation, Observation::Observed { .. })
            }) {
                assert!(
                    match &component.observation {
                        Observation::Observed {
                            outcome: HealthOutcome::Healthy,
                            ..
                        } => true,
                        Observation::Observed {
                            outcome: HealthOutcome::Degraded { issues },
                            ..
                        } => issues
                            .iter()
                            .all(|issue| issue.code == HealthIssueCode::UpdateAvailable),
                        _ => false,
                    } == healthy,
                    "only the exact routine release notice is excluded from service faults"
                );
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let status = tokio::process::Command::new(&python)
            .args([
                "-m",
                "pytest",
                "-q",
                "-c",
                "integration/pyproject.toml",
                "tests_ha/test_rust_repairs.py",
            ])
            .env("DOWNLOADS_INTEROP_URL", &harness.url)
            .env("DOWNLOADS_INTEROP_TOKEN", INTEGRATION)
            .env(
                "DOWNLOADS_INTEROP_HEALTHY",
                if healthy { "true" } else { "false" },
            )
            .env(
                "DOWNLOADS_INTEROP_CODE",
                expected.map(|code| code.to_string()).unwrap_or_default(),
            )
            .status()
            .await
            .unwrap();
        assert!(status.success());
        harness.close().await;
    }
}
fn native_download(series: bool) -> UpQueue {
    UpQueue {
        id: if series { 502 } else { 501 },
        title: "Example.Release.2160p".into(),
        movie: (!series).then(|| native_movie(7, 1)),
        series: series.then(|| native_series(8)),
        episode: series.then_some(UpQueueEpisode {
            season_number: 2,
            episode_number: 3,
        }),
        size: Some(1000.0),
        sizeleft: Some(250.0),
        status: "downloading".into(),
        tracked_download_state: "downloading".into(),
        tracked_download_status: "ok".into(),
        quality: Some(UpDownloadQuality {
            quality: UpQuality {
                name: "Remux-2160p".into(),
                resolution: Some(2160),
                source: Some("bluray".into()),
                modifier: Some("remux".into()),
            },
            revision: Some(UpRevision {
                version: 2,
                real: 0,
                is_repack: true,
            }),
        }),
        timeleft: Some("02:01:03".into()),
        download_client: Some("Native downloader".into()),
        indexer: Some("Example indexer".into()),
        protocol: Some("usenet".into()),
        languages: vec![UpNamed {
            id: 1,
            name: "English".into(),
        }],
        custom_formats: vec![UpNamed {
            id: 7,
            name: "Release format".into(),
        }],
        custom_format_score: Some(25),
        added: Some("2026-11-07T10:00:00Z".into()),
        estimated_completion_time: Some("2026-11-07T12:01:03Z".into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn library_details_use_existing_native_metadata_without_file_or_settings_requests() {
    let harness = Harness::new().await;
    {
        let mut native = harness.native.state.lock().await;
        let mut movie = native_movie(7, 1);
        movie.has_file = true;
        movie.movie_file_id = 9;
        movie.metadata = UpMovieMetadata {
            runtime: Some(77),
            genres: vec!["Science Fiction".into()],
            studio: Some("Example studio".into()),
            certification: Some("PG-13".into()),
            movie_file: Some(UpMovieFile {
                quality: native_download(false).quality,
                size: Some(12345),
                media_info: Some(UpMediaInfo {
                    video_codec: Some("HEVC".into()),
                    audio_codec: Some("DTS-HD MA".into()),
                    audio_channels: Some(5.1),
                }),
            }),
        };
        native.movie = Some(movie);
        let mut series = native_series(8);
        series.metadata = UpSeriesMetadata {
            runtime: Some(42),
            genres: vec!["Drama".into()],
            network: Some("Example network".into()),
            certification: Some("TV-14".into()),
            next_airing: Some("2026-12-01T12:00:00Z".into()),
        };
        native.series = Some(series);
    }
    let movie = harness
        .client
        .get(format!("{}/api/v1/movies/14337", harness.url))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap()
        .json::<MediaItem>()
        .await
        .unwrap();
    let MediaItem::Movie {
        metadata: Some(metadata),
        ..
    } = movie
    else {
        panic!("movie metadata missing")
    };
    assert_eq!(metadata.runtime_minutes, Some(77));
    assert_eq!(metadata.genres, vec!["Science Fiction"]);
    assert_eq!(metadata.studio.as_deref(), Some("Example studio"));
    let file = metadata.file.unwrap();
    assert_eq!(file.quality.unwrap().name, "Remux-2160p");
    assert_eq!(file.size_bytes, Some(12345));
    assert_eq!(file.video_codec.as_deref(), Some("HEVC"));
    assert_eq!(file.audio_codec.as_deref(), Some("DTS-HD MA"));
    assert_eq!(file.audio_channels.as_deref(), Some("5.1"));
    let series = harness
        .client
        .get(format!("{}/api/v1/series/78804", harness.url))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap()
        .json::<MediaItem>()
        .await
        .unwrap();
    let MediaItem::Series {
        metadata: Some(metadata),
        ..
    } = series
    else {
        panic!("series metadata missing")
    };
    assert_eq!(metadata.runtime_minutes, Some(42));
    assert_eq!(metadata.network.as_deref(), Some("Example network"));
    assert_eq!(
        metadata.next_airing.as_deref(),
        Some("2026-12-01T12:00:00Z")
    );
    assert!(
        harness
            .native
            .state
            .lock()
            .await
            .calls
            .iter()
            .all(|(method, path)| method == "GET"
                && matches!(
                    path.as_str(),
                    "/api/v3/movie"
                        | "/api/v3/series"
                        | "/api/v3/qualityprofile"
                        | "/api/v3/rootfolder"
                        | "/api/v3/episode"
                        | "/api/v3/queue"
                ))
    );
    assert!(harness.service.store.snapshot().await.requests.is_empty());
    harness.close().await;
}

#[tokio::test]
async fn prowlarr_availability_notices_require_the_matching_home_assistant_app_update() {
    for (available, unrelated, expected) in [
        (Some(false), false, false),
        (Some(true), false, true),
        (None, false, false),
        (Some(true), true, false),
    ] {
        let mut harness = Harness::with_supervisor(true).await;
        {
            let mut native = harness.native.state.lock().await;
            native.supervisor_update = available;
            native.supervisor_unrelated = unrelated;
            native.prowlarr_health = vec![UpHealth {
                source: "UpdateCheck".into(),
                severity: "warning".into(),
                message: None,
                wiki_url: "https://wiki.example.invalid/prowlarr#new-update-is-available".into(),
            }];
        }
        harness.start_worker();
        let health = wait_prowlarr(&harness).await;
        if expected {
            let HealthOutcome::Degraded { issues } = health else {
                panic!("expected HA app update")
            };
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].code, HealthIssueCode::UpdateAvailable);
            assert_eq!(
                issues[0].message,
                "An update is available in Home Assistant."
            );
        } else {
            assert!(matches!(health, HealthOutcome::Healthy));
        }
        assert!(
            harness
                .native
                .state
                .lock()
                .await
                .calls
                .iter()
                .all(|(method, _)| method == "GET")
        );
        harness.close().await;
    }
}

async fn wait_prowlarr(harness: &Harness) -> HealthOutcome {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let component = harness
                .client
                .get(format!("{}/api/v1/health/components/prowlarr", harness.url))
                .bearer_auth(READER)
                .send()
                .await
                .unwrap()
                .json::<ComponentHealth>()
                .await
                .unwrap();
            if let Observation::Observed { outcome, .. } = component.observation {
                return outcome;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn prowlarr_update_filter_preserves_check_failures_indexers_connections_and_standalone_notices()
 {
    for unavailable in [false, true] {
        let mut harness = Harness::with_supervisor(true).await;
        {
            let mut native = harness.native.state.lock().await;
            native.supervisor_update = Some(false);
            native.unavailable = unavailable;
            native.prowlarr_health = vec![
                UpHealth {
                    source: "UpdateCheck".into(),
                    severity: "error".into(),
                    message: None,
                    wiki_url: "https://wiki.example.invalid/prowlarr#update-check-failed".into(),
                },
                UpHealth {
                    source: "IndexerStatusCheck".into(),
                    severity: "error".into(),
                    message: None,
                    wiki_url: String::new(),
                },
            ];
        }
        harness.start_worker();
        match wait_prowlarr(&harness).await {
            HealthOutcome::Unavailable { .. } if unavailable => {}
            HealthOutcome::Degraded { issues } if !unavailable => {
                assert_eq!(issues.len(), 2);
                assert!(
                    issues
                        .iter()
                        .all(|issue| issue.severity == IssueSeverity::Error)
                );
                assert!(
                    issues
                        .iter()
                        .any(|issue| issue.code == HealthIssueCode::IndexerUnavailable)
                );
                assert!(
                    issues
                        .iter()
                        .all(|issue| issue.message != "An update is available.")
                );
            }
            _ => panic!("native functional failure was hidden"),
        }
        harness.close().await;
    }
    let mut harness = Harness::new().await;
    harness.native.state.lock().await.prowlarr_health = vec![UpHealth {
        source: "UpdateCheck".into(),
        severity: "warning".into(),
        message: None,
        wiki_url: "https://wiki.example.invalid/prowlarr#new-update-is-available".into(),
    }];
    harness.start_worker();
    let HealthOutcome::Degraded { issues } = wait_prowlarr(&harness).await else {
        panic!("standalone native notice missing")
    };
    assert_eq!(issues[0].message, "An update is available.");
    harness.close().await;
}

#[tokio::test]
async fn download_details_read_real_release_metadata_without_settings_or_mutations() {
    let harness = Harness::new().await;
    {
        let mut native = harness.native.state.lock().await;
        native.movie_queue = vec![native_download(false)];
        native.series_queue = vec![native_download(true)];
    }
    for (id, manager) in [
        ("radarr:501", ManagerId::Radarr),
        ("sonarr:502", ManagerId::Sonarr),
    ] {
        let response = harness
            .client
            .get(format!(
                "{}/api/v1/queue/{}",
                harness.url,
                id.replace(':', "%3A")
            ))
            .bearer_auth(READER)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let details = response.json::<DownloadDetails>().await.unwrap();
        assert_eq!(details.id.to_string(), id);
        assert_eq!(details.manager, manager);
        assert_eq!(details.status, DownloadStatus::Downloading);
        assert_eq!(details.import_status, DownloadImportStatus::NotStarted);
        let quality = details.quality.unwrap();
        assert_eq!(quality.name, "Remux-2160p");
        assert_eq!(quality.resolution, Some(2160));
        assert_eq!(quality.source.as_deref(), Some("bluray"));
        assert_eq!(quality.modifier.as_deref(), Some("remux"));
        assert!(quality.revision.unwrap().is_repack);
        let progress = details.progress.unwrap();
        assert_eq!(
            (
                progress.completed_bytes,
                progress.total_bytes,
                progress.remaining_bytes
            ),
            (750, 1000, 250)
        );
        assert_eq!(details.remaining_seconds, Some(7263));
        assert_eq!(details.protocol, Some(DownloadProtocol::Usenet));
        assert_eq!(details.languages, vec!["English"]);
        assert_eq!(details.custom_formats, vec!["Release format"]);
        assert_eq!(details.custom_format_score, Some(25));
        assert!(details.added_at.is_some() && details.estimated_completion_at.is_some());
        assert!(details.warnings.is_empty());
        assert_eq!(details.episode.is_some(), manager == ManagerId::Sonarr);
    }
    let queue = harness
        .client
        .get(format!("{}/api/v1/queue", harness.url))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap()
        .json::<Page<QueueItem>>()
        .await
        .unwrap();
    assert_eq!(
        queue
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["radarr:501", "sonarr:502"]
    );
    assert!(harness.service.store.snapshot().await.requests.is_empty());
    assert!(
        harness
            .native
            .state
            .lock()
            .await
            .calls
            .iter()
            .all(|(method, path)| method == "GET"
                && (path == "/api/v3/queue/details" || path == "/api/v3/queue"))
    );
    harness.close().await;
}

#[tokio::test]
async fn download_details_keep_unknowns_and_private_native_errors_out_of_public_values() {
    let harness = Harness::new().await;
    let mut record = native_download(false);
    record.sizeleft = None;
    record.quality = None;
    record.timeleft = Some("unknown".into());
    record.download_client = Some("https://private.invalid?apikey=fixture-secret".into());
    record.indexer = Some("/private/provider/path".into());
    record.protocol = Some("new-protocol".into());
    record.tracked_download_state = "importBlocked".into();
    record.tracked_download_status = "error".into();
    record.status_messages = vec![UpDownloadMessage {
        title: "Failure in /private/output".into(),
        messages: vec!["apikey=fixture-secret".into()],
    }];
    record.error_message = Some("Bearer fixture-secret".into());
    record.added = Some("2026-02-30T10:00:00Z".into());
    harness.native.state.lock().await.movie_queue = vec![record];
    let details = harness
        .client
        .get(format!("{}/api/v1/queue/radarr:501", harness.url))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap()
        .json::<DownloadDetails>()
        .await
        .unwrap();
    assert!(
        details.progress.is_none()
            && details.quality.is_none()
            && details.remaining_seconds.is_none()
    );
    assert!(
        details.download_client.is_none()
            && details.indexer.is_none()
            && details.added_at.is_none()
    );
    assert_eq!(details.protocol, Some(DownloadProtocol::Unknown));
    assert_eq!(details.import_status, DownloadImportStatus::Failed);
    assert!(matches!(
        details.acquisition,
        Acquisition::Blocked {
            reason: AcquisitionBlock::ImportFailed
        }
    ));
    assert_eq!(details.warnings.len(), 1);
    assert_eq!(details.warnings[0].code, DownloadWarningCode::ImportBlocked);
    assert_eq!(
        details.warnings[0].message,
        "The download needs attention before it can be imported."
    );
    harness.close().await;
}

#[tokio::test]
async fn download_details_distinguish_invalid_gone_and_unavailable_without_journal_effects() {
    let harness = Harness::new().await;
    for id in [
        "radarr:0",
        "radarr:01",
        "radarr:-1",
        "radarr:2147483648",
        "plex:1",
        "radarr-1",
    ] {
        let response = harness
            .client
            .get(format!("{}/api/v1/queue/{id}", harness.url))
            .bearer_auth(READER)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    assert!(harness.native.state.lock().await.calls.is_empty());
    let url = format!("{}/api/v1/queue/radarr:501", harness.url);
    assert_eq!(harness.client.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        harness
            .client
            .get(&url)
            .bearer_auth(READER)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    harness.native.state.lock().await.queue_unavailable = true;
    assert_eq!(
        harness
            .client
            .get(&url)
            .bearer_auth(READER)
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    {
        let mut native = harness.native.state.lock().await;
        native.queue_unavailable = false;
        native.movie_queue = vec![native_download(false); 1001];
    }
    assert_eq!(
        harness
            .client
            .get(&url)
            .bearer_auth(READER)
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    assert!(harness.service.store.snapshot().await.requests.is_empty());
    harness.close().await;
}

#[tokio::test]
async fn native_blocked_and_failed_states_override_completed_transfer_status() {
    let harness = Harness::new().await;
    for (state, reason, warning, import) in [
        (
            "importBlocked",
            AcquisitionBlock::ImportFailed,
            DownloadWarningCode::ImportBlocked,
            DownloadImportStatus::Failed,
        ),
        (
            "failedPending",
            AcquisitionBlock::DownloadFailed,
            DownloadWarningCode::DownloadFailed,
            DownloadImportStatus::Unknown,
        ),
        (
            "failed",
            AcquisitionBlock::DownloadFailed,
            DownloadWarningCode::DownloadFailed,
            DownloadImportStatus::Unknown,
        ),
    ] {
        let mut record = native_download(false);
        record.status = "completed".into();
        record.tracked_download_status = "warning".into();
        record.tracked_download_state = state.into();
        harness.native.state.lock().await.movie_queue = vec![record];
        let details = harness
            .client
            .get(format!("{}/api/v1/queue/radarr:501", harness.url))
            .bearer_auth(READER)
            .send()
            .await
            .unwrap()
            .json::<DownloadDetails>()
            .await
            .unwrap();
        assert!(
            matches!(details.acquisition, Acquisition::Blocked { reason: actual } if actual == reason),
            "wrong acquisition for {state}: {:?}",
            details.acquisition
        );
        assert_eq!(details.import_status, import);
        assert_eq!(details.warnings[0].code, warning);
    }
    harness.close().await;
}

fn native_import(series: bool) -> crate::media::imports::NativeImport {
    use crate::media::imports::{NativeImport, NativeImportData, NativeImportEpisode};
    NativeImport {
        id: if series { 72 } else { 71 },
        event_type: "downloadFolderImported".into(),
        date: if series {
            "2026-01-02T00:00:00Z"
        } else {
            "2026-01-01T00:00:00Z"
        }
        .into(),
        quality: native_download(series).quality,
        movie: (!series).then(|| native_movie(7, 1)),
        series: series.then(|| native_series(8)),
        episode: series.then_some(NativeImportEpisode {
            season_number: 2,
            episode_number: 3,
            tvdb_id: Some(123456),
        }),
        data: NativeImportData {
            imported_path: Some(
                if series {
                    "/media/series/Example.S02E03.mkv"
                } else {
                    "/media/movies/Example.mkv"
                }
                .into(),
            ),
            size: Some("12345".into()),
        },
    }
}
fn plex_import(series: bool) -> PlexFixtureMetadata {
    PlexFixtureMetadata {
        rating_key: "42".into(),
        kind: if series { "episode" } else { "movie" }.into(),
        guids: vec![PlexFixtureGuid {
            id: if series {
                "tvdb://123456"
            } else {
                "tmdb://14337"
            }
            .into(),
        }],
        updated_at: 1_767_312_060,
        parent_index: series.then_some(2),
        index: series.then_some(3),
        media: vec![PlexFixtureMedia {
            parts: vec![PlexFixturePart {
                file: native_import(series).data.imported_path.unwrap(),
                size: 12345,
                exists: Some(true),
                accessible: Some(true),
                streams: Some(vec![
                    PlexFixtureStream {
                        stream_type: 1,
                        language_code: None,
                    },
                    PlexFixtureStream {
                        stream_type: 3,
                        language_code: Some("eng".into()),
                    },
                ]),
            }],
        }],
    }
}
async fn imports_page(harness: &Harness, query: &str) -> Page<ImportedItem> {
    let response = harness
        .client
        .get(format!("{}/api/v1/imports{query}", harness.url))
        .bearer_auth(READER)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json::<Page<ImportedItem>>().await.unwrap()
}
#[tokio::test]
async fn native_import_history_includes_external_work_and_pages_stable_ids_newest_first() {
    let harness = Harness::new().await;
    {
        let mut native = harness.native.state.lock().await;
        native.movie_imports = vec![native_import(false)];
        native.series_imports = vec![native_import(true)];
    }
    let first = imports_page(&harness, "?limit=1").await;
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].id.to_string(), "sonarr:72");
    assert!(matches!(
        first.items[0].plex,
        PlexImportStatus::NotConfigured
    ));
    assert!(matches!(
        first.items[0].subtitles,
        SubtitleImportStatus::Unknown {
            reason: SubtitleUnknownReason::PlexUnverified
        }
    ));
    assert!(
        first
            .coverage
            .iter()
            .all(|source| source.state == CoverageState::Available)
    );
    let token = first.next_page.unwrap();
    let second = imports_page(&harness, &format!("?limit=1&page_token={token}")).await;
    assert_eq!(second.items[0].id.to_string(), "radarr:71");
    assert_eq!(second.items[0].imported_at, "2026-01-01T00:00:00Z");
    assert_eq!(
        second.items[0].quality.as_ref().unwrap().name,
        "Remux-2160p"
    );
    assert!(second.next_page.is_none());
    assert!(harness.service.store.snapshot().await.requests.is_empty());
    let native = harness.native.state.lock().await;
    assert!(
        native
            .calls
            .iter()
            .all(|(method, path)| method == "GET" && path == "/api/v3/history")
    );
    assert!(native.queries.iter().all(|(_, query)| {
        reqwest::Url::parse(&format!("http://example.invalid?{query}"))
            .unwrap()
            .query_pairs()
            .any(|(key, value)| key == "eventType" && value == "3")
    }));
    drop(native);
    harness.close().await;
}
#[tokio::test]
async fn exact_imported_movie_and_episode_file_proof_verifies_plex_and_subtitles_read_only() {
    for series in [false, true] {
        let harness = Harness::with_plex(true).await;
        {
            let mut native = harness.native.state.lock().await;
            if series {
                native.series_imports = vec![native_import(true)];
            } else {
                native.movie_imports = vec![native_import(false)];
            }
            native.plex_status = 200;
            native.plex_requires_key = true;
            native.plex_metadata = vec![plex_import(series)];
        }
        let page = imports_page(&harness, "").await;
        assert_eq!(page.items.len(), 1);
        assert!(matches!(
            page.items[0].plex,
            PlexImportStatus::Verified { .. }
        ));
        assert!(
            matches!(&page.items[0].subtitles, SubtitleImportStatus::Verified { languages, .. } if languages == &vec!["eng".to_string()])
        );
        assert!(harness.service.store.snapshot().await.requests.is_empty());
        let native = harness.native.state.lock().await;
        assert!(native.calls.iter().all(|(method, path)| method == "GET"
            && (path == "/api/v3/history"
                || path == "/library/sections"
                || path == "/library/all"
                || path == "/library/metadata/42")));
        let query = native
            .queries
            .iter()
            .find(|(path, _)| path == "/library/metadata/42")
            .unwrap();
        let pairs = reqwest::Url::parse(&format!("http://example.invalid?{}", query.1))
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(pairs.get("skipRefresh").map(String::as_str), Some("1"));
        assert_eq!(
            pairs.get("checkFileAvailability").map(String::as_str),
            Some("1")
        );
        assert!(!pairs.contains_key("checkFiles"));
        drop(native);
        harness.close().await;
    }
}
#[tokio::test]
async fn completed_history_never_infers_current_plex_or_subtitle_readiness_from_a_title_match() {
    enum Case {
        WrongGuid,
        WrongPath,
        WrongSize,
        Older,
        FutureMetadata,
        MissingExistence,
        NoStreams,
        NoSubtitles,
        Authentication,
        Connection,
        NoEpisodeGuid,
        NoFile,
    }
    for case in [
        Case::WrongGuid,
        Case::WrongPath,
        Case::WrongSize,
        Case::Older,
        Case::FutureMetadata,
        Case::MissingExistence,
        Case::NoStreams,
        Case::NoSubtitles,
        Case::Authentication,
        Case::Connection,
        Case::NoEpisodeGuid,
        Case::NoFile,
    ] {
        let series = matches!(case, Case::NoEpisodeGuid);
        let harness = Harness::with_plex(!matches!(case, Case::Authentication)).await;
        {
            let mut native = harness.native.state.lock().await;
            let mut imported = native_import(series);
            let mut plex = plex_import(series);
            native.plex_status = 200;
            native.plex_requires_key = true;
            match case {
                Case::WrongGuid => plex.guids[0].id = "tmdb://999".into(),
                Case::WrongPath => plex.media[0].parts[0].file = "/media/older-edition.mkv".into(),
                Case::WrongSize => plex.media[0].parts[0].size = 1,
                Case::Older => plex.updated_at = 1,
                Case::FutureMetadata => plex.updated_at = crate::monitor::unix_ms() / 1000 + 86_400,
                Case::MissingExistence => plex.media[0].parts[0].exists = None,
                Case::NoStreams => plex.media[0].parts[0].streams = None,
                Case::NoSubtitles => {
                    plex.media[0].parts[0].streams = Some(vec![PlexFixtureStream {
                        stream_type: 1,
                        language_code: None,
                    }])
                }
                Case::Authentication => {}
                Case::Connection => native.plex_status = 503,
                Case::NoEpisodeGuid => imported.episode.as_mut().unwrap().tvdb_id = None,
                Case::NoFile => imported.data.imported_path = None,
            }
            if series {
                native.series_imports = vec![imported];
            } else {
                native.movie_imports = vec![imported];
            }
            native.plex_metadata = vec![plex];
        }
        let page = imports_page(&harness, "").await;
        assert_eq!(page.items.len(), 1);
        let item = &page.items[0];
        match case {
            Case::NoStreams => {
                assert!(matches!(item.plex, PlexImportStatus::Verified { .. }));
                assert!(matches!(
                    item.subtitles,
                    SubtitleImportStatus::Unknown {
                        reason: SubtitleUnknownReason::StreamsUnavailable
                    }
                ));
            }
            Case::NoSubtitles => {
                assert!(matches!(item.plex, PlexImportStatus::Verified { .. }));
                assert!(matches!(
                    item.subtitles,
                    SubtitleImportStatus::Missing { .. }
                ));
            }
            Case::Authentication => assert!(matches!(
                item.plex,
                PlexImportStatus::Unavailable {
                    reason: UnavailableReason::Authentication
                }
            )),
            Case::Connection => assert!(matches!(
                item.plex,
                PlexImportStatus::Unavailable {
                    reason: UnavailableReason::Connection
                }
            )),
            Case::Older => assert!(matches!(
                item.plex,
                PlexImportStatus::Unknown {
                    reason: ImportVerificationReason::OlderVersion
                }
            )),
            Case::MissingExistence | Case::NoFile | Case::FutureMetadata => assert!(matches!(
                item.plex,
                PlexImportStatus::Unknown {
                    reason: ImportVerificationReason::FileEvidenceMissing
                }
            )),
            Case::NoEpisodeGuid => assert!(matches!(
                item.plex,
                PlexImportStatus::Unknown {
                    reason: ImportVerificationReason::IdentityUnavailable
                }
            )),
            _ => assert!(matches!(
                item.plex,
                PlexImportStatus::Unknown {
                    reason: ImportVerificationReason::NotMatched
                }
            )),
        }
        if !matches!(case, Case::NoStreams | Case::NoSubtitles) {
            assert!(matches!(
                item.subtitles,
                SubtitleImportStatus::Unknown {
                    reason: SubtitleUnknownReason::PlexUnverified
                }
            ));
        }
        assert!(harness.service.store.snapshot().await.requests.is_empty());
        harness.close().await;
    }
}
#[tokio::test]
async fn import_history_reports_truncation_and_retains_native_receipts_during_source_failure() {
    let harness = Harness::new().await;
    {
        let mut native = harness.native.state.lock().await;
        native.movie_imports = vec![native_import(false)];
        native.movie_import_total = Some(2001);
    }
    let before = imports_page(&harness, "").await;
    assert_eq!(before.items.len(), 1);
    assert_eq!(before.coverage[0].state, CoverageState::Partial);
    harness.native.state.lock().await.history_unavailable = true;
    let after = imports_page(&harness, "").await;
    assert_eq!(after.items[0].id, before.items[0].id);
    assert_eq!(
        after.items[0].observed_at_unix_ms,
        before.items[0].observed_at_unix_ms
    );
    assert!(
        after
            .coverage
            .iter()
            .all(|source| source.state == CoverageState::Unavailable)
    );
    harness.close().await;
    let harness = Harness::new().await;
    {
        let mut native = harness.native.state.lock().await;
        let mut transfer = native_download(false);
        transfer.status = "completed".into();
        native.movie_queue = vec![transfer];
        let mut grabbed = native_import(false);
        grabbed.event_type = "grabbed".into();
        native.movie_imports = vec![grabbed];
    }
    let unproven = imports_page(&harness, "").await;
    assert!(unproven.items.is_empty());
    assert_eq!(unproven.coverage[0].state, CoverageState::Partial);
    harness.close().await;
}

#[tokio::test]
async fn imports_since_filters_before_paging_and_plex_and_binds_validated_utc_cursors() {
    let harness = Harness::with_plex(true).await;
    {
        let mut native = harness.native.state.lock().await;
        native.movie_imports = vec![native_import(false)];
        native.series_imports = vec![native_import(true)];
        native.plex_status = 200;
        native.plex_requires_key = true;
        native.plex_metadata = vec![plex_import(true)];
    }
    let recent = imports_page(&harness, "?since=2026-01-02T00:00:00Z&limit=1").await;
    assert_eq!(recent.items.len(), 1);
    assert_eq!(recent.items[0].id.to_string(), "sonarr:72");
    assert!(recent.next_page.is_none());
    {
        let mut native = harness.native.state.lock().await;
        assert!(
            native
                .queries
                .iter()
                .filter(|(path, _)| path == "/library/all")
                .all(|(_, query)| !query.contains("14337"))
        );
        native.calls.clear();
        native.queries.clear();
    }
    let future = imports_page(&harness, "?since=9999-12-31T23:59:59Z").await;
    assert!(future.items.is_empty());
    assert!(
        harness
            .native
            .state
            .lock()
            .await
            .calls
            .iter()
            .all(|(_, path)| path == "/api/v3/history")
    );
    harness.native.state.lock().await.calls.clear();
    for since in [
        "tomorrow",
        "2026-01-02",
        "2026-01-02T01:00:00+01:00",
        "2026-02-30T00:00:00Z",
        "2026-+1-01T00:00:00Z",
    ] {
        let response = harness
            .client
            .get(format!("{}/api/v1/imports", harness.url))
            .query(&[("since", since)])
            .bearer_auth(READER)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    assert!(harness.native.state.lock().await.calls.is_empty());
    let all = imports_page(&harness, "?limit=1").await;
    let cursor = all.next_page.unwrap();
    harness.native.state.lock().await.calls.clear();
    let mismatch = harness
        .client
        .get(format!("{}/api/v1/imports", harness.url))
        .query(&[
            ("page_token", cursor.as_str()),
            ("since", "2026-01-01T00:00:00Z"),
        ])
        .bearer_auth(READER)
        .send()
        .await
        .unwrap();
    assert_eq!(mismatch.status(), 400);
    assert!(harness.native.state.lock().await.calls.is_empty());
    harness.close().await;
}
