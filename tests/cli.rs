use ha_downloads::status::{State, Status};
use std::process::Command;

#[test]
fn doctor_reads_the_real_status_contract_and_reports_readiness_by_exit_code() {
    let directory = tempfile::tempdir().expect("status directory");
    let path = directory.path().join("status.json");
    for (status, expected) in [
        (Status::needs_configuration("Configure the VPN."), 2),
        (Status::ready("test", None, true, true), 0),
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
        assert_eq!(code, Some(2));
    }
}
