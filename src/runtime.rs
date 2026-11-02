//! Own both process groups. SAB stops before Gluetun can remove its protections.
use crate::{
    config::Config,
    status::{DownloaderState, Status, Verification, VpnState},
    vpn::{PRIVATE_DIR, Probe, Protection, Secrets},
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    sync::watch,
    time::{Instant, sleep, timeout},
};

struct OwnedProcess {
    child: Child,
    group: Option<i32>,
}
impl OwnedProcess {
    fn spawn(mut command: Command) -> Result<Self> {
        command.process_group(0);
        command.kill_on_drop(true);
        let child = command.spawn().context("Start protected service")?;
        let group = child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .context("Service PID is unavailable")?;
        Ok(Self {
            child,
            group: Some(group),
        })
    }
    fn exited(&mut self) -> Result<Option<ExitStatus>> {
        self.child.try_wait().context("Read service exit status")
    }
    async fn stop(&mut self) -> Result<()> {
        let Some(group) = self.group else {
            return Ok(());
        };
        let term = signal_group(group, libc::SIGTERM);
        let waited = timeout(Duration::from_secs(10), self.child.wait()).await;
        // A parent can exit while its OpenVPN subprocess is still alive. Always close the entire owned group.
        let killed = signal_group(group, libc::SIGKILL);
        let reaped = if waited.is_err() {
            self.child.wait().await.map(|_| ())
        } else {
            waited.context("Wait for service")?.map(|_| ())
        };
        self.group = None;
        term?;
        killed?;
        reaped.context("Reap service")
    }
}
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if let Some(group) = self.group.take() {
            let _ = signal_group(group, libc::SIGKILL);
        }
    }
}
fn signal_group(group: i32, signal: i32) -> Result<()> {
    // SAFETY: every positive group here comes from an owned child created in its own process group.
    if unsafe { libc::kill(-group, signal) } == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error).context("Signal owned service group");
        }
    }
    Ok(())
}
/// The HA app runs without an init wrapper in a private process namespace.
/// After the two direct services are reaped, PID 1 owns their orphaned descendants,
/// including OpenVPN, which deliberately creates a different process group.
async fn reap_container_children() -> Result<()> {
    if !cfg!(target_os = "linux") || std::process::id() != 1 {
        return Ok(());
    }
    let terminate_until = Instant::now() + Duration::from_secs(1);
    let deadline = terminate_until + Duration::from_secs(2);
    let mut terminated = BTreeSet::new();
    let mut killed = BTreeSet::new();
    loop {
        let mut children = BTreeSet::new();
        for task in fs::read_dir("/proc/self/task").context("Inspect owned container tasks")? {
            let path = task?.path().join("children");
            let listing = match fs::read_to_string(path) {
                Ok(listing) => listing,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("Inspect owned container children"),
            };
            for pid in listing.split_whitespace() {
                let pid = pid.parse::<i32>().context("Invalid owned child PID")?;
                ensure!(pid > 1, "Invalid owned child PID");
                children.insert(pid);
            }
        }
        if children.is_empty() {
            return Ok(());
        }
        for pid in children {
            let mut status = 0;
            // SAFETY: the kernel's children list is scoped to this container PID 1.
            // Both registered direct children were already waited, so their status cannot be stolen.
            let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if result > 0 {
                continue;
            }
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    continue;
                }
                return Err(error).context("Reap owned container descendant");
            }
            let signal = if Instant::now() >= terminate_until {
                if !killed.insert(pid) {
                    continue;
                }
                libc::SIGKILL
            } else {
                if !terminated.insert(pid) {
                    continue;
                }
                libc::SIGTERM
            };
            // SAFETY: an unreaped child PID remains allocated until this parent waits it.
            if unsafe { libc::kill(pid, signal) } == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error).context("Stop owned container descendant");
                }
            }
        }
        ensure!(
            Instant::now() < deadline,
            "Owned container descendants did not stop"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

#[derive(Default)]
struct Children {
    vpn: Option<OwnedProcess>,
    downloader: Option<OwnedProcess>,
}
impl Children {
    async fn stop(&mut self) -> Result<()> {
        let sab = match self.downloader.as_mut() {
            Some(child) => child.stop().await,
            None => Ok(()),
        };
        let vpn = match self.vpn.as_mut() {
            Some(child) => child.stop().await,
            None => Ok(()),
        };
        let descendants = reap_container_children().await;
        sab.and(vpn).and(descendants)
    }
    fn check_children(&mut self) -> Result<()> {
        if let Some(vpn) = self.vpn.as_mut()
            && vpn.exited()?.is_some()
        {
            bail!("VPN service exited. Downloads have been stopped.")
        }
        if let Some(sab) = self.downloader.as_mut()
            && sab.exited()?.is_some()
        {
            bail!("Downloader exited unexpectedly.")
        }
        Ok(())
    }
    async fn supervise(
        &mut self,
        config: &Config,
        secrets: &Secrets,
        status: &watch::Sender<Status>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        if *shutdown.borrow() {
            return Ok(());
        }
        self.vpn = Some(OwnedProcess::spawn(secrets.command(config)?)?);
        let probe = Probe::new(secrets.api_key.clone())?;
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            self.check_children()?;
            let protection = tokio::select! { changed = shutdown.changed() => { let _ = changed; return Ok(()) }, check = probe.check() => check };
            if protection.ready() {
                break;
            }
            if Instant::now() >= deadline {
                bail!("VPN protection checks did not pass. Downloads remain stopped.")
            }
            tokio::select! { _ = shutdown.changed() => return Ok(()), _ = sleep(Duration::from_secs(1)) => {} }
        }
        if *shutdown.borrow() {
            return Ok(());
        }
        prepare_downloader(Path::new("/data/sabnzbd"))?;
        let mut sab = Command::new("/opt/venv/bin/python");
        sab.args([
            "/opt/SABnzbd.py",
            "--config-file",
            "/data/sabnzbd/sabnzbd.ini",
            "--server",
            "127.0.0.1:8081",
            "--console",
            "--pause",
            "--disable-file-log",
        ])
        .env_clear()
        .env(
            "PATH",
            "/opt/venv/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", "/data/sabnzbd")
        .env("PYTHONUNBUFFERED", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        self.downloader = Some(OwnedProcess::spawn(sab)?);
        let mut starting = Status::starting(&config.vpn.provider);
        starting.downloader.state = DownloaderState::Starting;
        starting.downloader.paused = Some(true);
        status.send_replace(starting);
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.check_children()?;
            let protection = tokio::select! { _ = shutdown.changed() => return Ok(()), value = probe.check() => value };
            if !protection.firewall || !protection.dns_safe {
                bail!("Protection checks failed. Downloads have been stopped.")
            }
            if client
                .get("http://127.0.0.1:8081/sabnzbd/api?mode=version&output=json")
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            if Instant::now() >= deadline {
                bail!("Downloader did not become ready.")
            }
            tokio::select! { _ = shutdown.changed() => return Ok(()), _ = sleep(Duration::from_secs(1)) => {} }
        }
        loop {
            self.check_children()?;
            let protection = tokio::select! { _ = shutdown.changed() => return Ok(()), value = probe.check() => value };
            if !protection.firewall || !protection.dns_safe {
                bail!("Protection checks failed. Downloads have been stopped.")
            }
            publish(status, &config.vpn.provider, protection, &probe).await;
            tokio::select! { _ = shutdown.changed() => return Ok(()), _ = sleep(Duration::from_secs(3)) => {} }
        }
    }
}
fn prepare_downloader(directory: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("Prepare downloader directory"),
    }
    let meta = fs::symlink_metadata(directory)?;
    // SAFETY: geteuid has no arguments and cannot fail.
    let uid = unsafe { libc::geteuid() };
    ensure!(
        meta.is_dir() && meta.uid() == uid && meta.mode() & 0o077 == 0,
        "Downloader directory must be owned and private"
    );
    let path = directory.join("sabnzbd.ini");
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(mut file) => {
            // Configure only a new file. Existing SAB credentials and queue settings remain SAB-owned.
            file.write_all(b"[misc]\nurl_base = /sabnzbd\n")?;
            file.sync_all()?;
            File::open(directory)?.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)?;
            let meta = file.metadata()?;
            ensure!(
                meta.is_file()
                    && meta.nlink() == 1
                    && meta.uid() == uid
                    && meta.mode() & 0o077 == 0,
                "Downloader configuration must be an owned private file"
            );
        }
        Err(error) => return Err(error).context("Prepare downloader configuration"),
    }
    Ok(())
}

async fn publish(
    sender: &watch::Sender<Status>,
    provider: &str,
    protection: Protection,
    probe: &Probe,
) {
    let mut status = if protection.ready() {
        Status::ready(
            provider,
            probe.public_ip().await,
            protection.firewall,
            protection.dns,
        )
    } else if protection.tunnel {
        let mut value = Status::degraded(
            provider,
            "DNS answers are unavailable. Downloads retain the protected resolver and firewall.",
            true,
        );
        value.vpn.state = VpnState::Connected;
        value
    } else {
        Status::degraded(
            provider,
            "VPN connection interrupted. The firewall blocks direct downloads.",
            true,
        )
    };
    status.protection.kill_switch = if protection.firewall {
        Verification::Verified
    } else {
        Verification::Failed
    };
    status.protection.dns = if protection.dns {
        Verification::Verified
    } else {
        Verification::Failed
    };
    // Runtime never sends SAB a resume command. Each cold start is paused; its UI owns later changes.
    sender.send_replace(status);
}

pub async fn run(
    config: Config,
    status: watch::Sender<Status>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    if let Err(error) = config.validate() {
        status.send_replace(Status::needs_configuration(&error.to_string()));
        return Ok(());
    }
    if !cfg!(target_os = "linux") || std::process::id() != 1 {
        status.send_replace(Status::degraded(
            &config.vpn.provider,
            "VPN services require the app's private Linux container. Downloads remain stopped.",
            false,
        ));
        bail!("VPN services require the private Linux app container with init disabled");
    }
    status.send_replace(Status::starting(&config.vpn.provider));
    let secrets = match Secrets::create(Path::new(PRIVATE_DIR), &config) {
        Ok(secrets) => secrets,
        Err(error) => {
            status.send_replace(Status::degraded(
                &config.vpn.provider,
                "Private VPN configuration could not be prepared. Downloads remain stopped.",
                false,
            ));
            return Err(error);
        }
    };
    let mut children = Children::default();
    let result = children
        .supervise(&config, &secrets, &status, &mut shutdown)
        .await;
    status.send_replace(Status::stopping(&config.vpn.provider));
    let cleanup = children.stop().await;
    drop(secrets);
    if result.is_err() || cleanup.is_err() {
        status.send_replace(Status::degraded(&config.vpn.provider, "A protected service failed. Downloads are stopped; check the app configuration and restart.", false));
    }
    result.and(cleanup)
}

#[cfg(test)]
mod tests {
    use crate::{
        config::Config,
        runtime::{OwnedProcess, run},
        status::{State, Status},
    };
    use std::time::Duration;
    use tokio::{process::Command, sync::watch};
    #[test]
    fn bootstrap_preserves_existing_private_sab_configuration() {
        let root = tempfile::tempdir().expect("temp");
        let directory = root.path().join("sab");
        crate::runtime::prepare_downloader(&directory).expect("new config");
        let path = directory.join("sabnzbd.ini");
        let original = b"[misc]\nurl_base = /sabnzbd\napi_key = test-generated-only\n";
        std::fs::write(&path, original).expect("owned existing config");
        crate::runtime::prepare_downloader(&directory).expect("preserve");
        assert_eq!(std::fs::read(path).expect("read"), original);
    }
    #[test]
    fn bootstrap_rejects_symlink_configuration() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().expect("temp");
        let directory = root.path().join("sab");
        std::fs::create_dir(&directory).expect("directory");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("private");
        let other = root.path().join("other");
        std::fs::write(&other, b"unchanged").expect("other");
        symlink(&other, directory.join("sabnzbd.ini")).expect("link");
        assert!(crate::runtime::prepare_downloader(&directory).is_err());
        assert_eq!(std::fs::read(other).expect("read"), b"unchanged");
    }
    #[tokio::test]
    async fn configured_native_start_is_rejected_before_services() {
        assert_ne!(std::process::id(), 1, "Cargo runs this test as its child");
        let config: Config = serde_json::from_str(
            r#"{"vpn":{"provider":"test","username":"test","password":"test"}}"#,
        )
        .expect("valid configuration");
        config.validate().expect("configured");
        let (sender, receiver) = watch::channel(Status::starting("test"));
        let (_shutdown, signal) = watch::channel(false);
        let error = run(config, sender, signal)
            .await
            .expect_err("native VPN services refused");
        assert_eq!(
            error.to_string(),
            "VPN services require the private Linux app container with init disabled"
        );
        assert_eq!(receiver.borrow().state, State::Degraded);
    }
    #[tokio::test]
    async fn missing_configuration_starts_nothing() {
        let (sender, receiver) = watch::channel(Status::starting(""));
        let (_shutdown, signal) = watch::channel(false);
        run(Config::default(), sender, signal)
            .await
            .expect("setup mode");
        assert_eq!(receiver.borrow().state, State::NeedsConfiguration);
    }
    #[tokio::test]
    async fn terminates_and_reaps_a_real_child() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "trap 'exit 0' TERM; while :; do sleep 1; done"]);
        let mut owned = OwnedProcess::spawn(command).expect("spawn");
        tokio::time::timeout(Duration::from_secs(12), owned.stop())
            .await
            .expect("bounded stop")
            .expect("stop");
        assert!(owned.exited().expect("status").is_some());
        owned.stop().await.expect("idempotent stop");
    }
    #[tokio::test]
    async fn unexpected_exit_remains_observable_and_cleanup_is_idempotent() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 23"]);
        let mut owned = OwnedProcess::spawn(command).expect("spawn");
        let exit = owned.child.wait().await.expect("wait");
        assert_eq!(exit.code(), Some(23));
        owned.stop().await.expect("cleanup");
        owned.stop().await.expect("repeat cleanup");
    }
}
