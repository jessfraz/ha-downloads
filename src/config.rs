use ipnet::IpNet;
use serde::{Deserialize, Deserializer};
use std::{fmt, path::Path};
use thiserror::Error;
use zeroize::Zeroizing;

#[derive(Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub vpn: VpnConfig,
    pub network: NetworkConfig,
    #[serde(deserialize_with = "secret")]
    pub api_token: Zeroizing<String>,
    pub media: MediaConfig,
}
#[derive(Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConfig {
    pub radarr: MediaConnection,
    pub sonarr: MediaConnection,
    pub prowlarr: MediaConnection,
    pub plex: MediaConnection,
    #[serde(deserialize_with = "secret")]
    pub request_token: Zeroizing<String>,
    #[serde(deserialize_with = "secret")]
    pub admin_token: Zeroizing<String>,
    #[serde(deserialize_with = "secret")]
    pub integration_token: Zeroizing<String>,
    pub requester_user_ids: Vec<String>,
}
#[derive(Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConnection {
    pub url: String,
    #[serde(deserialize_with = "secret")]
    pub api_key: Zeroizing<String>,
}
impl fmt::Debug for MediaConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaConfig { credentials: [REDACTED] }")
    }
}
impl fmt::Debug for MediaConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaConnection { credentials: [REDACTED] }")
    }
}
impl MediaConfig {
    pub fn validate(&self, status_token: &str) -> Result<(), ConfigError> {
        let tokens = [
            status_token,
            &self.request_token,
            &self.admin_token,
            &self.integration_token,
        ];
        for (i, token) in tokens.iter().enumerate() {
            if !token.is_empty()
                && (token.len() < 32
                    || token.len() > 4096
                    || token.chars().any(char::is_control)
                    || tokens[..i].iter().any(|other| other == token))
            {
                return Err(ConfigError::Media);
            }
        }
        for (connection, key_required) in [
            (&self.radarr, true),
            (&self.sonarr, true),
            (&self.prowlarr, true),
            (&self.plex, false),
        ] {
            if connection.url.is_empty() && connection.api_key.is_empty() {
                continue;
            }
            let url = reqwest::Url::parse(&connection.url).map_err(|_| ConfigError::Media)?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || (key_required && connection.api_key.is_empty())
                || connection.api_key.chars().any(char::is_control)
                || connection.api_key.len() > 4096
            {
                return Err(ConfigError::Media);
            }
        }
        if self.requester_user_ids.len() > 100
            || self.requester_user_ids.iter().any(|id| {
                id.is_empty()
                    || id.len() > 128
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            })
        {
            return Err(ConfigError::Media);
        }
        Ok(())
    }
}
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Config { credentials: [REDACTED] }")
    }
}
#[derive(Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct VpnConfig {
    pub provider: String,
    pub protocol: VpnProtocol,
    #[serde(deserialize_with = "secret")]
    pub username: Zeroizing<String>,
    #[serde(deserialize_with = "secret")]
    pub password: Zeroizing<String>,
    #[serde(deserialize_with = "secret")]
    pub wireguard_private_key: Zeroizing<String>,
    pub wireguard_addresses: String,
    pub countries: String,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VpnProtocol {
    #[default]
    Openvpn,
    Wireguard,
}
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    pub local_subnets: Vec<IpNet>,
}
impl Default for NetworkConfig {
    fn default() -> Self {
        const HA_NETWORK: IpNet = IpNet::V4(ipnet::Ipv4Net::new_assert(
            std::net::Ipv4Addr::new(172, 30, 32, 0),
            23,
        ));
        Self {
            local_subnets: vec![HA_NETWORK],
        }
    }
}
impl NetworkConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        for subnet in &self.local_subnets {
            let IpNet::V4(net) = subnet else {
                return Err(ConfigError::Subnet);
            };
            if !net.network().is_private() || !net.broadcast().is_private() {
                return Err(ConfigError::Subnet);
            }
        }
        Ok(())
    }
}
fn secret<'de, D: Deserializer<'de>>(d: D) -> Result<Zeroizing<String>, D::Error> {
    String::deserialize(d).map(Zeroizing::new)
}
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Cannot read app options.")]
    Read,
    #[error("App options are invalid. Check their field names and value types.")]
    Parse,
    #[error("Choose a VPN provider and enter its service credentials in the app settings.")]
    Missing,
    #[error("The VPN provider identifier must use lowercase letters, digits, or underscores.")]
    Provider,
    #[error("VPN configuration contains unsupported control characters.")]
    UnsafeValue,
    #[error("WireGuard needs a valid private key and interface address.")]
    Wireguard,
    #[error(
        "Local network exceptions must be private IPv4 subnets. Public and default routes are not allowed."
    )]
    Subnet,
    #[error("The optional status API token must contain at least 32 characters.")]
    Token,
    #[error(
        "Media connections need valid HTTP(S) URLs and credentials; API role tokens must be distinct and at least 32 characters."
    )]
    Media,
    #[error(
        "Cannot load private app pairing credentials. Existing credentials must remain private and valid."
    )]
    ManagedCredential,
}

struct AppRuntime {
    linux: bool,
    process_id: u32,
    supervisor_available: bool,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::load_for_runtime(
            path,
            AppRuntime {
                linux: cfg!(target_os = "linux"),
                process_id: std::process::id(),
                supervisor_available: std::env::var_os("SUPERVISOR_TOKEN")
                    .is_some_and(|token| !token.is_empty()),
            },
        )
    }

    fn load_for_runtime(path: &Path, runtime: AppRuntime) -> Result<Self, ConfigError> {
        let bytes = Zeroizing::new(std::fs::read(path).map_err(|_| ConfigError::Read)?);
        let mut config: Self = serde_json::from_slice(&bytes).map_err(|_| ConfigError::Parse)?;
        if runtime.linux
            && runtime.process_id == 1
            && runtime.supervisor_available
            && config.media.integration_token.is_empty()
        {
            #[cfg(unix)]
            {
                let directory = path
                    .parent()
                    .ok_or(ConfigError::ManagedCredential)?
                    .join("media");
                config.media.integration_token =
                    crate::media::credentials::load_or_create(&directory)?;
            }
            #[cfg(not(unix))]
            return Err(ConfigError::ManagedCredential);
        }
        Ok(config)
    }
    pub fn is_configured(&self) -> bool {
        !self.vpn.provider.is_empty()
            && match self.vpn.protocol {
                VpnProtocol::Openvpn => {
                    self.vpn.provider == "custom"
                        || (!self.vpn.username.is_empty() && !self.vpn.password.is_empty())
                }
                VpnProtocol::Wireguard => {
                    !self.vpn.wireguard_private_key.is_empty()
                        && !self.vpn.wireguard_addresses.is_empty()
                }
            }
    }
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.media.validate(&self.api_token)?;
        if !self.vpn.provider.is_empty()
            && (!self
                .vpn
                .provider
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                || self.vpn.provider.len() > 64)
        {
            return Err(ConfigError::Provider);
        }
        for value in [
            &self.vpn.username[..],
            &self.vpn.password[..],
            &self.vpn.wireguard_private_key[..],
            &self.vpn.wireguard_addresses,
            &self.vpn.countries,
        ] {
            if value.chars().any(char::is_control) || value.len() > 4096 {
                return Err(ConfigError::UnsafeValue);
            }
        }
        if self.vpn.protocol == VpnProtocol::Wireguard && !self.vpn.wireguard_private_key.is_empty()
        {
            let key = &self.vpn.wireguard_private_key;
            if key.len() != 44
                || !key.ends_with('=')
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
            {
                return Err(ConfigError::Wireguard);
            }
            let addresses: Result<Vec<IpNet>, _> = self
                .vpn
                .wireguard_addresses
                .split(',')
                .map(|s| s.trim().parse())
                .collect();
            if addresses.is_err() {
                return Err(ConfigError::Wireguard);
            }
        }
        self.network.validate()?;
        if !self.api_token.is_empty()
            && (self.api_token.len() < 32 || self.api_token.chars().any(char::is_control))
        {
            return Err(ConfigError::Token);
        }
        if !self.is_configured() {
            return Err(ConfigError::Missing);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use crate::config::{AppRuntime, Config, ConfigError, MediaConfig, MediaConnection};
    fn configured() -> Config {
        serde_json::from_str(r#"{"vpn":{"provider":"test","username":"user","password":"secret"}}"#)
            .expect("fixture")
    }
    #[test]
    fn parse_errors_never_disclose_values() {
        let error = Config::load(std::path::Path::new("/does-not-exist")).expect_err("missing");
        assert_eq!(error.to_string(), "Cannot read app options.");
    }
    #[test]
    fn debug_redacts_all_credentials() {
        let c = configured();
        let d = format!("{c:?}");
        assert!(!d.contains("secret"));
        assert!(!d.contains("user"));
    }
    #[test]
    fn public_bypasses_are_rejected() {
        let mut c = configured();
        for s in [
            "0.0.0.0/0",
            "10.0.0.0/7",
            "192.168.0.0/15",
            "::/0",
            "203.0.113.0/24",
        ] {
            c.network.local_subnets = vec![s.parse().expect("subnet")];
            assert!(matches!(c.validate(), Err(ConfigError::Subnet)));
        }
    }
    #[test]
    fn valid_config_has_no_off_switch() {
        assert!(configured().validate().is_ok());
        assert!(serde_json::from_str::<Config>(r#"{"firewall":false}"#).is_err());
    }
    #[test]
    fn newline_credentials_are_rejected() {
        let mut c = configured();
        c.vpn.password.push('\n');
        assert!(matches!(c.validate(), Err(ConfigError::UnsafeValue)));
    }

    #[test]
    fn plex_identity_allows_an_optional_key_without_weakening_arr_credentials_or_urls() {
        let mut media = MediaConfig::default();
        media.plex.url = "http://plex.example:32400".into();
        assert!(media.validate("").is_ok());
        media.plex.api_key.push_str("fixture-key");
        assert!(media.validate("").is_ok());
        media.plex.url.clear();
        assert!(matches!(media.validate(""), Err(ConfigError::Media)));
        media.plex.api_key.clear();
        for url in [
            "ftp://plex.example",
            "http://user:password@plex.example",
            "http://plex.example?key=value",
            "http://plex.example/#fragment",
        ] {
            media.plex.url = url.into();
            assert!(matches!(media.validate(""), Err(ConfigError::Media)));
        }
        let connection = MediaConnection {
            url: "http://media.example".into(),
            ..MediaConnection::default()
        };
        for media in [
            MediaConfig {
                radarr: connection.clone(),
                ..MediaConfig::default()
            },
            MediaConfig {
                sonarr: connection.clone(),
                ..MediaConfig::default()
            },
            MediaConfig {
                prowlarr: connection,
                ..MediaConfig::default()
            },
        ] {
            assert!(matches!(media.validate(""), Err(ConfigError::Media)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_pairing_is_generated_only_at_the_home_assistant_runtime_boundary() {
        for runtime in [
            AppRuntime {
                linux: false,
                process_id: 1,
                supervisor_available: true,
            },
            AppRuntime {
                linux: true,
                process_id: 2,
                supervisor_available: true,
            },
            AppRuntime {
                linux: true,
                process_id: 1,
                supervisor_available: false,
            },
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("options.json");
            std::fs::write(&path, b"{}").unwrap();
            let config = Config::load_for_runtime(&path, runtime).unwrap();
            assert!(config.media.integration_token.is_empty());
            assert!(!directory.path().join("media").exists());
        }

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("options.json");
        let options = r#"{"vpn":{"provider":"test","username":"fixture","password":"fixture"},"api_token":"readonly-fixture-aaaaaaaaaaaaaaaa","media":{"request_token":"request-fixture-aaaaaaaaaaaaaaaaa","admin_token":"admin-fixture-aaaaaaaaaaaaaaaaaaa"}}"#;
        std::fs::write(&path, options).unwrap();
        let original: Config = serde_json::from_str(options).unwrap();
        let config = Config::load_for_runtime(
            &path,
            AppRuntime {
                linux: true,
                process_id: 1,
                supervisor_available: true,
            },
        )
        .unwrap();
        assert_eq!(config.media.integration_token.len(), 64);
        assert!(config.api_token == original.api_token);
        assert!(config.media.request_token == original.media.request_token);
        assert!(config.media.admin_token == original.media.admin_token);
        assert!(config.validate().is_ok());
        let reopened = Config::load_for_runtime(
            &path,
            AppRuntime {
                linux: true,
                process_id: 1,
                supervisor_available: true,
            },
        )
        .unwrap();
        assert!(config.media.integration_token == reopened.media.integration_token);
        // Loading never rewrites HA-managed options with the generated secret.
        assert!(std::fs::read_to_string(&path).unwrap() == options);
    }

    #[cfg(unix)]
    #[test]
    fn an_explicit_integration_token_does_not_read_or_replace_managed_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("options.json");
        let media = directory.path().join("media");
        std::fs::create_dir(&media).unwrap();
        let managed = media.join("integration-token");
        std::fs::write(&managed, b"corrupt-state").unwrap();
        let options = r#"{"media":{"integration_token":"manual-fixture-aaaaaaaaaaaaaaaaaa"}}"#;
        std::fs::write(&path, options).unwrap();
        let original: Config = serde_json::from_str(options).unwrap();
        let config = Config::load_for_runtime(
            &path,
            AppRuntime {
                linux: true,
                process_id: 1,
                supervisor_available: true,
            },
        )
        .unwrap();
        assert!(config.media.integration_token == original.media.integration_token);
        assert!(std::fs::read(&managed).unwrap() == b"corrupt-state");
    }
}
