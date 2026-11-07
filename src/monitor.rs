//! Bounded, credential-free check results and connection history.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    #[default]
    NotChecked,
    Passed,
    Stopped,
    Timeout,
    TransportError,
    HttpError,
    InvalidResponse,
    HealthFailed,
    UnsafeResolver,
    DnsLookupFailed,
    FirewallFailed,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthReason {
    NotReady,
    Icmp,
    Dns,
    Tls,
    Timeout,
    Connection,
    Other,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportReason {
    Connect,
    ConnectionRefused,
    ConnectionReset,
    BrokenPipe,
    UnexpectedEof,
    Body,
    Other,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct Check {
    pub outcome: Outcome,
    pub http_status: Option<u16>,
    pub health_reason: Option<HealthReason>,
    pub transport_reason: Option<TransportReason>,
    pub elapsed_ms: u64,
}
impl Check {
    pub(crate) fn measured(outcome: Outcome, started: Instant) -> Self {
        Self {
            outcome,
            elapsed_ms: millis(started.elapsed()),
            ..Self::default()
        }
    }
    pub(crate) fn passed(self) -> bool {
        self.outcome == Outcome::Passed
    }
    fn same_result(self, other: Self) -> bool {
        self.outcome == other.outcome
            && self.http_status == other.http_status
            && self.health_reason == other.health_reason
            && self.transport_reason == other.transport_reason
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct Checks {
    pub vpn_service: Check,
    pub vpn_health: Check,
    pub dns_service: Check,
    pub dns_lookup: Check,
    pub resolver: Check,
    pub firewall: Check,
}
impl Checks {
    pub(crate) fn ready(self) -> bool {
        self.entries().into_iter().all(Check::passed)
    }
    fn entries(self) -> [Check; 6] {
        [
            self.vpn_service,
            self.vpn_health,
            self.dns_service,
            self.dns_lookup,
            self.resolver,
            self.firewall,
        ]
    }
    fn same_results(self, other: Self) -> bool {
        self.entries()
            .into_iter()
            .zip(other.entries())
            .all(|(a, b)| a.same_result(b))
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct Incident {
    pub started_at_unix_ms: u64,
    pub recovered_at_unix_ms: Option<u64>,
    pub duration_ms: u64,
    pub initial_checks: Checks,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct Monitoring {
    pub checked_at_unix_ms: Option<u64>,
    pub checks: Checks,
    pub incident_count: u64,
    pub last_incident: Option<Incident>,
}
#[derive(Default)]
pub(crate) struct Tracker {
    previous: Option<Checks>,
    changed: Option<Instant>,
    incident_started: Option<Instant>,
    monitoring: Monitoring,
    writer: Option<crate::diagnostics::NonblockingStderr>,
}
impl Tracker {
    pub(crate) fn new() -> Self {
        Self {
            writer: Some(crate::diagnostics::NonblockingStderr::open()),
            ..Self::default()
        }
    }
    pub(crate) fn observe(&mut self, checks: Checks) -> Monitoring {
        self.observe_at(checks, Instant::now(), unix_ms())
    }
    fn observe_at(&mut self, checks: Checks, now: Instant, timestamp: u64) -> Monitoring {
        self.monitoring.checked_at_unix_ms = Some(timestamp);
        self.monitoring.checks = checks;
        let recovered = checks.ready() && self.incident_started.is_some();
        if !checks.ready() && self.incident_started.is_none() {
            self.incident_started = Some(now);
            self.monitoring.incident_count = self.monitoring.incident_count.saturating_add(1);
            self.monitoring.last_incident = Some(Incident {
                started_at_unix_ms: timestamp,
                recovered_at_unix_ms: None,
                duration_ms: 0,
                initial_checks: checks,
            });
        }
        if let (Some(started), Some(incident)) = (
            self.incident_started,
            self.monitoring.last_incident.as_mut(),
        ) {
            incident.duration_ms = millis(now.saturating_duration_since(started));
            if recovered {
                incident.recovered_at_unix_ms = Some(timestamp);
                self.incident_started = None;
            }
        }
        if self
            .previous
            .is_none_or(|previous| !previous.same_results(checks))
        {
            #[derive(Serialize)]
            struct Event<'a> {
                event: &'static str,
                at_unix_ms: u64,
                since_previous_change_ms: u64,
                checks: Checks,
                incident_count: u64,
                last_incident: &'a Option<Incident>,
            }
            let event = Event {
                event: if recovered {
                    "connection_checks_recovered"
                } else {
                    "connection_checks_changed"
                },
                at_unix_ms: timestamp,
                since_previous_change_ms: self
                    .changed
                    .map_or(0, |t| millis(now.saturating_duration_since(t))),
                checks,
                incident_count: self.monitoring.incident_count,
                last_incident: &self.monitoring.last_incident,
            };
            if let Some(writer) = &mut self.writer {
                writer.record(&event);
            }
            self.changed = Some(now);
            self.previous = Some(checks);
        }
        self.monitoring.clone()
    }
}
fn millis(duration: std::time::Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}
pub(crate) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, millis)
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LifecycleEvent {
    StatusPersistenceFailed,
    ProtectionServiceFailed {
        reason: Option<ServiceFailure>,
    },
    ProtectionTaskStopped,
    DownloaderReadinessTimedOut {
        last_probe: Check,
    },
    VpnProcessExited {
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    DownloaderProcessExited {
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
}
/// Fixed failure reasons may cross the logging boundary; raw service errors may not.
#[derive(Clone, Copy, Debug, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum ServiceFailure {
    #[error("VPN service exited. Downloads have been stopped.")]
    VpnExited,
    #[error("Downloader exited unexpectedly. Downloads have been stopped.")]
    DownloaderExited,
    #[error("VPN protection checks did not pass before startup timed out.")]
    VpnNotReady,
    #[error("Downloader startup timed out. Downloads are stopped.")]
    DownloaderNotReady,
    #[error("Protection checks failed. Downloads have been stopped.")]
    ProtectionChecksFailed,
}
pub fn log_lifecycle(event: LifecycleEvent) {
    #[derive(Serialize)]
    struct Record {
        at_unix_ms: u64,
        #[serde(flatten)]
        event: LifecycleEvent,
    }
    crate::diagnostics::NonblockingStderr::open().record(&Record {
        at_unix_ms: unix_ms(),
        event,
    });
}

#[cfg(test)]
mod tests {
    use crate::monitor::{Check, Checks, Outcome, Tracker};
    use std::time::{Duration, Instant};
    fn healthy() -> Checks {
        let passed = Check {
            outcome: Outcome::Passed,
            ..Check::default()
        };
        Checks {
            vpn_service: passed,
            vpn_health: passed,
            dns_service: passed,
            dns_lookup: passed,
            resolver: passed,
            firewall: passed,
        }
    }
    #[test]
    fn production_logging_does_not_change_monitor_state() {
        let mut tracker = Tracker::new();
        assert!(tracker.observe(healthy()).checks.ready());
    }
    #[test]
    fn recovery_keeps_the_failure_and_uses_monotonic_duration() {
        let mut tracker = Tracker::default();
        let now = Instant::now();
        let good = healthy();
        tracker.observe_at(good, now, 10000);
        let mut bad = good;
        bad.vpn_health.outcome = Outcome::Timeout;
        let first = tracker.observe_at(bad, now + Duration::from_secs(2), 12000);
        assert_eq!(first.incident_count, 1);
        let recovered = tracker.observe_at(good, now + Duration::from_secs(9), 11000);
        let incident = recovered.last_incident.expect("history");
        assert_eq!(incident.duration_ms, 7000);
        assert_eq!(incident.recovered_at_unix_ms, Some(11000));
        assert_eq!(incident.initial_checks.vpn_health.outcome, Outcome::Timeout);
        assert_eq!(
            tracker
                .observe_at(good, now + Duration::from_secs(10), 13000)
                .incident_count,
            1
        );
    }
    #[test]
    fn changing_failures_belong_to_one_incident_and_timings_do_not_create_transitions() {
        let mut tracker = Tracker::default();
        let now = Instant::now();
        let mut bad = healthy();
        bad.dns_lookup.outcome = Outcome::DnsLookupFailed;
        tracker.observe_at(bad, now, 1);
        bad.vpn_health.outcome = Outcome::HealthFailed;
        assert_eq!(
            tracker
                .observe_at(bad, now + Duration::from_secs(1), 1001)
                .incident_count,
            1
        );
        let changed = tracker.changed;
        bad.vpn_health.elapsed_ms = 400;
        tracker.observe_at(bad, now + Duration::from_secs(2), 2001);
        assert_eq!(tracker.changed, changed);
    }
}
