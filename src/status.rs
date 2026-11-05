use crate::monitor::Monitoring;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    NeedsConfiguration,
    Starting,
    Ready,
    Degraded,
    Stopping,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VpnState {
    Disconnected,
    Connecting,
    Connected,
    Unknown,
    Unhealthy,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloaderState {
    Stopped,
    Starting,
    Running,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    Unverified,
    Verified,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct VpnStatus {
    pub state: VpnState,
    pub provider: String,
    pub public_ip: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct DownloaderStatus {
    pub state: DownloaderState,
    pub url: String,
    pub paused: Option<bool>,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct ProtectionStatus {
    pub kill_switch: Verification,
    pub dns: Verification,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct Status {
    pub version: String,
    pub state: State,
    pub message: String,
    pub vpn: VpnStatus,
    pub downloader: DownloaderStatus,
    pub protection: ProtectionStatus,
    #[serde(default)]
    pub diagnostics: Monitoring,
}
impl Status {
    pub fn needs_configuration(message: &str) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            state: State::NeedsConfiguration,
            message: message.into(),
            vpn: VpnStatus {
                state: VpnState::Disconnected,
                provider: String::new(),
                public_ip: None,
            },
            downloader: DownloaderStatus {
                state: DownloaderState::Stopped,
                url: "sabnzbd/".into(),
                paused: None,
            },
            protection: ProtectionStatus {
                kill_switch: Verification::Unverified,
                dns: Verification::Unverified,
            },
            diagnostics: Monitoring::default(),
        }
    }
    pub fn starting(provider: &str) -> Self {
        let mut value = Self::needs_configuration("Connecting the VPN before starting downloads.");
        value.state = State::Starting;
        value.vpn.provider = provider.into();
        value.vpn.state = VpnState::Connecting;
        value
    }
    pub fn ready(
        provider: &str,
        public_ip: Option<String>,
        firewall_verified: bool,
        dns_verified: bool,
    ) -> Self {
        let mut value = Self::starting(provider);
        value.state = if firewall_verified && dns_verified {
            State::Ready
        } else {
            State::Degraded
        };
        value.message = if firewall_verified && dns_verified {
            "VPN connected. Downloads use the protected network."
        } else {
            "Protection checks have not passed. Downloads remain stopped."
        }
        .into();
        value.vpn.state = VpnState::Connected;
        value.vpn.public_ip = public_ip;
        value.downloader.state = if firewall_verified && dns_verified {
            DownloaderState::Running
        } else {
            DownloaderState::Stopped
        };
        value.protection.kill_switch = if firewall_verified {
            Verification::Verified
        } else {
            Verification::Failed
        };
        value.protection.dns = if dns_verified {
            Verification::Verified
        } else {
            Verification::Failed
        };
        value
    }
    pub fn degraded(provider: &str, message: &str, downloader_running: bool) -> Self {
        let mut value = Self::needs_configuration(message);
        value.state = State::Degraded;
        value.vpn.state = VpnState::Unknown;
        value.vpn.provider = provider.into();
        value.downloader.state = if downloader_running {
            DownloaderState::Running
        } else {
            DownloaderState::Stopped
        };
        value
    }
    pub fn stopping(provider: &str) -> Self {
        let mut value = Self::degraded(provider, "Stopping downloads and the VPN.", false);
        value.state = State::Stopping;
        value
    }
}

#[cfg(test)]
mod tests {
    use crate::status::{DownloaderState, State, Status, Verification};
    #[test]
    fn tunnel_health_alone_never_claims_protection() {
        let status = Status::ready("test", None, false, true);
        assert_eq!(status.state, State::Degraded);
        assert_eq!(status.downloader.state, DownloaderState::Stopped);
        assert_eq!(status.protection.kill_switch, Verification::Failed);
    }
    #[test]
    fn unavailable_health_is_not_a_confirmed_disconnection() {
        let status = Status::degraded("test", "VPN health is unavailable.", true);
        assert_ne!(status.vpn.state, crate::status::VpnState::Disconnected);
    }
}
