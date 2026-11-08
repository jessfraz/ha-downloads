use ha_downloads::media::model::{
    Acquisition, ApiErrorCode, Capabilities, Component, ComponentHealth, CoverageState,
    CreateRequest, CredentialRole, DownloadDetails, DownloadImportStatus, DownloadProtocol,
    DownloadStatus, Freshness, HealthIssueCode, HealthOutcome, HealthSnapshot, ImportedItem,
    LibraryState, ManagerId, ManagerList, MediaIdentity, MediaItem, MovieIntent, Observation,
    Operation, OperationReadiness, Page, PlexImportStatus, ReadinessState, RequestAvailability,
    RequestId, RequestRecord, SourceCoverage, Submission, SubtitleImportStatus,
    SubtitleUnknownReason, SystemResponse, Transport,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Command, Output},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const REQUEST: &str = "266319ec-c797-460c-a49e-6b90fc1f6ae8";

#[derive(Debug)]
struct Captured {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct Fixture {
    origin: String,
    requests: Arc<Mutex<Vec<Captured>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Fixture {
    fn json(status: u16, body: &impl Serialize) -> Self {
        Self::response(
            status,
            serde_json::to_vec(body).unwrap(),
            &[],
            Duration::ZERO,
        )
    }

    fn response(status: u16, body: Vec<u8>, headers: &[(&str, &str)], delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let extra = headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}\r\n"))
            .collect::<String>();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // macOS may inherit the listening socket's nonblocking mode.
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        captured.lock().unwrap().push(read_request(&mut stream));
                        thread::sleep(delay);
                        let response = format!(
                            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
                            body.len()
                        );
                        // The timeout test deliberately closes before the fixture replies.
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            }
        });
        Self {
            origin,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn captured(&self) -> std::sync::MutexGuard<'_, Vec<Captured>> {
        self.requests.lock().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                assert!(result.is_ok(), "HTTP fixture worker failed");
            }
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Captured {
    let mut data = Vec::new();
    let mut buffer = [0; 4096];
    let boundary = loop {
        let size = stream.read(&mut buffer).unwrap();
        assert_ne!(size, 0, "request headers incomplete");
        data.extend_from_slice(&buffer[..size]);
        if let Some(index) = data.windows(4).position(|v| v == b"\r\n\r\n") {
            break index + 4;
        }
        assert!(data.len() < 16384);
    };
    let head = std::str::from_utf8(&data[..boundary]).unwrap();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap().split_whitespace();
    let method = request_line.next().unwrap().to_owned();
    let target = request_line.next().unwrap().to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let length = headers
        .get("content-length")
        .map(|v| v.parse::<usize>().unwrap())
        .unwrap_or(0);
    while data.len() - boundary < length {
        let size = stream.read(&mut buffer).unwrap();
        assert_ne!(size, 0, "request body incomplete");
        data.extend_from_slice(&buffer[..size]);
    }
    Captured {
        method,
        target,
        headers,
        body: data[boundary..boundary + length].to_vec(),
    }
}

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ha-downloads"));
    for name in [
        "DOWNLOADS_URL",
        "DOWNLOADS_API_TOKEN",
        "DOWNLOADS_TOKEN_FILE",
        "HOME_ASSISTANT_URL",
        "HOME_ASSISTANT_TOKEN",
        "HOME_ASSISTANT_TOKEN_FILE",
    ] {
        command.env_remove(name);
    }
    command.env("DOWNLOADS_API_TOKEN", "native-fixture-token");
    command
}

fn native(fixture: &Fixture, args: &[&str]) -> Output {
    cli()
        .env("DOWNLOADS_URL", &fixture.origin)
        .args(args)
        .output()
        .unwrap()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    status: u16,
    etag: Option<String>,
    body: T,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    status: Option<u16>,
    error_code: String,
}

fn success<T: DeserializeOwned>(output: &Output) -> Envelope<T> {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn failure(output: &Output, exit: i32, code: &str) -> Failure {
    assert_eq!(output.status.code(), Some(exit));
    assert!(output.stdout.is_empty());
    let error: Failure = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error.error_code, code);
    error
}

fn input() -> CreateRequest {
    CreateRequest::Movie {
        tmdb_id: 14337.try_into().unwrap(),
        intent: MovieIntent::SearchNow {},
    }
}

fn record() -> RequestRecord {
    RequestRecord {
        id: REQUEST.parse::<RequestId>().unwrap(),
        created_at_unix_ms: 100,
        input: input(),
        submission: Submission::Pending,
    }
}

fn movie() -> MediaItem {
    MediaItem::Movie {
        tmdb_id: 14337.try_into().unwrap(),
        title: "Primer".into(),
        year: Some(2004),
        overview: "A time-travel film.".into(),
        poster: None,
        library: LibraryState::NotAdded,
        acquisition: Acquisition::NotRequested,
        request: RequestAvailability::Ready,
        metadata: None,
    }
}

fn series() -> MediaItem {
    MediaItem::Series {
        tvdb_id: 123.try_into().unwrap(),
        title: "Fixture Series".into(),
        year: Some(2026),
        overview: "A series.".into(),
        poster: None,
        library: LibraryState::NotAdded,
        acquisition: Acquisition::NotRequested,
        request: RequestAvailability::Ready,
        seasons: vec![],
        metadata: None,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

#[test]
fn native_system_preserves_typed_body_and_auth_without_fabricated_actor() {
    let body = SystemResponse {
        version: "fixture".into(),
        capabilities: Capabilities {
            read: true,
            request: true,
            admin: false,
        },
        transport: Transport::Native,
        credential_role: CredentialRole::Requester,
    };
    let fixture = Fixture::json(200, &body);
    let output = native(&fixture, &["system"]);
    let result = success::<SystemResponse>(&output);
    assert_eq!(result.status, 200);
    assert!(result.etag.is_none());
    assert_eq!(result.body.credential_role, CredentialRole::Requester);
    let calls = fixture.captured();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "GET");
    assert_eq!(calls[0].target, "/api/v1/system");
    assert_eq!(
        calls[0].headers.get("authorization").unwrap(),
        "Bearer native-fixture-token"
    );
    assert!(
        !calls[0]
            .headers
            .keys()
            .any(|key| key.starts_with("x-downloads-actor"))
    );
}

#[test]
fn ha_search_is_one_encoded_page_with_partial_coverage_and_selected_credential() {
    let mut lookup = movie();
    let MediaItem::Movie { request, .. } = &mut lookup else {
        panic!("movie fixture");
    };
    *request = RequestAvailability::ParentSetupRequired;
    let page = Page {
        items: vec![lookup],
        next_page: Some("opaque-next-page".into()),
        coverage: vec![SourceCoverage {
            source: Component::Radarr,
            state: CoverageState::Partial,
            observed_at_unix_ms: 100,
        }],
    };
    let fixture = Fixture::json(200, &page);
    let output = cli()
        .env("DOWNLOADS_URL", "http://127.0.0.1:9")
        .env("HOME_ASSISTANT_URL", &fixture.origin)
        .env("HOME_ASSISTANT_TOKEN", "ha-fixture-token")
        .args([
            "--transport",
            "home-assistant",
            "search",
            "Primer & other",
            "--kind",
            "movie",
            "--limit",
            "3",
            "--page-token",
            "opaque + token",
        ])
        .output()
        .unwrap();
    let result = success::<Page<MediaItem>>(&output);
    assert_eq!(result.body.coverage[0].state, CoverageState::Partial);
    assert_eq!(result.body.next_page.as_deref(), Some("opaque-next-page"));
    assert_eq!(result.body.items.len(), 1);
    assert!(matches!(
        &result.body.items[0],
        MediaItem::Movie {
            request: RequestAvailability::ParentSetupRequired,
            acquisition: Acquisition::NotRequested,
            ..
        }
    ));
    let calls = fixture.captured();
    assert_eq!(calls.len(), 1);
    let url = reqwest::Url::parse(&format!("{}{}", fixture.origin, calls[0].target)).unwrap();
    assert_eq!(url.path(), "/api/downloads/v1/search");
    let query = url.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
    assert_eq!(query.get("query").unwrap(), "Primer & other");
    assert_eq!(query.get("page_token").unwrap(), "opaque + token");
    assert_eq!(query.get("limit").unwrap(), "3");
    assert_eq!(
        calls[0].headers.get("authorization").unwrap(),
        "Bearer ha-fixture-token"
    );
    assert!(
        !calls[0]
            .headers
            .keys()
            .any(|key| key.starts_with("x-downloads-actor"))
    );
}

#[test]
fn caller_request_key_and_body_are_sent_once_without_preflight_lookup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("request.json");
    std::fs::write(&path, serde_json::to_vec(&input()).unwrap()).unwrap();
    let fixture = Fixture::json(202, &record());
    let output = cli()
        .env("DOWNLOADS_URL", &fixture.origin)
        .args([
            "requests",
            "create",
            "--idempotency-key",
            "caller-key_7",
            "--input",
        ])
        .arg(&path)
        .output()
        .unwrap();
    let result = success::<RequestRecord>(&output);
    assert_eq!(result.status, 202);
    assert!(matches!(result.body.submission, Submission::Pending));
    let calls = fixture.captured();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "POST");
    assert_eq!(calls[0].target, "/api/v1/requests");
    assert_eq!(
        calls[0].headers.get("idempotency-key").unwrap(),
        "caller-key_7"
    );
    assert_eq!(
        serde_json::from_slice::<CreateRequest>(&calls[0].body).unwrap(),
        input()
    );
}

#[test]
fn readiness_exit_uses_only_the_servers_operation_verdict_and_keeps_snapshot() {
    for (state, exit) in [(ReadinessState::Ready, 0), (ReadinessState::Blocked, 3)] {
        let now = now_ms();
        let body = HealthSnapshot {
            components: vec![ComponentHealth {
                component: Component::Plex,
                observation: Observation::Observed {
                    outcome: HealthOutcome::NotConfigured,
                    freshness: Freshness::Fresh,
                    observed_at_unix_ms: now,
                    expires_at_unix_ms: now + 60000,
                },
            }],
            readiness: vec![OperationReadiness {
                operation: Operation::RequestMovie,
                state,
                reasons: if state == ReadinessState::Ready {
                    vec![]
                } else {
                    vec![HealthIssueCode::ChecksStale]
                },
                expires_at_unix_ms: now + 60000,
            }],
            generated_at_unix_ms: now,
        };
        let fixture = Fixture::json(200, &body);
        let output = native(
            &fixture,
            &["health", "check", "--operation", "request_movie"],
        );
        assert_eq!(output.status.code(), Some(exit));
        assert!(output.stderr.is_empty());
        let result: Envelope<HealthSnapshot> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.body.readiness[0].state, state);
        let calls = fixture.captured();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].target.starts_with("/api/v1/health/stack?"));
        assert!(calls[0].target.contains("operation=request_movie"));
    }
}

#[test]
fn stale_future_and_ambiguous_ready_verdicts_exit_three_with_snapshot() {
    for case in ["stale", "future", "ambiguous", "wrong-operation"] {
        let now = now_ms();
        let mut body = HealthSnapshot {
            components: vec![],
            generated_at_unix_ms: now,
            readiness: vec![OperationReadiness {
                operation: Operation::RequestMovie,
                state: ReadinessState::Ready,
                reasons: vec![],
                expires_at_unix_ms: now + 60000,
            }],
        };
        match case {
            "stale" => body.readiness[0].expires_at_unix_ms = now - 1,
            "future" => body.generated_at_unix_ms = now + 60000,
            "ambiguous" => body.readiness.push(body.readiness[0].clone()),
            "wrong-operation" => body.readiness[0].operation = Operation::RequestSeries,
            _ => unreachable!(),
        }
        let fixture = Fixture::json(200, &body);
        let output = native(
            &fixture,
            &["health", "check", "--operation", "request_movie"],
        );
        assert_eq!(output.status.code(), Some(3), "{case}");
        assert!(output.stderr.is_empty());
        let actual: Envelope<HealthSnapshot> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(actual.body.readiness.len(), body.readiness.len());
        assert_eq!(fixture.captured().len(), 1);
    }
}

#[derive(Serialize)]
struct ServerError<'a> {
    error_code: ApiErrorCode,
    message: &'a str,
    request_id: &'a str,
}

#[test]
fn known_server_errors_are_typed_and_private_error_text_is_never_printed() {
    for (status, code) in [
        (401, ApiErrorCode::AuthenticationRequired),
        (403, ApiErrorCode::PermissionDenied),
        (409, ApiErrorCode::IdempotencyConflict),
    ] {
        let fixture = Fixture::json(
            status,
            &ServerError {
                error_code: code,
                message: "private-upstream-secret",
                request_id: "private-request-id",
            },
        );
        let output = native(&fixture, &["system"]);
        assert_eq!(failure(&output, 1, &code.to_string()).status, Some(status));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private"));
        assert_eq!(fixture.captured().len(), 1);
    }
}

#[test]
fn redirects_are_not_followed_and_errors_are_not_retried() {
    let destination = Fixture::json(200, &record());
    let redirect = Fixture::response(
        302,
        b"private redirect body".to_vec(),
        &[("Location", &destination.origin)],
        Duration::ZERO,
    );
    let output = native(&redirect, &["requests", "get", REQUEST]);
    failure(&output, 1, "redirect_refused");
    assert_eq!(redirect.captured().len(), 1);
    assert!(destination.captured().is_empty());
    let unavailable = Fixture::json(
        503,
        &ServerError {
            error_code: ApiErrorCode::UpstreamUnavailable,
            message: "secret",
            request_id: "secret",
        },
    );
    failure(
        &native(&unavailable, &["system"]),
        1,
        "upstream_unavailable",
    );
    assert_eq!(unavailable.captured().len(), 1);
}

#[test]
fn ambiguous_transport_bad_origins_and_missing_token_file_do_not_make_requests() {
    let fixture = Fixture::json(200, &record());
    let output = cli()
        .env("DOWNLOADS_URL", &fixture.origin)
        .env("HOME_ASSISTANT_URL", &fixture.origin)
        .args(["system"])
        .output()
        .unwrap();
    failure(&output, 1, "select_transport");
    for url in [
        "http://user:private-password@localhost/",
        "http://localhost/api/v1",
        "http://localhost/?secret=private",
        "file:///private",
    ] {
        let output = cli()
            .args(["--transport", "native", "--url", url, "system"])
            .output()
            .unwrap();
        failure(&output, 1, "invalid_url");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private"));
    }
    let directory = tempfile::tempdir().unwrap();
    let output = cli()
        .env("DOWNLOADS_URL", &fixture.origin)
        .args(["--token-file"])
        .arg(directory.path().join("missing"))
        .arg("system")
        .output()
        .unwrap();
    failure(&output, 1, "credential_read_failed");
    assert!(fixture.captured().is_empty());
}

#[test]
fn strict_write_inputs_and_local_limits_are_rejected_before_http() {
    let fixture = Fixture::json(202, &record());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.json");
    let valid = serde_json::to_string(&input()).unwrap();
    for bytes in [
        b"not-json".to_vec(),
        valid
            .replacen(
                '{',
                r#"{"profile":{"id":"276319ec-c797-460c-a49e-6b90fc1f6ae8","revision":1},"#,
                1,
            )
            .into_bytes(),
        valid
            .replace(
                "\"mode\":\"search_now\"",
                "\"mode\":\"search_now\",\"force\":true",
            )
            .into_bytes(),
        vec![b' '; 1024 * 1024 + 1],
    ] {
        let expected = if bytes.len() > 1024 * 1024 {
            "input_too_large"
        } else {
            "invalid_json_input"
        };
        std::fs::write(&path, bytes).unwrap();
        let output = cli()
            .env("DOWNLOADS_URL", &fixture.origin)
            .args([
                "requests",
                "create",
                "--idempotency-key",
                "caller-key",
                "--input",
            ])
            .arg(&path)
            .output()
            .unwrap();
        failure(&output, 1, expected);
    }
    for args in [
        vec!["movies", "list", "--limit", "101"],
        vec!["movies", "get", "0"],
        vec!["requests", "create"],
    ] {
        let output = native(&fixture, &args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: ha_downloads::cli::StatusError = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error.error, ha_downloads::cli::ErrorCode::Configuration);
    }
    assert!(fixture.captured().is_empty());
}

#[test]
fn removed_settings_commands_and_profile_target_argument_are_rejected_before_http() {
    let fixture = Fixture::json(200, &record());
    for args in [
        vec!["profiles", "list"],
        vec!["profiles", "get", REQUEST],
        vec!["profiles", "create", "--idempotency-key", "caller-key"],
        vec!["profiles", "replace", REQUEST, "--if-match", "\"1\""],
        vec!["managers", "options", "radarr"],
        vec![
            "health",
            "check",
            "--operation",
            "request_movie",
            "--profile-id",
            REQUEST,
        ],
    ] {
        let output = native(&fixture, &args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: ha_downloads::cli::StatusError = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error.error, ha_downloads::cli::ErrorCode::Configuration);
        assert_eq!(error.message, "Invalid arguments. Use --help for usage.");
    }
    assert!(fixture.captured().is_empty());
}

#[test]
fn home_assistant_request_sends_profileless_intent_with_the_callers_uuid() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("request.json");
    std::fs::write(&path, serde_json::to_vec(&input()).unwrap()).unwrap();
    let fixture = Fixture::json(202, &record());
    let output = cli()
        .env("HOME_ASSISTANT_URL", &fixture.origin)
        .env("HOME_ASSISTANT_TOKEN", "ha-fixture-token")
        .args([
            "--transport",
            "home-assistant",
            "requests",
            "create",
            "--idempotency-key",
            REQUEST,
            "--input",
        ])
        .arg(&path)
        .output()
        .unwrap();
    success::<RequestRecord>(&output);
    let calls = fixture.captured();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "POST");
    assert_eq!(calls[0].target, "/api/downloads/v1/requests");
    assert_eq!(calls[0].headers.get("idempotency-key").unwrap(), REQUEST);
    assert_eq!(
        serde_json::from_slice::<CreateRequest>(&calls[0].body).unwrap(),
        input()
    );
}

#[test]
fn response_size_typed_decoding_and_total_deadline_are_enforced() {
    for (body, expected) in [
        (b"{}".to_vec(), "invalid_json_response_outcome_unknown"),
        (
            vec![b' '; 8 * 1024 * 1024 + 1],
            "response_too_large_outcome_unknown",
        ),
    ] {
        let fixture = Fixture::response(200, body, &[], Duration::ZERO);
        failure(&native(&fixture, &["system"]), 1, expected);
        assert_eq!(fixture.captured().len(), 1);
    }
    let fixture = Fixture::response(200, b"{}".to_vec(), &[], Duration::from_millis(1200));
    failure(
        &native(&fixture, &["--timeout", "1", "system"]),
        1,
        "request_timeout_outcome_unknown",
    );
    assert_eq!(fixture.captured().len(), 1);
}

fn read_route<T: Serialize + DeserializeOwned>(args: &[&str], body: &T, expected: &str) {
    let fixture = Fixture::json(200, body);
    success::<T>(&native(&fixture, args));
    let calls = fixture.captured();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "GET");
    assert_eq!(calls[0].target, expected);
}

#[test]
fn remaining_read_commands_use_the_documented_single_request_routes() {
    read_route(
        &["managers", "list"],
        &ManagerList { items: vec![] },
        "/api/v1/managers",
    );
    let mut managed = movie();
    let MediaItem::Movie { library, .. } = &mut managed else {
        panic!("movie fixture");
    };
    *library = LibraryState::Managed { upstream_id: 23 };
    let fixture = Fixture::json(200, &managed);
    let result = success::<MediaItem>(&native(&fixture, &["movies", "get", "14337"]));
    assert!(matches!(
        result.body,
        MediaItem::Movie {
            library: LibraryState::Managed { upstream_id: 23 },
            request: RequestAvailability::Ready,
            ..
        }
    ));
    let calls = fixture.captured();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "GET");
    assert_eq!(calls[0].target, "/api/v1/movies/14337");
    read_route(
        &["movies", "list", "--limit", "1"],
        &Page::<MediaItem> {
            items: vec![movie()],
            next_page: None,
            coverage: vec![],
        },
        "/api/v1/movies?limit=1",
    );
    read_route(&["series", "get", "123"], &series(), "/api/v1/series/123");
    read_route(
        &["series", "list"],
        &Page::<MediaItem> {
            items: vec![],
            next_page: None,
            coverage: vec![],
        },
        "/api/v1/series",
    );
    read_route(
        &["series", "episodes", "123", "--season", "2"],
        &Page::<ha_downloads::media::model::Episode> {
            items: vec![],
            next_page: None,
            coverage: vec![],
        },
        "/api/v1/series/123/episodes?season=2",
    );
    read_route(
        &["requests", "get", REQUEST],
        &record(),
        &format!("/api/v1/requests/{REQUEST}"),
    );
    read_route(
        &["requests", "lookup", "--idempotency-key", "original-key"],
        &record(),
        "/api/v1/request-keys/original-key",
    );
    read_route(
        &["requests", "list"],
        &Page::<RequestRecord> {
            items: vec![],
            next_page: None,
            coverage: vec![],
        },
        "/api/v1/requests",
    );
    read_route(
        &["queue", "list"],
        &Page::<ha_downloads::media::model::QueueItem> {
            items: vec![],
            next_page: None,
            coverage: vec![],
        },
        "/api/v1/queue",
    );
    read_route(
        &["health", "component", "vpn"],
        &ComponentHealth {
            component: Component::Vpn,
            observation: Observation::NeverChecked,
        },
        "/api/v1/health/components/vpn",
    );
    read_route(
        &["health"],
        &HealthSnapshot {
            components: vec![],
            readiness: vec![],
            generated_at_unix_ms: 100,
        },
        "/api/v1/health/stack",
    );
}

#[test]
fn queue_details_preserve_typed_observations_in_both_transports() {
    let body = DownloadDetails {
        id: "radarr:12".parse().unwrap(),
        manager: ManagerId::Radarr,
        media: MediaIdentity::Movie {
            tmdb_id: 14337.try_into().unwrap(),
        },
        title: "Example.Release.2160p".into(),
        episode: None,
        acquisition: Acquisition::Waiting,
        status: DownloadStatus::Queued,
        quality: None,
        progress: None,
        remaining_seconds: None,
        download_client: Some("SABnzbd".into()),
        indexer: None,
        protocol: Some(DownloadProtocol::Usenet),
        import_status: DownloadImportStatus::NotStarted,
        warnings: vec![],
        languages: vec!["English".into()],
        custom_formats: vec![],
        custom_format_score: None,
        added_at: Some("2026-11-07T20:00:00Z".into()),
        estimated_completion_at: None,
        observed_at_unix_ms: 100,
    };
    for ha in [false, true] {
        let fixture = Fixture::json(200, &body);
        let output = if ha {
            cli()
                .env("HOME_ASSISTANT_URL", &fixture.origin)
                .env("HOME_ASSISTANT_TOKEN", "ha-fixture-token")
                .args(["--transport", "home-assistant", "queue", "get", "radarr:12"])
                .output()
                .unwrap()
        } else {
            native(&fixture, &["queue", "get", "radarr:12"])
        };
        let result = success::<DownloadDetails>(&output);
        assert_eq!(result.body.id, body.id);
        assert_eq!(result.body.status, DownloadStatus::Queued);
        assert_eq!(result.body.languages, ["English"]);
        let calls = fixture.captured();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, "GET");
        assert_eq!(
            calls[0].target,
            if ha {
                "/api/downloads/v1/queue/radarr:12"
            } else {
                "/api/v1/queue/radarr:12"
            }
        );
    }
    let fixture = Fixture::json(200, &body);
    for id in [
        "radarr:0",
        "sonarr:01",
        "radarr:+1",
        "prowlarr:12",
        "radarr:2147483648",
        "radarr:12/stop",
    ] {
        let output = native(&fixture, &["queue", "get", id]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    assert!(fixture.captured().is_empty());
}

#[test]
fn native_imports_are_typed_paginated_reads_with_strict_since_dates() {
    let body = Page {
        items: vec![ImportedItem {
            id: "radarr:31".parse().unwrap(),
            manager: ManagerId::Radarr,
            media: MediaIdentity::Movie {
                tmdb_id: 14337.try_into().unwrap(),
            },
            title: "Example import".into(),
            episode: None,
            imported_at: "2026-11-07T20:00:00Z".into(),
            quality: None,
            plex: PlexImportStatus::NotConfigured,
            subtitles: SubtitleImportStatus::Unknown {
                reason: SubtitleUnknownReason::PlexUnverified,
            },
            observed_at_unix_ms: 100,
        }],
        next_page: Some("scoped-next-page".into()),
        coverage: vec![],
    };
    let fixture = Fixture::json(200, &body);
    let output = native(
        &fixture,
        &[
            "imports",
            "list",
            "--since",
            "2026-11-07T19:00:00Z",
            "--limit",
            "3",
        ],
    );
    let result = success::<Page<ImportedItem>>(&output);
    assert_eq!(result.body.items[0].id.to_string(), "radarr:31");
    assert_eq!(result.body.next_page.as_deref(), Some("scoped-next-page"));
    let calls = fixture.captured();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "GET");
    let url = reqwest::Url::parse(&format!("{}{}", fixture.origin, calls[0].target)).unwrap();
    assert_eq!(url.path(), "/api/v1/imports");
    let query = url.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
    assert_eq!(query.get("since").unwrap(), "2026-11-07T19:00:00Z");
    assert_eq!(query.get("limit").unwrap(), "3");
    drop(calls);
    for since in [
        "not-time",
        "2025-02-29T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-01-01T00:00:00+01:00",
        "2026-+1-01T00:00:00Z",
    ] {
        assert!(
            !native(&fixture, &["imports", "list", "--since", since])
                .status
                .success()
        );
    }
    assert_eq!(fixture.captured().len(), 1);
}

#[test]
fn legacy_help_doctor_and_local_status_preserve_the_published_contract() {
    for args in [
        vec![],
        vec!["--help"],
        vec!["status", "--help"],
        vec!["doctor", "--help"],
    ] {
        let output = cli()
            .env("HOME_ASSISTANT_URL", "private-not-an-origin")
            .env("DOWNLOADS_URL", "private-not-an-origin")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("ha-downloads"));
        assert!(!text.contains("private-not-an-origin"));
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("status.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&ha_downloads::status::Status::needs_configuration(
            "Configure the VPN.",
        ))
        .unwrap(),
    )
    .unwrap();
    for command in ["status", "doctor"] {
        let output = cli()
            .env("HOME_ASSISTANT_URL", "private-unused-origin")
            .env("HOME_ASSISTANT_TOKEN", "private-unused-token")
            .args([command, "--status-file"])
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stderr.is_empty());
        let report: ha_downloads::cli::StatusReport =
            serde_json::from_slice(&output.stdout).unwrap();
        match report {
            ha_downloads::cli::StatusReport::Native { health, status } => {
                assert!(!health.ready);
                assert_eq!(
                    status.state,
                    ha_downloads::status::State::NeedsConfiguration
                );
            }
            ha_downloads::cli::StatusReport::HomeAssistant { .. } => {
                panic!("legacy default transport changed")
            }
        }
    }
}

fn remote_health() -> ha_downloads::status::Health {
    ha_downloads::status::Health {
        ready: true,
        state: ha_downloads::status::State::Ready,
        reasons: vec![],
        checked_at_unix_ms: Some(9_999_999_999_999),
        age_ms: Some(0),
        max_age_ms: 30000,
        last_failure: None,
    }
}

#[test]
fn legacy_native_status_uses_token_file_server_clock_and_report_without_media_envelope() {
    let directory = tempfile::tempdir().unwrap();
    let token = directory.path().join("token");
    std::fs::write(&token, "legacy-file-token").unwrap();
    let body = ha_downloads::status::StatusSnapshot {
        status: ha_downloads::status::Status::ready("fixture", None, true, true),
        health: remote_health(),
    };
    let fixture = Fixture::json(200, &body);
    let output = cli()
        .args(["status", "--url", &fixture.origin, "--token-file"])
        .arg(&token)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let report: ha_downloads::cli::StatusReport = serde_json::from_slice(&output.stdout).unwrap();
    match report {
        ha_downloads::cli::StatusReport::Native { health, .. } => {
            assert!(health.ready);
            assert_eq!(health.checked_at_unix_ms, Some(9_999_999_999_999));
        }
        ha_downloads::cli::StatusReport::HomeAssistant { .. } => panic!("wrong status transport"),
    }
    let calls = fixture.captured();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].target, "/api/v1/status");
    assert_eq!(
        calls[0].headers.get("authorization").unwrap(),
        "Bearer legacy-file-token"
    );
    assert_eq!(calls[0].headers.get("cache-control").unwrap(), "no-cache");
}

#[test]
fn legacy_ha_status_preserves_cached_safe_health_endpoint_and_output() {
    let fixture = Fixture::json(200, &remote_health());
    let output = cli()
        .env("HOME_ASSISTANT_URL", &fixture.origin)
        .env("HOME_ASSISTANT_TOKEN", "legacy-ha-token")
        .args(["--transport", "home-assistant", "status"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let report: ha_downloads::cli::StatusReport = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        matches!(report, ha_downloads::cli::StatusReport::HomeAssistant { health } if health.ready)
    );
    let calls = fixture.captured();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].target, "/api/downloads/health");
    assert_eq!(
        calls[0].headers.get("authorization").unwrap(),
        "Bearer legacy-ha-token"
    );
}

#[test]
fn legacy_status_does_not_fall_back_to_media_environment_token() {
    let fixture = Fixture::json(200, &remote_health());
    let output = cli()
        .env("DOWNLOADS_URL", &fixture.origin)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: ha_downloads::cli::StatusError = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error.error, ha_downloads::cli::ErrorCode::Configuration);
    assert!(fixture.captured().is_empty());
}

#[test]
fn ha_media_requires_https_outside_loopback_without_sending_credentials() {
    for origin in [
        "http://192.0.2.1/",
        "http://ha.example/",
        "http://[2001:db8::1]/",
    ] {
        let output = cli()
            .env("HOME_ASSISTANT_URL", origin)
            .env("HOME_ASSISTANT_TOKEN", "private-ha-token")
            .args(["--transport", "home-assistant", "system"])
            .output()
            .unwrap();
        failure(&output, 1, "invalid_url");
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("private-ha-token"));
        assert!(!text.contains(origin));
    }
}

#[test]
fn ha_aggregate_health_uses_the_new_stack_route_without_replacing_legacy_health() {
    let fixture = Fixture::json(
        200,
        &HealthSnapshot {
            components: vec![],
            readiness: vec![],
            generated_at_unix_ms: now_ms(),
        },
    );
    let output = cli()
        .env("HOME_ASSISTANT_URL", &fixture.origin)
        .env("HOME_ASSISTANT_TOKEN", "ha-media-token")
        .args(["--transport", "home-assistant", "health"])
        .output()
        .unwrap();
    success::<HealthSnapshot>(&output);
    let calls = fixture.captured();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].target, "/api/downloads/v1/health/stack");
    assert_eq!(
        calls[0].headers.get("authorization").unwrap(),
        "Bearer ha-media-token"
    );
}

#[test]
fn selected_transport_never_uses_the_other_token_and_file_override_is_exact() {
    let body = SystemResponse {
        version: "fixture".into(),
        capabilities: Capabilities {
            read: true,
            request: false,
            admin: false,
        },
        transport: Transport::Native,
        credential_role: CredentialRole::ReadOnly,
    };
    let fixture = Fixture::json(200, &body);
    let missing = cli()
        .env("HOME_ASSISTANT_URL", &fixture.origin)
        .args(["system"])
        .output()
        .unwrap();
    failure(&missing, 1, "missing_token");
    assert!(fixture.captured().is_empty());
    let directory = tempfile::tempdir().unwrap();
    let token = directory.path().join("token");
    std::fs::write(&token, "file-fixture-token\r\n").unwrap();
    let output = cli()
        .env("DOWNLOADS_URL", &fixture.origin)
        .env(
            "DOWNLOADS_TOKEN_FILE",
            directory.path().join("must-not-be-read"),
        )
        .arg("--token-file")
        .arg(&token)
        .arg("system")
        .output()
        .unwrap();
    success::<SystemResponse>(&output);
    let calls = fixture.captured();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].headers.get("authorization").unwrap(),
        "Bearer file-fixture-token"
    );
}
