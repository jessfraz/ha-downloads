use crate::monitor::{Monitoring, ServiceIncident};
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

/// Native API snapshot shared by the producer and its typed CLI client.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct StatusSnapshot {
    #[serde(flatten)]
    pub status: Status,
    pub health: Health,
}

/// Functional readiness is separate from `/healthz`, which serves the watchdog.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct Health {
    pub ready: bool,
    pub state: State,
    pub reasons: Vec<HealthReason>,
    pub checked_at_unix_ms: Option<u64>,
    pub age_ms: Option<u64>,
    pub max_age_ms: u64,
    pub last_failure: Option<ServiceIncident>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthReason {
    NotReady,
    ChecksMissing,
    ChecksStale,
    VpnUnavailable,
    FirewallUnverified,
    DnsUnverified,
    DownloaderUnavailable,
    DownloaderModulesUnavailable,
}

impl Status {
    pub fn health(&self) -> Health {
        self.health_at(crate::monitor::unix_ms())
    }

    fn health_at(&self, now_unix_ms: u64) -> Health {
        // The runtime probes every three seconds; allow bounded probe time and
        // scheduler delays, without treating a frozen snapshot as healthy.
        const MAX_AGE_MS: u64 = 30_000;
        let mut reasons = Vec::new();
        if self.state != State::Ready {
            reasons.push(HealthReason::NotReady);
        }
        let checked_at = self
            .diagnostics
            .checked_at_unix_ms
            .zip(self.diagnostics.downloader_checked_at_unix_ms)
            .map(|(protection, downloader)| protection.min(downloader));
        let future = self
            .diagnostics
            .checked_at_unix_ms
            .is_some_and(|time| time > now_unix_ms)
            || self
                .diagnostics
                .downloader_checked_at_unix_ms
                .is_some_and(|time| time > now_unix_ms);
        let age = checked_at
            .filter(|_| !future)
            .and_then(|checked| now_unix_ms.checked_sub(checked));
        if checked_at.is_none() {
            reasons.push(HealthReason::ChecksMissing);
        } else if age.is_none_or(|age| age > MAX_AGE_MS) {
            reasons.push(HealthReason::ChecksStale);
        }
        if self.vpn.state != VpnState::Connected
            || !self.diagnostics.checks.vpn_service.passed()
            || !self.diagnostics.checks.vpn_health.passed()
        {
            reasons.push(HealthReason::VpnUnavailable);
        }
        if self.protection.kill_switch != Verification::Verified
            || !self.diagnostics.checks.firewall.passed()
        {
            reasons.push(HealthReason::FirewallUnverified);
        }
        if self.protection.dns != Verification::Verified
            || !self.diagnostics.checks.resolver.passed()
            || !self.diagnostics.checks.dns_service.passed()
            || !self.diagnostics.checks.dns_lookup.passed()
        {
            reasons.push(HealthReason::DnsUnverified);
        }
        if self.downloader.state != DownloaderState::Running
            || !self.diagnostics.downloader_check.passed()
        {
            reasons.push(
                if self.diagnostics.downloader_check.outcome
                    == crate::monitor::Outcome::ModulesUnavailable
                {
                    HealthReason::DownloaderModulesUnavailable
                } else {
                    HealthReason::DownloaderUnavailable
                },
            );
        }
        Health {
            ready: reasons.is_empty(),
            state: self.state.clone(),
            reasons,
            checked_at_unix_ms: checked_at,
            age_ms: age,
            max_age_ms: MAX_AGE_MS,
            last_failure: self.diagnostics.last_failure.clone(),
        }
    }

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
    use crate::{
        monitor::{Check, Checks, Outcome},
        status::{DownloaderState, HealthReason, State, Status, Verification},
    };

    fn checked_status() -> Status {
        let passed = Check {
            outcome: Outcome::Passed,
            ..Check::default()
        };
        let mut status = Status::ready("test", None, true, true);
        status.diagnostics.checks = Checks {
            vpn_service: passed,
            vpn_health: passed,
            dns_service: passed,
            dns_lookup: passed,
            resolver: passed,
            firewall: passed,
        };
        status.diagnostics.checked_at_unix_ms = Some(100_000);
        status.diagnostics.downloader_check = passed;
        status.diagnostics.downloader_checked_at_unix_ms = Some(101_000);
        status
    }

    #[test]
    fn readiness_requires_current_functional_checks_and_ignores_queue_pause() {
        let mut status = checked_status();
        for paused in [None, Some(false), Some(true)] {
            status.downloader.paused = paused;
            assert!(status.health_at(130_000).ready);
        }
        let stale = status.health_at(130_001);
        assert!(!stale.ready);
        assert_eq!(stale.reasons, vec![HealthReason::ChecksStale]);
        assert_eq!(stale.checked_at_unix_ms, Some(100_000));
        assert_eq!(stale.age_ms, Some(30_001));
        assert_eq!(
            status.health_at(99_999).reasons,
            vec![HealthReason::ChecksStale]
        );
        status.diagnostics.downloader_checked_at_unix_ms = Some(200_000);
        assert_eq!(
            status.health_at(102_000).reasons,
            vec![HealthReason::ChecksStale]
        );
        status.diagnostics.downloader_checked_at_unix_ms = None;
        assert_eq!(
            status.health_at(102_000).reasons,
            vec![HealthReason::ChecksMissing]
        );
    }

    #[test]
    fn readiness_does_not_accept_stale_flags_or_a_hung_downloader() {
        let mut status = checked_status();
        status.diagnostics.checks.firewall.outcome = Outcome::FirewallFailed;
        assert_eq!(
            status.health_at(102_000).reasons,
            vec![HealthReason::FirewallUnverified]
        );
        status.diagnostics.checks.firewall.outcome = Outcome::Passed;
        status.diagnostics.downloader_check.outcome = Outcome::Timeout;
        assert_eq!(
            status.health_at(102_000).reasons,
            vec![HealthReason::DownloaderUnavailable]
        );
        status.diagnostics.downloader_check.outcome = Outcome::ModulesUnavailable;
        assert_eq!(
            status.health_at(102_000).reasons,
            vec![HealthReason::DownloaderModulesUnavailable]
        );
        status.diagnostics.downloader_check.outcome = Outcome::Passed;
        status.state = State::Degraded;
        assert_eq!(
            status.health_at(102_000).reasons,
            vec![HealthReason::NotReady]
        );
    }
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
