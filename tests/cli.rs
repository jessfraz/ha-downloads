use ha_downloads::{
    monitor::{Check, Checks, Outcome},
    status::{State, Status},
};
use std::process::Command;

fn ready_status() -> Status {
    let mut status = Status::ready("test", None, true, true);
    let passed = Check {
        outcome: Outcome::Passed,
        ..Check::default()
    };
    status.diagnostics.checks = Checks {
        vpn_service: passed,
        vpn_health: passed,
        dns_service: passed,
        dns_lookup: passed,
        resolver: passed,
        firewall: passed,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis()
        .try_into()
        .expect("timestamp");
    status.diagnostics.checked_at_unix_ms = Some(now);
    status.diagnostics.downloader_check = passed;
    status.diagnostics.downloader_checked_at_unix_ms = Some(now);
    status
}

#[test]
fn doctor_reads_the_real_status_contract_and_reports_readiness_by_exit_code() {
    let directory = tempfile::tempdir().expect("status directory");
    let path = directory.path().join("status.json");
    for (status, expected) in [
        (Status::needs_configuration("Configure the VPN."), 2),
        (ready_status(), 0),
        (Status::degraded("test", "Tunnel unavailable.", true), 2),
    ] {
        std::fs::write(&path, serde_json::to_vec(&status).expect("encode")).expect("write status");
        let output = Command::new(env!("CARGO_BIN_EXE_downloads"))
            .args(["doctor", "--status-file"])
            .arg(&path)
            .output()
            .expect("doctor");
        assert_eq!(output.status.code(), Some(expected));
        let actual: Status =
            serde_json::from_slice(&output.stdout).expect("typed diagnostic output");
        assert_eq!(actual.state, status.state);
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn doctor_rejects_credential_urls_without_echoing_the_secret() {
    let output = Command::new(env!("CARGO_BIN_EXE_downloads"))
        .args(["doctor", "--url", "http://user:private-password@localhost/"])
        .output()
        .expect("doctor");
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).expect("error");
    assert!(error.contains("without credentials"));
    assert!(!error.contains("private-password"));
    assert!(output.stdout.is_empty());
}

#[test]
fn malformed_local_status_is_a_diagnostic_error() {
    let directory = tempfile::tempdir().expect("status directory");
    let path = directory.path().join("status.json");
    std::fs::write(&path, b"not-status").expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_downloads"))
        .args(["doctor", "--status-file"])
        .arg(&path)
        .output()
        .expect("doctor");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .expect("error")
            .contains("Local app status is invalid")
    );
}

#[test]
fn status_serialization_keeps_unknown_queue_state_unknown() {
    let status = Status::ready("test", None, true, true);
    let restored: Status =
        serde_json::from_slice(&serde_json::to_vec(&status).expect("encode")).expect("decode");
    assert_eq!(restored.state, State::Ready);
    assert_eq!(restored.downloader.paused, None);
}

#[test]
fn implicit_serve_uses_the_same_environment_validation_as_explicit_serve() {
    use std::time::{Duration, Instant};
    for args in [&[][..], &["serve"][..]] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_downloads"))
            .args(args)
            .env("DOWNLOADS_API_LISTEN", "not-an-address")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start CLI");
        let deadline = Instant::now() + Duration::from_secs(5);
        let code = loop {
            if let Some(status) = child.try_wait().expect("status") {
                break status.code();
            }
            if Instant::now() >= deadline {
                child.kill().expect("stop stuck CLI");
                child.wait().expect("reap CLI");
                panic!("CLI ignored its environment validation");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(code, Some(1));
    }
}

#[test]
fn doctor_does_not_accept_a_ready_label_without_current_checks() {
    let directory = tempfile::tempdir().expect("status directory");
    let path = directory.path().join("status.json");
    let status = Status::ready("test", None, true, true);
    std::fs::write(&path, serde_json::to_vec(&status).expect("encode")).expect("write status");
    let output = Command::new(env!("CARGO_BIN_EXE_downloads"))
        .args(["doctor", "--status-file"])
        .arg(path)
        .output()
        .expect("doctor");
    assert_eq!(output.status.code(), Some(2));
    let actual: Status = serde_json::from_slice(&output.stdout).expect("typed status");
    assert_eq!(actual.state, State::Ready);
}

#[tokio::test]
async fn remote_doctor_uses_the_servers_readiness_despite_clock_skew() {
    use ha_downloads::status::Health;
    use serde::Serialize;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    #[derive(Serialize)]
    struct RemoteStatus {
        #[serde(flatten)]
        status: Status,
        health: Health,
    }
    let mut status = ready_status();
    let mut health = status.health();
    let server_time = 9_999_999_999_999;
    status.diagnostics.checked_at_unix_ms = Some(server_time);
    status.diagnostics.downloader_checked_at_unix_ms = Some(server_time);
    health.checked_at_unix_ms = Some(server_time);
    health.age_ms = Some(0);
    let body = serde_json::to_vec(&RemoteStatus { status, health }).expect("response");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}/", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut request = [0; 4096];
        let count = stream.read(&mut request).await.expect("request");
        let request = std::str::from_utf8(&request[..count]).expect("request text");
        assert!(request.starts_with("GET /api/v1/status "));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-token")
        );
        stream.write_all(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ).as_bytes()).await.expect("headers");
        stream.write_all(&body).await.expect("body");
    });
    let directory = tempfile::tempdir().expect("token directory");
    let path = directory.path().join("token");
    std::fs::write(&path, "fixture-token").expect("fixture credential");
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_downloads"))
        .args(["doctor", "--url", &url, "--token-file"])
        .arg(path)
        .env("NO_PROXY", "127.0.0.1")
        .output()
        .await
        .expect("doctor");
    server.await.expect("HTTP fixture");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let report: ha_downloads::cli::StatusReport =
        serde_json::from_slice(&output.stdout).expect("typed status and health");
    match report {
        ha_downloads::cli::StatusReport::Native { health, .. } => {
            assert!(health.ready);
            assert_eq!(health.checked_at_unix_ms, Some(server_time));
            assert_eq!(health.age_ms, Some(0));
        }
        ha_downloads::cli::StatusReport::HomeAssistant { .. } => panic!("wrong transport"),
    }
}

#[test]
fn native_cli_help_is_offline_and_never_prints_environment_credentials() {
    for args in [&[][..], &["--help"][..], &["status", "--help"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_ha-downloads"))
            .args(args)
            .env("HOME_ASSISTANT_URL", "not-an-origin")
            .env("HOME_ASSISTANT_TOKEN", "fixture-private-value")
            .env(
                "DOWNLOADS_URL",
                "http://user:fixture-private-value@invalid.example/",
            )
            .output()
            .expect("help");
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).expect("help text");
        assert!(text.contains("ha-downloads"));
        assert!(!text.contains("fixture-private-value"));
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn argument_errors_never_echo_credential_like_arguments() {
    for binary in [
        env!("CARGO_BIN_EXE_ha-downloads"),
        env!("CARGO_BIN_EXE_downloads"),
    ] {
        for args in [
            &["--transport", "fixture-private-value", "status"][..],
            &["status", "--fixture-private-value"][..],
        ] {
            let output = Command::new(binary)
                .args(args)
                .output()
                .expect("invalid arguments");
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert!(
                !String::from_utf8(output.stderr)
                    .expect("redacted error")
                    .contains("fixture-private-value")
            );
        }
    }
}

#[test]
fn home_assistant_requires_tls_outside_loopback() {
    let output = Command::new(env!("CARGO_BIN_EXE_ha-downloads"))
        .args(["--transport", "home-assistant", "status"])
        .env("HOME_ASSISTANT_URL", "http://192.0.2.1/")
        .env("HOME_ASSISTANT_TOKEN", "fixture-private-value")
        .env_remove("DOWNLOADS_URL")
        .env_remove("DOWNLOADS_TOKEN_FILE")
        .output()
        .expect("insecure origin");
    let error: ha_downloads::cli::StatusError =
        serde_json::from_slice(&output.stderr).expect("typed error");
    assert_eq!(error.error, ha_downloads::cli::ErrorCode::Configuration);
    assert!(error.message.contains("HTTPS"));
}

#[test]
fn native_cli_outputs_computed_health_and_typed_redacted_errors() {
    use ha_downloads::cli::{ErrorCode, StatusError, StatusReport};
    let directory = tempfile::tempdir().expect("status directory");
    let path = directory.path().join("status.json");
    let status = ready_status();
    std::fs::write(&path, serde_json::to_vec(&status).expect("encode")).expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_ha-downloads"))
        .args(["status", "--status-file"])
        .arg(&path)
        .env("HOME_ASSISTANT_URL", "unused-invalid-origin")
        .env("HOME_ASSISTANT_TOKEN", "fixture-private-value")
        .output()
        .expect("status");
    assert!(output.status.success());
    let report: StatusReport = serde_json::from_slice(&output.stdout).expect("typed report");
    match report {
        StatusReport::Native {
            health,
            status: snapshot,
        } => {
            assert!(health.ready);
            assert!(health.reasons.is_empty());
            assert_eq!(snapshot.state, status.state);
        }
        StatusReport::HomeAssistant { .. } => panic!("wrong transport"),
    }
    let output = Command::new(env!("CARGO_BIN_EXE_ha-downloads"))
        .args([
            "status",
            "--url",
            "https://user:fixture-private-value@invalid.example/",
            "--token-file",
        ])
        .arg(directory.path().join("missing"))
        .output()
        .expect("invalid URL");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: StatusError = serde_json::from_slice(&output.stderr).expect("typed error");
    assert_eq!(error.error, ErrorCode::Configuration);
    assert!(
        !String::from_utf8(output.stderr)
            .expect("error")
            .contains("fixture-private-value")
    );
}

#[tokio::test]
async fn home_assistant_cli_rejects_missing_health_without_leaking_response_bodies() {
    use ha_downloads::cli::{ErrorCode, StatusError};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}/", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut request = [0; 4096];
        let count = stream.read(&mut request).await.expect("request");
        let request = std::str::from_utf8(&request[..count]).expect("request text");
        assert!(request.starts_with("GET /api/downloads/health "));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-ha-token")
        );
        let body = r#"{"ready":true,"state":"ready","reasons":[],"max_age_ms":30000,"private":"fixture-private-response"}"#;
        stream.write_all(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
        ).as_bytes()).await.expect("response");
    });
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ha-downloads"))
        .args(["--transport", "home-assistant", "status"])
        .env("HOME_ASSISTANT_URL", url)
        .env("HOME_ASSISTANT_TOKEN", "fixture-ha-token")
        .env("NO_PROXY", "127.0.0.1")
        .env_remove("DOWNLOADS_URL")
        .env_remove("DOWNLOADS_TOKEN_FILE")
        .output()
        .await
        .expect("HA status");
    server.await.expect("HTTP fixture");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: StatusError = serde_json::from_slice(&output.stderr).expect("typed error");
    assert_eq!(error.error, ErrorCode::InvalidResponse);
    let text = String::from_utf8(output.stderr).expect("error");
    assert!(!text.contains("fixture-private-response"));
    assert!(!text.contains("fixture-ha-token"));
}
