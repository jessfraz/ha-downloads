//! Transmission shares Gluetun's namespace; the runtime owns its child lifecycle.
use crate::{
    config::TorrentConfig,
    monitor::{Check, Outcome},
    vpn::request_failure,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::CStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
    process::Stdio,
    time::Instant,
};
use tokio::process::Command;
use zeroize::Zeroizing;

const SETTINGS_LIMIT: usize = 1024 * 1024;
const RESPONSE_LIMIT: usize = 64 * 1024;
const SESSION_HEADER: &str = "x-transmission-session-id";

#[derive(Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
struct Settings {
    #[serde(alias = "download_dir")]
    download_dir: String,
    #[serde(alias = "incomplete_dir")]
    incomplete_dir: String,
    #[serde(alias = "incomplete_dir_enabled")]
    incomplete_dir_enabled: bool,
    #[serde(alias = "rpc_enabled")]
    rpc_enabled: bool,
    #[serde(alias = "rpc_bind_address")]
    rpc_bind_address: String,
    #[serde(alias = "rpc_port")]
    rpc_port: u16,
    #[serde(alias = "rpc_url")]
    rpc_url: String,
    #[serde(alias = "rpc_authentication_required")]
    rpc_authentication_required: bool,
    #[serde(alias = "rpc_username")]
    rpc_username: String,
    #[serde(
        alias = "rpc_password",
        serialize_with = "serialize_password",
        deserialize_with = "deserialize_password"
    )]
    rpc_password: Zeroizing<String>,
    #[serde(alias = "rpc_whitelist_enabled")]
    rpc_whitelist_enabled: bool,
    #[serde(alias = "rpc_whitelist")]
    rpc_whitelist: String,
    #[serde(alias = "rpc_host_whitelist_enabled")]
    rpc_host_whitelist_enabled: bool,
    #[serde(alias = "rpc_host_whitelist")]
    rpc_host_whitelist: String,
    #[serde(alias = "port_forwarding_enabled")]
    port_forwarding_enabled: bool,
    #[serde(alias = "lpd_enabled")]
    lpd_enabled: bool,
    #[serde(alias = "script_torrent_added_enabled")]
    script_torrent_added_enabled: bool,
    #[serde(alias = "script_torrent_done_enabled")]
    script_torrent_done_enabled: bool,
    #[serde(alias = "script_torrent_done_seeding_enabled")]
    script_torrent_done_seeding_enabled: bool,
    #[serde(alias = "peer_limit_global")]
    peer_limit_global: u32,
    #[serde(alias = "peer_limit_per_torrent")]
    peer_limit_per_torrent: u32,
    #[serde(alias = "download_queue_enabled")]
    download_queue_enabled: bool,
    #[serde(alias = "download_queue_size")]
    download_queue_size: u32,
    #[serde(alias = "seed_ratio_limited")]
    ratio_limit_enabled: bool,
    #[serde(alias = "seed_ratio_limit")]
    ratio_limit: f64,
    #[serde(alias = "speed_limit_up_enabled")]
    speed_limit_up_enabled: bool,
    #[serde(alias = "speed_limit_up")]
    speed_limit_up: u32,
    #[serde(alias = "alt_speed_enabled")]
    alt_speed_enabled: bool,
    #[serde(alias = "alt_speed_up")]
    alt_speed_up: u32,
    // Transmission owns queue, seeding and other native options outside this adapter.
    #[serde(flatten)]
    native: BTreeMap<String, serde_json::Value>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: "/media/downloads/torrents".into(),
            incomplete_dir: "/media/downloads/torrents/incomplete".into(),
            incomplete_dir_enabled: true,
            rpc_enabled: true,
            rpc_bind_address: "127.0.0.1".into(),
            rpc_port: 8082,
            rpc_url: "/transmission/".into(),
            rpc_authentication_required: true,
            rpc_username: String::new(),
            rpc_password: Zeroizing::new(String::new()),
            rpc_whitelist_enabled: true,
            rpc_whitelist: "127.0.0.1".into(),
            rpc_host_whitelist_enabled: true,
            rpc_host_whitelist: "localhost".into(),
            port_forwarding_enabled: false,
            lpd_enabled: false,
            script_torrent_added_enabled: false,
            script_torrent_done_enabled: false,
            script_torrent_done_seeding_enabled: false,
            peer_limit_global: 60,
            peer_limit_per_torrent: 30,
            download_queue_enabled: true,
            download_queue_size: 2,
            ratio_limit_enabled: true,
            ratio_limit: 0.0,
            speed_limit_up_enabled: true,
            speed_limit_up: 1,
            alt_speed_enabled: false,
            alt_speed_up: 1,
            native: BTreeMap::new(),
        }
    }
}
fn serialize_password<S: serde::Serializer>(
    password: &Zeroizing<String>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(password)
}
fn deserialize_password<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Zeroizing<String>, D::Error> {
    String::deserialize(deserializer).map(Zeroizing::new)
}

/// Called only while the owned Transmission child is stopped. Keep native
/// preferences, replacing only the loopback RPC boundary and configured login.
pub(crate) fn prepare(config: &TorrentConfig, directory: &Path) -> Result<()> {
    if !config.enabled {
        return Ok(());
    }
    ensure!(
        !config.username.is_empty()
            && !config.username.contains(':')
            && !config.username.chars().any(char::is_control)
            && !config.password.is_empty()
            && !config.password.starts_with('{')
            && !config.password.chars().any(char::is_control),
        "Invalid torrent RPC credentials"
    );
    match fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("Create private torrent directory"),
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory)
        .context("Open private torrent directory")?;
    let metadata = directory.metadata()?;
    // SAFETY: geteuid has no arguments and cannot fail.
    let owner = unsafe { libc::geteuid() };
    ensure!(
        metadata.is_dir() && metadata.uid() == owner && metadata.mode() & 0o7777 == 0o700,
        "Torrent directory must be owned and private"
    );
    let mut settings = match open_at(&directory, c"settings.json", libc::O_RDONLY, 0) {
        Ok(file) => {
            let metadata = file.metadata()?;
            // Transmission atomically saves native settings as 0644. They remain
            // private behind the owned 0700 directory verified above; accepting
            // its read bits must not admit writable, executable or linked state.
            ensure!(
                metadata.is_file()
                    && metadata.nlink() == 1
                    && metadata.uid() == owner
                    && matches!(metadata.mode() & 0o7777, 0o600 | 0o644)
                    && metadata.len() <= SETTINGS_LIMIT as u64,
                "Torrent settings must be an owned private file"
            );
            let mut bytes = Zeroizing::new(Vec::new());
            file.take(SETTINGS_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .context("Read private torrent settings")?;
            ensure!(
                bytes.len() <= SETTINGS_LIMIT,
                "Torrent settings are too large"
            );
            serde_json::from_slice::<Settings>(&bytes)
                .map_err(|_| anyhow::anyhow!("Invalid private torrent settings"))?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(error) => return Err(error).context("Open private torrent settings"),
    };
    ensure!(
        Path::new(&settings.download_dir).is_absolute()
            && Path::new(&settings.incomplete_dir).is_absolute(),
        "Torrent download directories must be absolute"
    );
    settings.rpc_enabled = true;
    settings.rpc_bind_address = "127.0.0.1".into();
    settings.rpc_port = 8082;
    settings.rpc_url = "/transmission/".into();
    settings.rpc_authentication_required = true;
    settings.rpc_username.clone_from(&config.username);
    settings.rpc_password = config.password.clone();
    settings.rpc_whitelist_enabled = true;
    settings.rpc_whitelist = "127.0.0.1".into();
    settings.rpc_host_whitelist_enabled = true;
    settings.rpc_host_whitelist = "localhost".into();
    let bytes = Zeroizing::new(
        serde_json::to_vec_pretty(&settings)
            .map_err(|_| anyhow::anyhow!("Encode private torrent settings"))?,
    );
    ensure!(
        bytes.len() <= SETTINGS_LIMIT,
        "Torrent settings are too large"
    );
    let mut nonce = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut nonce)?;
    let temporary =
        std::ffi::CString::new(format!(".settings-{:032x}.tmp", u128::from_ne_bytes(nonce)))?;
    let mut file = open_at(
        &directory,
        &temporary,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o600,
    )
    .context("Create temporary torrent settings")?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        // SAFETY: both names are valid C strings relative to this owned directory.
        // Descriptor-relative operations cannot follow a replaced directory path.
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                directory.as_raw_fd(),
                c"settings.json".as_ptr(),
            )
        } == -1
        {
            return Err(std::io::Error::last_os_error());
        }
        directory.sync_all()
    })();
    if result.is_err() {
        // SAFETY: cleanup names only the temporary file created in this directory.
        let _ = unsafe { libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0) };
    }
    result.context("Publish private torrent settings")
}

fn open_at(directory: &File, name: &CStr, flags: i32, mode: libc::c_uint) -> std::io::Result<File> {
    // SAFETY: the borrowed directory and C string remain live through openat.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            mode,
        )
    };
    if descriptor == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful openat returned a new descriptor owned by this File.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

pub(crate) fn command(directory: &Path) -> Command {
    let mut command = Command::new("/usr/bin/transmission-daemon");
    command
        .args(["--foreground", "--config-dir"])
        .arg(directory)
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", directory)
        .env("TR_SAVE_VERSION_FORMAT", "4")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Method {
    SessionGet,
}
#[derive(Deserialize, Serialize)]
struct Request {
    method: Method,
}
#[derive(Deserialize, Serialize)]
struct Response {
    result: String,
    arguments: Session,
}
#[derive(Deserialize, Serialize)]
struct Session {
    version: String,
}

pub(crate) async fn check(
    client: &reqwest::Client,
    config: &TorrentConfig,
    url: &reqwest::Url,
) -> Check {
    let started = Instant::now();
    if !config.enabled {
        return Check::measured(Outcome::NotChecked, started);
    }
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/transmission/rpc"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Check::measured(Outcome::InvalidResponse, started);
    }
    let request = || {
        client
            .post(url.clone())
            .basic_auth(&config.username, Some(config.password.as_str()))
            .json(&Request {
                method: Method::SessionGet,
            })
    };
    let mut response = match request().send().await {
        Ok(response) => response,
        Err(error) => return request_failure(&error, started),
    };
    // Transmission's CSRF handshake repeats only this fixed read, never a write.
    if response.status() == reqwest::StatusCode::CONFLICT {
        let Some(session) = response
            .headers()
            .get(SESSION_HEADER)
            .cloned()
            .filter(|value| {
                !value.is_empty() && value.as_bytes().len() <= 256 && value.to_str().is_ok()
            })
        else {
            return Check {
                http_status: Some(409),
                ..Check::measured(Outcome::InvalidResponse, started)
            };
        };
        response = match request().header(SESSION_HEADER, session).send().await {
            Ok(response) => response,
            Err(error) => return request_failure(&error, started),
        };
    }
    let http_status = response.status().as_u16();
    let result = |outcome| Check {
        http_status: Some(http_status),
        ..Check::measured(outcome, started)
    };
    if !response.status().is_success() {
        return result(Outcome::HttpError);
    }
    if response
        .content_length()
        .is_some_and(|length| length > RESPONSE_LIMIT as u64)
    {
        return result(Outcome::InvalidResponse);
    }
    let mut body = Zeroizing::new(Vec::new());
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if body.len() + chunk.len() <= RESPONSE_LIMIT => {
                body.extend_from_slice(&chunk)
            }
            Ok(Some(_)) => return result(Outcome::InvalidResponse),
            Ok(None) => break,
            Err(error) => return request_failure(&error, started),
        }
    }
    match serde_json::from_slice::<Response>(&body) {
        Ok(value) if value.result == "success" && !value.arguments.version.trim().is_empty() => {
            result(Outcome::Passed)
        }
        _ => result(Outcome::InvalidResponse),
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        config::TorrentConfig,
        monitor::Outcome,
        torrent::{RESPONSE_LIMIT, Request, Response, Session, Settings, check, prepare},
    };
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        path::Path,
        process::Stdio,
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        process::Command,
        time::{Instant, sleep, timeout},
    };
    use zeroize::Zeroizing;

    fn config() -> TorrentConfig {
        TorrentConfig {
            enabled: true,
            username: "torrent".into(),
            password: Zeroizing::new("private-test-password".into()),
        }
    }

    #[test]
    fn native_settings_keep_user_preferences_and_enforce_private_rpc() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("torrent");
        prepare(&config(), &directory).unwrap();
        let path = directory.join("settings.json");
        let mut settings: Settings = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(settings.download_dir, "/media/downloads/torrents");
        assert_eq!(
            settings.incomplete_dir,
            "/media/downloads/torrents/incomplete"
        );
        assert!(settings.ratio_limit_enabled);
        assert_eq!(settings.ratio_limit, 0.0);
        assert!(settings.speed_limit_up_enabled);
        assert_eq!(settings.speed_limit_up, 1);
        assert!(!settings.alt_speed_enabled);
        assert_eq!(settings.alt_speed_up, 1);
        assert!(!settings.port_forwarding_enabled && !settings.lpd_enabled);
        assert!(
            !settings.script_torrent_added_enabled
                && !settings.script_torrent_done_enabled
                && !settings.script_torrent_done_seeding_enabled
        );
        assert_eq!(settings.peer_limit_global, 60);
        assert_eq!(settings.peer_limit_per_torrent, 30);
        assert!(settings.download_queue_enabled);
        assert_eq!(settings.download_queue_size, 2);
        settings.download_dir = "/media/chosen/finished".into();
        settings.incomplete_dir = "/media/chosen/incomplete".into();
        settings.incomplete_dir_enabled = false;
        settings.ratio_limit = 1.5;
        settings
            .native
            .insert("queue-stalled-minutes".into(), serde_json::Value::from(30));
        settings.rpc_bind_address = "0.0.0.0".into();
        settings.rpc_authentication_required = false;
        fs::write(&path, serde_json::to_vec(&settings).unwrap()).unwrap();
        let mut changed = config();
        changed.password = Zeroizing::new("replacement-password".into());
        prepare(&changed, &directory).unwrap();
        let settings: Settings = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(settings.download_dir, "/media/chosen/finished");
        assert_eq!(settings.incomplete_dir, "/media/chosen/incomplete");
        assert!(!settings.incomplete_dir_enabled);
        assert_eq!(settings.ratio_limit, 1.5);
        assert_eq!(
            settings
                .native
                .get("queue-stalled-minutes")
                .and_then(serde_json::Value::as_u64),
            Some(30)
        );
        assert_eq!(settings.rpc_bind_address, "127.0.0.1");
        assert_eq!(settings.rpc_port, 8082);
        assert_eq!(settings.rpc_url, "/transmission/");
        assert!(
            settings.rpc_enabled
                && settings.rpc_authentication_required
                && settings.rpc_whitelist_enabled
                && settings.rpc_host_whitelist_enabled
        );
        assert_eq!(settings.rpc_whitelist, "127.0.0.1");
        assert_eq!(settings.rpc_host_whitelist, "localhost");
        assert_eq!(settings.rpc_username, changed.username);
        assert_eq!(settings.rpc_password.as_str(), changed.password.as_str());
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
    }

    #[test]
    fn native_snake_case_settings_cannot_override_rpc_isolation() {
        #[derive(serde::Serialize)]
        struct NativeSettings<'a> {
            rpc_bind_address: &'a str,
            rpc_authentication_required: bool,
            download_dir: &'a str,
            incomplete_dir: &'a str,
        }
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("torrent");
        prepare(&config(), &directory).unwrap();
        let path = directory.join("settings.json");
        fs::write(
            &path,
            serde_json::to_vec(&NativeSettings {
                rpc_bind_address: "0.0.0.0",
                rpc_authentication_required: false,
                download_dir: "/media/chosen",
                incomplete_dir: "/media/chosen/partial",
            })
            .unwrap(),
        )
        .unwrap();
        prepare(&config(), &directory).unwrap();
        let settings: Settings = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(settings.rpc_bind_address, "127.0.0.1");
        assert!(settings.rpc_authentication_required);
        assert_eq!(settings.download_dir, "/media/chosen");
        assert_eq!(settings.incomplete_dir, "/media/chosen/partial");
    }

    /// Exercise Transmission itself as both settings consumer and producer.
    /// DOWNLOADS_TEST_TRANSMISSION selects the real 4.1 daemon, not a substitute.
    #[tokio::test]
    #[ignore = "requires a native Transmission 4.1 daemon in DOWNLOADS_TEST_TRANSMISSION"]
    async fn native_daemon_settings_survive_preparation_and_restart() {
        let binary = std::env::var_os("DOWNLOADS_TEST_TRANSMISSION").expect("native daemon path");
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("torrent");
        let config = config();
        prepare(&config, &directory).unwrap();
        let path = directory.join("settings.json");
        let mut settings: Settings = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let download_dir = root.path().join("chosen-downloads");
        settings.download_dir = download_dir.to_str().unwrap().to_owned();
        settings.incomplete_dir = root
            .path()
            .join("chosen-incomplete")
            .to_str()
            .unwrap()
            .to_owned();
        // This fixture has no torrents and cannot perform external discovery.
        for name in ["dht-enabled", "pex-enabled"] {
            settings
                .native
                .insert(name.into(), serde_json::Value::from(false));
        }
        for name in ["ip-endpoints-ipv4", "ip-endpoints-ipv6"] {
            settings.native.insert(
                name.into(),
                serde_json::to_value(Vec::<String>::new()).unwrap(),
            );
        }
        settings
            .native
            .insert("queue-stalled-minutes".into(), serde_json::Value::from(17));
        fs::write(&path, serde_json::to_vec(&settings).unwrap()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let url = format!("http://127.0.0.1:{port}/transmission/rpc")
            .parse()
            .unwrap();
        for _ in 0..2 {
            run_native_daemon(Path::new(&binary), &directory, &config, &url).await;
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o7777,
                0o700
            );
            // The native producer uses its own file mode when atomically saving.
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
                0o644
            );
            prepare(&config, &directory).expect("reopen native-produced settings after shutdown");
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
                0o600
            );
            let settings: Settings = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(settings.download_dir, download_dir.to_str().unwrap());
            assert_eq!(
                settings
                    .native
                    .get("queue-stalled-minutes")
                    .and_then(serde_json::Value::as_u64),
                Some(17)
            );
            assert!(settings.ratio_limit_enabled && settings.speed_limit_up_enabled);
            assert_eq!(settings.ratio_limit, 0.0);
            assert_eq!(settings.speed_limit_up, 1);
            assert_eq!(settings.rpc_port, 8082);
        }
    }

    async fn run_native_daemon(
        binary: &Path,
        directory: &Path,
        config: &TorrentConfig,
        url: &reqwest::Url,
    ) {
        let mut child = Command::new(binary)
            .args(["--foreground", "--config-dir"])
            .arg(directory)
            .arg("--port")
            .arg(url.port().unwrap().to_string())
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", directory)
            .env("TR_SAVE_VERSION_FORMAT", "4")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("start native Transmission daemon");
        let client = client();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if check(&client, config, url).await.outcome == Outcome::Passed {
                break;
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "native daemon exited before readiness"
            );
            assert!(
                Instant::now() < deadline,
                "native daemon did not become ready"
            );
            sleep(Duration::from_millis(50)).await;
        }
        let pid = i32::try_from(child.id().unwrap()).unwrap();
        // SAFETY: the live PID belongs to this fixture's owned child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        assert!(
            timeout(Duration::from_secs(10), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn malformed_nonprivate_and_linked_state_is_not_replaced() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("torrent");
        prepare(&config(), &directory).unwrap();
        let path = directory.join("settings.json");
        let linked = root.path().join("hard-linked-settings");
        fs::hard_link(&path, &linked).unwrap();
        assert!(prepare(&config(), &directory).is_err());
        fs::remove_file(linked).unwrap();
        let original = fs::read(&path).unwrap();
        for mode in [0o620, 0o602, 0o610, 0o601, 0o4600] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert!(prepare(&config(), &directory).is_err());
            assert_eq!(fs::read(&path).unwrap(), original);
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
                mode
            );
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, b"invalid private state").unwrap();
        assert!(prepare(&config(), &directory).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"invalid private state");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(prepare(&config(), &directory).is_err());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o666
        );
        fs::remove_file(&path).unwrap();
        let other = root.path().join("other");
        fs::write(&other, b"outside state").unwrap();
        symlink(&other, &path).unwrap();
        assert!(prepare(&config(), &directory).is_err());
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&other).unwrap(), b"outside state");
        let link = root.path().join("linked-directory");
        symlink(&directory, &link).unwrap();
        assert!(prepare(&config(), &link).is_err());
        fs::remove_file(&path).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare(&config(), &directory).is_err());
        assert!(!path.exists());
    }

    async fn server(
        responses: Vec<(u16, &'static str, Vec<u8>)>,
    ) -> (reqwest::Url, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/transmission/rpc", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let task = tokio::spawn(async move {
            for (index, (status, extra_headers, response)) in responses.into_iter().enumerate() {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let header_end = loop {
                    let mut byte = [0_u8; 1];
                    stream.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                    if request.ends_with(b"\r\n\r\n") {
                        break request.len();
                    }
                    assert!(request.len() < 4096);
                };
                let headers = std::str::from_utf8(&request).unwrap().to_ascii_lowercase();
                assert!(headers.starts_with("post /transmission/rpc http/1.1\r\n"));
                assert!(headers.contains("authorization: basic "));
                assert_eq!(
                    headers.contains("x-transmission-session-id: test-session"),
                    index == 1
                );
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                request.resize(header_end + length, 0);
                stream.read_exact(&mut request[header_end..]).await.unwrap();
                let _: Request = serde_json::from_slice(&request[header_end..]).unwrap();
                stream.write_all(format!("HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Length: {}\r\n{extra_headers}\r\n", response.len()).as_bytes()).await.unwrap();
                let _ = stream.write_all(&response).await;
            }
        });
        (url, task)
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
    }

    fn success(version: &str) -> Vec<u8> {
        serde_json::to_vec(&Response {
            result: "success".into(),
            arguments: Session {
                version: version.into(),
            },
        })
        .unwrap()
    }

    #[tokio::test]
    async fn rpc_health_completes_only_one_session_id_handshake() {
        let (url, server) = server(vec![
            (409, "X-Transmission-Session-Id: test-session\r\n", vec![]),
            (200, "", success("4.0.6")),
        ])
        .await;
        let result = check(&client(), &config(), &url).await;
        assert_eq!(result.outcome, Outcome::Passed);
        assert_eq!(result.http_status, Some(200));
        server.await.unwrap();
        let (url, server) = self::server(vec![
            (409, "X-Transmission-Session-Id: test-session\r\n", vec![]),
            (409, "", vec![]),
        ])
        .await;
        let result = check(&client(), &config(), &url).await;
        assert_eq!(result.outcome, Outcome::HttpError);
        assert_eq!(result.http_status, Some(409));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rpc_health_rejects_bad_handshakes_empty_versions_and_oversized_bodies() {
        for response in [
            (409, "", vec![]),
            (200, "", success("")),
            (200, "", vec![b' '; RESPONSE_LIMIT + 1]),
        ] {
            let (url, server) = server(vec![response]).await;
            assert_eq!(
                check(&client(), &config(), &url).await.outcome,
                Outcome::InvalidResponse
            );
            server.await.unwrap();
        }
        let url = "http://example.invalid/transmission/rpc".parse().unwrap();
        assert_eq!(
            check(&client(), &config(), &url).await.outcome,
            Outcome::InvalidResponse
        );
    }
}
