//! Gluetun is the only owner of tunnel, DNS and firewall configuration.
use crate::config::{Config, VpnProtocol};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::IpAddr,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    process::Command,
};
use zeroize::Zeroizing;

pub(crate) const PRIVATE_DIR: &str = "/run/downloads";
const CUSTOM_CONFIG: &str = "/data/openvpn/custom.conf";

/// An exclusive, private namespace. Cleanup only removes files created by this instance.
pub(crate) struct Secrets {
    path: PathBuf,
    files: Vec<(PathBuf, u64, u64)>,
    pub(crate) api_key: Zeroizing<String>,
}
impl Secrets {
    pub(crate) fn create(base: &Path, config: &Config) -> Result<Self> {
        if !base.exists() {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(base)
                .context("Create private runtime directory")?;
        }
        let meta = fs::symlink_metadata(base).context("Inspect private runtime directory")?;
        ensure!(
            meta.is_dir() && meta.uid() == effective_uid() && meta.mode() & 0o777 == 0o700,
            "Runtime directory must be owned and private"
        );
        let mut random = [0_u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let api_key = Zeroizing::new(
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        );
        let path = base.join(format!("session-{}-{}", std::process::id(), &api_key[..16]));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        let mut secrets = Self {
            path,
            files: Vec::new(),
            api_key,
        };
        secrets.write("openvpn_user", config.vpn.username.as_bytes())?;
        secrets.write("openvpn_password", config.vpn.password.as_bytes())?;
        secrets.write("wireguard_key", config.vpn.wireguard_private_key.as_bytes())?;
        let auth = Zeroizing::new(format!(
            "[[roles]]\nname = \"downloads\"\nroutes = [\"GET /v1/vpn/status\", \"GET /v1/dns/status\", \"GET /v1/publicip/ip\"]\nauth = \"apikey\"\napikey = \"{}\"\n",
            secrets.api_key.as_str()
        ));
        secrets.write("control.toml", auth.as_bytes())?;
        File::open(&secrets.path)?.sync_all()?;
        Ok(secrets)
    }
    fn write(&mut self, name: &str, bytes: &[u8]) -> Result<()> {
        let path = self.path.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let meta = file.metadata()?;
        self.files.push((path, meta.dev(), meta.ino()));
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }
    pub(crate) fn command(&self, config: &Config) -> Result<Command> {
        if config.vpn.provider == "custom" {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(CUSTOM_CONFIG)
                .context("Custom VPN requires /data/openvpn/custom.conf")?;
            let meta = file.metadata()?;
            ensure!(
                meta.is_file()
                    && meta.nlink() == 1
                    && meta.uid() == effective_uid()
                    && meta.mode() & 0o077 == 0,
                "Custom VPN configuration must be an owned private regular file"
            );
        }
        let mut command = Command::new("/gluetun-entrypoint");
        // Do not inherit arbitrary upstream environment overrides, especially firewall/DNS off switches.
        command.env_clear().envs([
            (
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            ),
            ("VPN_SERVICE_PROVIDER", config.vpn.provider.as_str()),
            (
                "VPN_TYPE",
                match config.vpn.protocol {
                    VpnProtocol::Openvpn => "openvpn",
                    VpnProtocol::Wireguard => "wireguard",
                },
            ),
            (
                "FIREWALL_ENABLED_DISABLING_IT_SHOOTS_YOU_IN_YOUR_FOOT",
                "on",
            ),
            ("DNS_SERVER", "on"),
            ("DNS_KEEP_NAMESERVER", "off"),
            ("DNS_ADDRESS", "127.0.0.1"),
            ("DNS_UPSTREAM_RESOLVER_TYPE", "dot"),
            ("DNS_UPSTREAM_RESOLVERS", "cloudflare"),
            ("DNS_UPSTREAM_IPV6", "off"),
            ("HTTPPROXY", "on"),
            ("HTTPPROXY_LISTENING_ADDRESS", ":8888"),
            ("HTTP_CONTROL_SERVER_ADDRESS", "127.0.0.1:8000"),
            ("HEALTH_SERVER_ADDRESS", "127.0.0.1:9999"),
            ("FIREWALL_INPUT_PORTS", "8080,8099,8888"),
            ("LOG_LEVEL", "warn"),
            // Gluetun3.41.3 dereferences these rates before applying its defaults.
            ("PPROF_ENABLED", "no"),
            ("PPROF_BLOCK_PROFILE_RATE", "0"),
            ("PPROF_MUTEX_PROFILE_RATE", "0"),
            ("PPROF_HTTP_SERVER_ADDRESS", "127.0.0.1:6060"),
            ("PUID", "0"),
            ("PGID", "0"),
        ]);
        command
            .env("OPENVPN_USER_SECRETFILE", self.path.join("openvpn_user"))
            .env(
                "OPENVPN_PASSWORD_SECRETFILE",
                self.path.join("openvpn_password"),
            )
            .env(
                "WIREGUARD_PRIVATE_KEY_SECRETFILE",
                self.path.join("wireguard_key"),
            )
            .env(
                "HTTP_CONTROL_SERVER_AUTH_CONFIG_FILEPATH",
                self.path.join("control.toml"),
            )
            .env("WIREGUARD_ADDRESSES", &config.vpn.wireguard_addresses)
            .env("SERVER_COUNTRIES", &config.vpn.countries)
            .env(
                "FIREWALL_OUTBOUND_SUBNETS",
                config
                    .network
                    .local_subnets
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            );
        if config.vpn.provider == "custom" {
            command.env("OPENVPN_CUSTOM_CONFIG", CUSTOM_CONFIG);
        }
        // Upstream diagnostics can include service credentials. Only sanitized lifecycle messages leave this process.
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Ok(command)
    }
}
impl Drop for Secrets {
    fn drop(&mut self) {
        for (path, dev, ino) in &self.files {
            if let Ok(meta) = fs::symlink_metadata(path)
                && meta.is_file()
                && meta.dev() == *dev
                && meta.ino() == *ino
                && meta.nlink() == 1
            {
                let _ = fs::remove_file(path);
            }
        }
        let _ = fs::remove_dir(&self.path);
    }
}
fn effective_uid() -> u32 {
    // SAFETY: geteuid has no arguments and cannot fail.
    unsafe { libc::geteuid() }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Protection {
    pub(crate) firewall: bool,
    pub(crate) dns: bool,
    pub(crate) dns_safe: bool,
    pub(crate) tunnel: bool,
}
impl Protection {
    pub(crate) fn ready(self) -> bool {
        self.firewall && self.dns && self.tunnel
    }
}

pub(crate) struct Probe {
    client: reqwest::Client,
    key: Zeroizing<String>,
}
#[derive(Deserialize)]
struct ServiceState {
    status: String,
}
#[derive(Deserialize)]
struct PublicIp {
    public_ip: IpAddr,
}
impl Probe {
    pub(crate) fn new(key: Zeroizing<String>) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()?,
            key,
        })
    }
    async fn running(&self, service: &str) -> bool {
        let result = self
            .client
            .get(format!("http://127.0.0.1:8000/v1/{service}/status"))
            .header("X-API-Key", self.key.as_str())
            .send()
            .await;
        match result {
            Ok(response) if response.status().is_success() => response
                .json::<ServiceState>()
                .await
                .is_ok_and(|s| s.status == "running"),
            _ => false,
        }
    }
    pub(crate) async fn check(&self) -> Protection {
        let (firewall, dns_safe, tunnel) = tokio::join!(
            firewall_verified(),
            self.dns_configuration_safe(),
            self.tunnel_healthy()
        );
        let dns = dns_safe && self.running("dns").await && dns_roundtrip().await.is_ok();
        Protection {
            firewall,
            dns,
            dns_safe,
            tunnel,
        }
    }
    async fn dns_configuration_safe(&self) -> bool {
        let Ok(resolv) = tokio::fs::read_to_string("/etc/resolv.conf").await else {
            return false;
        };
        loopback_resolver(&resolv)
    }
    async fn tunnel_healthy(&self) -> bool {
        self.running("vpn").await
            && self
                .client
                .get("http://127.0.0.1:9999/")
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
    }
    pub(crate) async fn public_ip(&self) -> Option<String> {
        self.client
            .get("http://127.0.0.1:8000/v1/publicip/ip")
            .header("X-API-Key", self.key.as_str())
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<PublicIp>()
            .await
            .ok()
            .map(|p| p.public_ip.to_string())
    }
}
fn loopback_resolver(contents: &str) -> bool {
    let mut found = false;
    for line in contents.lines() {
        let mut words = line.split('#').next().unwrap_or("").split_whitespace();
        if words.next() == Some("nameserver") {
            match words.next().and_then(|s| s.parse::<IpAddr>().ok()) {
                Some(IpAddr::V4(ip)) if ip == std::net::Ipv4Addr::LOCALHOST => found = true,
                _ => return false,
            }
        }
    }
    found
}
async fn dns_roundtrip() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(2), async {
        // A real DNS answer, rather than a listening socket, proves the configured resolver is usable.
        let query = b"\x6a\x73\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x07example\x03com\x00\x00\x01\x00\x01";
        let mut socket = TcpStream::connect("127.0.0.1:53").await?;
        socket.write_u16(query.len() as u16).await?;
        socket.write_all(query).await?;
        let length = socket.read_u16().await?;
        ensure!((12..=4096).contains(&length), "Invalid DNS response length");
        let mut answer = vec![0; usize::from(length)]; socket.read_exact(&mut answer).await?;
        ensure!(answer[..2] == query[..2] && answer[2] & 0x80 != 0 && answer[3] & 0x0f == 0 && (answer[6] != 0 || answer[7] != 0), "DNS response has no successful answer");
        Ok(())
    }).await.context("DNS check timed out")?
}
async fn firewall_verified() -> bool {
    let (v4, v6) = tokio::join!(default_deny("iptables"), default_deny("ip6tables"));
    v4 && v6
}
async fn default_deny(program: &str) -> bool {
    let mut command = Command::new(program);
    command
        .args(["-w", "1", "-S", "OUTPUT"])
        .kill_on_drop(true)
        .stdin(Stdio::null());
    let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(2), command.output()).await
    else {
        return false;
    };
    output.status.success()
        && String::from_utf8(output.stdout)
            .is_ok_and(|s| s.lines().any(|line| line == "-P OUTPUT DROP"))
}

#[cfg(test)]
mod tests {
    use crate::{
        config::Config,
        vpn::{Secrets, loopback_resolver},
    };
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    };
    #[test]
    fn rejects_direct_dns_fallback() {
        assert!(loopback_resolver("nameserver 127.0.0.1\n"));
        assert!(!loopback_resolver(
            "nameserver 127.0.0.1\nnameserver 192.0.2.53\n"
        ));
        assert!(!loopback_resolver("search example\n"));
        assert!(!loopback_resolver("nameserver 127.0.0.11\n"));
        assert!(!loopback_resolver("nameserver ::1\n"));
    }
    #[test]
    fn secrets_are_private_and_cleanup_is_owned() {
        let root = tempfile::tempdir().expect("temp");
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).expect("mode");
        let config: Config = serde_json::from_str(
            r#"{"vpn":{"provider":"test","username":"a","password":"secret"}}"#,
        )
        .expect("config");
        let secrets = Secrets::create(root.path(), &config).expect("secrets");
        let path = secrets.path.clone();
        assert_eq!(fs::metadata(&path).expect("meta").mode() & 0o777, 0o700);
        for (file, _, _) in &secrets.files {
            assert_eq!(fs::metadata(file).expect("meta").mode() & 0o777, 0o600);
        }
        let command = secrets.command(&config).expect("command");
        assert!(!format!("{command:?}").contains("secret\""));
        drop(secrets);
        assert!(!path.exists());
    }
    #[test]
    fn refuses_foreign_runtime_directory() {
        let root = tempfile::tempdir().expect("temp");
        let link = root.path().join("link");
        symlink(root.path(), &link).expect("link");
        assert!(Secrets::create(&link, &Config::default()).is_err());
    }
}
