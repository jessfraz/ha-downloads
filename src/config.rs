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
}
impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = Zeroizing::new(std::fs::read(path).map_err(|_| ConfigError::Read)?);
        serde_json::from_slice(&bytes).map_err(|_| ConfigError::Parse)
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
        for subnet in &self.network.local_subnets {
            let IpNet::V4(net) = subnet else {
                return Err(ConfigError::Subnet);
            };
            if !net.network().is_private() || !net.broadcast().is_private() {
                return Err(ConfigError::Subnet);
            }
        }
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
    use crate::config::{Config, ConfigError};
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
}
