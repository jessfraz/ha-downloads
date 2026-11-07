use crate::{
    media::{
        model::*,
        service::{MediaService, RootObservation},
        upstream::{UpHealth, UpRoot, UpStatus, read_bytes},
    },
    status::{DownloaderState, Verification, VpnState},
};
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::watch;

const TTL: u64 = 45_000;
impl MediaService {
    pub(crate) async fn health_loop(&self, mut stop: watch::Receiver<bool>) {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {_ = stop.changed()=>return,_=interval.tick()=>{tokio::select!{_=stop.changed()=>return,_=self.refresh_health()=>{}}}}
        }
    }
    async fn refresh_health(&self) {
        let (radarr, sonarr, prowlarr, sab) = tokio::join!(
            manager_health(&self.radarr),
            manager_health(&self.sonarr),
            manager_health(&self.prowlarr),
            self.sab_health()
        );
        let now = crate::monitor::unix_ms();
        let mut components = vec![
            observed(Component::Radarr, radarr, now, TTL),
            observed(Component::Sonarr, sonarr, now, TTL),
            observed(Component::Prowlarr, prowlarr, now, TTL),
            observed(Component::Sabnzbd, sab, now, TTL),
        ];
        let roots = self.storage_health().await;
        components.push(observed(
            Component::Storage,
            roots,
            crate::monitor::unix_ms(),
            TTL,
        ));
        components.push(observed(
            Component::Plex,
            self.plex_health().await,
            crate::monitor::unix_ms(),
            TTL,
        ));
        *self.health.write().await = components;
    }
    async fn sab_health(&self) -> HealthOutcome {
        if self.status.borrow().downloader.state != DownloaderState::Running {
            return HealthOutcome::Unavailable {
                reason: UnavailableReason::NotRunning,
            };
        }
        let key = match tokio::fs::read_to_string("/data/sabnzbd/sabnzbd.ini").await {
            Ok(contents) if contents.len() <= 1024 * 1024 => contents
                .lines()
                .filter_map(|line| line.split_once('='))
                .find(|(name, _)| name.trim() == "api_key")
                .map(|(_, value)| zeroize::Zeroizing::new(value.trim().to_owned())),
            _ => None,
        };
        let Some(key) = key.filter(|key| !key.is_empty()) else {
            return HealthOutcome::Unavailable {
                reason: UnavailableReason::Authentication,
            };
        };
        let result = async {
            let client = reqwest::Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()
                .map_err(|_| ())?;
            let response = client
                .get("http://127.0.0.1:8081/sabnzbd/api")
                .query(&[
                    ("mode", "queue"),
                    ("output", "json"),
                    ("limit", "1"),
                    ("apikey", key.as_str()),
                ])
                .send()
                .await
                .map_err(|_| ())?;
            if !response.status().is_success() {
                return Err(());
            }
            let bytes = read_bytes(response, 128 * 1024).await.map_err(|_| ())?;
            serde_json::from_slice::<SabResponse>(&bytes).map_err(|_| ())
        }
        .await;
        match result {
            Ok(response) => {
                // An intentional pause is not a fault. Native disk-space observations
                // concern download staging, not unrelated movie/TV destination roots.
                let storage = response
                    .queue
                    .diskspace1
                    .parse::<f64>()
                    .ok()
                    .zip(response.queue.diskspace2.parse::<f64>().ok())
                    .map(|(a, b)| a.is_finite() && b.is_finite() && a > 0.0 && b > 0.0);
                if let Some(ready) = storage {
                    self.cache.write().await.download_storage =
                        Some((ready, crate::monitor::unix_ms()));
                }
                HealthOutcome::Healthy
            }
            Err(()) => HealthOutcome::Unavailable {
                reason: UnavailableReason::Connection,
            },
        }
    }
    async fn storage_health(&self) -> HealthOutcome {
        let profiles = self.profiles(None).await.items;
        let (movies, series) = tokio::join!(
            self.radarr.get::<Vec<UpRoot>>("rootfolder", &[]),
            self.sonarr.get::<Vec<UpRoot>>("rootfolder", &[])
        );
        let now = crate::monitor::unix_ms();
        let mut issues = vec![];
        let mut cache = self.cache.write().await;
        for profile in profiles {
            let roots = match &profile {
                DownloadProfile::Movie { .. } => &movies,
                DownloadProfile::Series { .. } => &series,
            };
            let root = profile.root_folder_id();
            match roots {
                Ok(roots) => {
                    let accessible = roots
                        .iter()
                        .any(|r| r.id == root.get() && r.accessible && r.free_space != Some(0));
                    cache.roots.insert(
                        profile.id().clone(),
                        RootObservation {
                            root,
                            accessible,
                            observed_at_unix_ms: now,
                        },
                    );
                    if !accessible {
                        issues.push(issue(
                            HealthIssueCode::StorageUnavailable,
                            "A configured library is unavailable or full.",
                        ));
                    }
                }
                Err(_) => {
                    cache.roots.insert(
                        profile.id().clone(),
                        RootObservation {
                            root,
                            accessible: false,
                            observed_at_unix_ms: now,
                        },
                    );
                    issues.push(issue(
                        HealthIssueCode::UpstreamUnavailable,
                        "Library accessibility cannot be verified.",
                    ));
                }
            }
        }
        match cache.download_storage {
            Some((false, _)) => issues.push(issue(
                HealthIssueCode::StorageLow,
                "Download staging has no free space.",
            )),
            Some((true, at)) if now.checked_sub(at).is_some_and(|age| age <= TTL) => {}
            _ => issues.push(issue(
                HealthIssueCode::ChecksMissing,
                "Download staging accessibility cannot be verified.",
            )),
        }
        if issues.is_empty() {
            HealthOutcome::Healthy
        } else {
            HealthOutcome::Degraded { issues }
        }
    }
    async fn plex_health(&self) -> HealthOutcome {
        let connection = &self.config.plex;
        if connection.url.is_empty() {
            return HealthOutcome::NotConfigured;
        }
        let result = async {
            let url = reqwest::Url::parse(&format!(
                "{}/identity",
                connection.url.trim_end_matches('/')
            ))
            .map_err(|_| ())?;
            self.egress.authorize(&url).map_err(|_| ())?;
            let client = reqwest::Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()
                .map_err(|_| ())?;
            let response = client
                .get(url)
                .header("X-Plex-Token", &*connection.api_key)
                .header("Accept", "application/json")
                .send()
                .await
                .map_err(|_| ())?;
            if !response.status().is_success() {
                return Err(());
            }
            let bytes = read_bytes(response, 64 * 1024).await.map_err(|_| ())?;
            serde_json::from_slice::<PlexIdentity>(&bytes).map_err(|_| ())
        }
        .await;
        match result {
            Ok(identity) if !identity.container.version.is_empty() => HealthOutcome::Healthy,
            _ => HealthOutcome::Unavailable {
                reason: UnavailableReason::Connection,
            },
        }
    }
    pub async fn health_snapshot(
        &self,
        operation: Option<Operation>,
        profile_id: Option<&ProfileId>,
    ) -> Result<HealthSnapshot, ApiErrorCode> {
        if operation.is_none() && profile_id.is_some() {
            return Err(ApiErrorCode::InvalidArguments);
        }
        let requested_operation = operation;
        let now = crate::monitor::unix_ms();
        let mut components = self.health.read().await.clone();
        for component in [
            Component::Radarr,
            Component::Sonarr,
            Component::Prowlarr,
            Component::Sabnzbd,
            Component::Storage,
            Component::Plex,
        ] {
            if !components.iter().any(|c| c.component == component) {
                components.push(ComponentHealth {
                    component,
                    observation: Observation::NeverChecked,
                });
            }
        }
        let status = self.status.borrow().clone();
        for (component, healthy) in [
            (
                Component::Vpn,
                status.vpn.state == VpnState::Connected
                    && status.diagnostics.checks.vpn_health.passed()
                    && status.diagnostics.checks.vpn_service.passed(),
            ),
            (
                Component::Firewall,
                status.protection.kill_switch == Verification::Verified
                    && status.diagnostics.checks.firewall.passed(),
            ),
            (
                Component::Dns,
                status.protection.dns == Verification::Verified
                    && status.diagnostics.checks.dns_lookup.passed()
                    && status.diagnostics.checks.resolver.passed()
                    && status.diagnostics.checks.dns_service.passed(),
            ),
        ] {
            components.push(match status.diagnostics.checked_at_unix_ms {
                Some(at) => observed(
                    component,
                    if healthy {
                        HealthOutcome::Healthy
                    } else {
                        HealthOutcome::Degraded {
                            issues: vec![issue(
                                HealthIssueCode::ProtectionUnverified,
                                "Protection checks have not passed.",
                            )],
                        }
                    },
                    at,
                    30_000,
                ),
                None => ComponentHealth {
                    component,
                    observation: Observation::NeverChecked,
                },
            });
        }
        for component in &mut components {
            if component.component == Component::Sabnzbd {
                // Queue access and free staging space do not prove SAB's
                // native decoder modules are usable. Keep both observations.
                match status.diagnostics.downloader_checked_at_unix_ms {
                    None => component.observation = Observation::NeverChecked,
                    Some(at) => {
                        if let Observation::Observed {
                            outcome,
                            observed_at_unix_ms,
                            expires_at_unix_ms,
                            ..
                        } = &mut component.observation
                        {
                            *expires_at_unix_ms =
                                (*expires_at_unix_ms).min(at.saturating_add(30_000));
                            if at > now || *observed_at_unix_ms > now {
                                *expires_at_unix_ms = 0;
                            }
                            *observed_at_unix_ms = (*observed_at_unix_ms).min(at);
                            if status.downloader.state != DownloaderState::Running
                                || !status.diagnostics.downloader_check.passed()
                            {
                                *outcome = HealthOutcome::Degraded {
                                    issues: vec![issue(
                                        HealthIssueCode::DownloadClientUnavailable,
                                        "Downloader native checks have not passed.",
                                    )],
                                };
                            }
                        }
                    }
                }
            }
            if let Observation::Observed {
                freshness,
                observed_at_unix_ms,
                expires_at_unix_ms,
                ..
            } = &mut component.observation
            {
                *freshness = if now >= *observed_at_unix_ms && now <= *expires_at_unix_ms {
                    Freshness::Fresh
                } else {
                    Freshness::Stale
                };
            }
        }
        let profiles = self.profiles(None).await.items;
        let cache = self.cache.read().await;
        let mut readiness = vec![];
        let operations = operation.map_or_else(
            || {
                vec![
                    Operation::RequestMovie,
                    Operation::RequestSeries,
                    Operation::Transfer,
                    Operation::ImportMovie,
                    Operation::ImportSeries,
                ]
            },
            |op| vec![op],
        );
        for operation in operations {
            let mut reasons = vec![];
            let mut selected = None;
            let mut expires = now.saturating_add(TTL);
            if operation == Operation::Transfer {
                for component in [
                    Component::Vpn,
                    Component::Firewall,
                    Component::Dns,
                    Component::Sabnzbd,
                ] {
                    expires = expires.min(component_expiry(&components, component));
                    if !component_ready(&components, component) {
                        reasons.push(
                            if matches!(
                                component,
                                Component::Vpn | Component::Firewall | Component::Dns
                            ) {
                                HealthIssueCode::ProtectionUnverified
                            } else {
                                HealthIssueCode::UpstreamUnavailable
                            },
                        );
                    }
                }
                expires = expires.min(
                    cache
                        .download_storage
                        .map_or(0, |(_, at)| at.saturating_add(TTL)),
                );
                if !cache.download_storage.is_some_and(|(ready, at)| {
                    ready && now.checked_sub(at).is_some_and(|age| age <= TTL)
                }) {
                    reasons.push(HealthIssueCode::StorageUnavailable)
                }
            } else {
                let kind = if matches!(operation, Operation::RequestMovie | Operation::ImportMovie)
                {
                    MediaKind::Movie
                } else {
                    MediaKind::Series
                };
                let candidates = profiles
                    .iter()
                    .filter(|p| p.kind() == kind)
                    .collect::<Vec<_>>();
                let profile = if let Some(id) = profile_id {
                    Some(
                        profiles
                            .iter()
                            .find(|p| p.id() == id && p.kind() == kind)
                            .ok_or(ApiErrorCode::InvalidArguments)?,
                    )
                } else {
                    candidates
                        .iter()
                        .find(|p| p.is_default())
                        .copied()
                        .or_else(|| {
                            if candidates.len() == 1 {
                                candidates.first().copied()
                            } else {
                                None
                            }
                        })
                };
                if profile.is_none() {
                    if requested_operation.is_some() && profile_id.is_none() && candidates.len() > 1
                    {
                        return Err(ApiErrorCode::AmbiguousOperationTarget);
                    }
                    reasons.push(HealthIssueCode::NotConfigured)
                }
                if let Some(profile) = profile {
                    selected = Some(profile.id().clone());
                    // A profile can change while a probe is in flight. Only
                    // evidence for its current native root proves readiness.
                    let root = cache
                        .roots
                        .get(profile.id())
                        .filter(|root| root.root == profile.root_folder_id());
                    expires = expires
                        .min(root.map_or(0, |root| root.observed_at_unix_ms.saturating_add(TTL)));
                    if root.is_none() {
                        reasons.push(HealthIssueCode::ChecksMissing);
                    } else if !root.is_some_and(|root| {
                        root.accessible
                            && now
                                .checked_sub(root.observed_at_unix_ms)
                                .is_some_and(|age| age <= TTL)
                    }) {
                        reasons.push(HealthIssueCode::StorageUnavailable)
                    }
                }
                let component = if kind == MediaKind::Movie {
                    Component::Radarr
                } else {
                    Component::Sonarr
                };
                expires = expires.min(component_expiry(&components, component));
                if !manager_ready(&components, component) {
                    reasons.push(HealthIssueCode::UpstreamUnavailable)
                }
                if matches!(operation, Operation::ImportMovie | Operation::ImportSeries) {
                    expires = expires.min(
                        cache
                            .download_storage
                            .map_or(0, |(_, at)| at.saturating_add(TTL)),
                    );
                    if !cache.download_storage.is_some_and(|(ready, at)| {
                        ready && now.checked_sub(at).is_some_and(|age| age <= TTL)
                    }) {
                        reasons.push(HealthIssueCode::StorageUnavailable)
                    }
                }
            }
            reasons.sort_unstable();
            reasons.dedup();
            readiness.push(OperationReadiness {
                operation,
                profile_id: selected,
                state: if reasons.is_empty() {
                    ReadinessState::Ready
                } else {
                    ReadinessState::Blocked
                },
                reasons,
                expires_at_unix_ms: expires,
            });
        }
        components.sort_by_key(|c| c.component);
        Ok(HealthSnapshot {
            components,
            readiness,
            generated_at_unix_ms: now,
        })
    }
}
fn component_ready(components: &[ComponentHealth], component: Component) -> bool {
    components.iter().any(|c|c.component==component&&matches!(&c.observation,Observation::Observed{outcome:HealthOutcome::Healthy,freshness:Freshness::Fresh,..})||c.component==component&&matches!(&c.observation,Observation::Observed{outcome:HealthOutcome::Degraded{issues},freshness:Freshness::Fresh,..} if issues.iter().all(|issue|issue.severity==IssueSeverity::Warning)))
}
fn manager_ready(components: &[ComponentHealth], component: Component) -> bool {
    components.iter().any(|item| {
        item.component == component
            && match &item.observation {
                Observation::Observed {
                    outcome: HealthOutcome::Healthy,
                    freshness: Freshness::Fresh,
                    ..
                } => true,
                Observation::Observed {
                    outcome: HealthOutcome::Degraded { issues },
                    freshness: Freshness::Fresh,
                    ..
                } => issues.iter().all(|issue| {
                    issue.severity == IssueSeverity::Warning
                        || matches!(
                            issue.code,
                            HealthIssueCode::StorageUnavailable | HealthIssueCode::StorageLow
                        )
                }),
                _ => false,
            }
    })
}
fn component_expiry(components: &[ComponentHealth], component: Component) -> u64 {
    components
        .iter()
        .find_map(|c| {
            if c.component == component {
                match c.observation {
                    Observation::Observed {
                        expires_at_unix_ms, ..
                    } => Some(expires_at_unix_ms),
                    Observation::NeverChecked => Some(0),
                }
            } else {
                None
            }
        })
        .unwrap_or(0)
}
fn observed(component: Component, outcome: HealthOutcome, at: u64, ttl: u64) -> ComponentHealth {
    ComponentHealth {
        component,
        observation: Observation::Observed {
            outcome,
            freshness: Freshness::Fresh,
            observed_at_unix_ms: at,
            expires_at_unix_ms: at.saturating_add(ttl),
        },
    }
}
fn issue(code: HealthIssueCode, message: &str) -> HealthIssue {
    HealthIssue {
        code,
        severity: IssueSeverity::Error,
        message: message.into(),
    }
}
async fn manager_health(up: &crate::media::upstream::Upstream) -> HealthOutcome {
    if !up.configured() {
        return HealthOutcome::NotConfigured;
    }
    let result = tokio::try_join!(
        up.get::<UpStatus>("system/status", &[]),
        up.get::<Vec<UpHealth>>("health", &[])
    );
    match result {
        Ok((_, health)) => {
            let mut issues = Vec::<HealthIssue>::new();
            for h in health {
                let source = h.source.to_ascii_lowercase();
                let code = if source.contains("indexer") {
                    HealthIssueCode::IndexerUnavailable
                } else if source.contains("downloadclient") {
                    HealthIssueCode::DownloadClientUnavailable
                } else if source.contains("rootfolder") || source.contains("disk") {
                    HealthIssueCode::StorageUnavailable
                } else {
                    HealthIssueCode::UpstreamHealth
                };
                let severity = if h.severity.eq_ignore_ascii_case("warning") {
                    IssueSeverity::Warning
                } else {
                    IssueSeverity::Error
                };
                if issues
                    .iter()
                    .any(|issue| issue.code == code && issue.severity == severity)
                {
                    continue;
                }
                issues.push(HealthIssue {
                    code,
                    severity,
                    message: match code {
                        HealthIssueCode::IndexerUnavailable => "An indexer needs attention.",
                        HealthIssueCode::DownloadClientUnavailable => {
                            "A download client needs attention."
                        }
                        HealthIssueCode::StorageUnavailable => "Storage needs attention.",
                        _ => "The service reported a health issue.",
                    }
                    .into(),
                });
            }
            if issues.is_empty() {
                HealthOutcome::Healthy
            } else {
                HealthOutcome::Degraded { issues }
            }
        }
        Err(ApiErrorCode::InvalidUpstreamResponse) => HealthOutcome::Unavailable {
            reason: UnavailableReason::InvalidResponse,
        },
        _ => HealthOutcome::Unavailable {
            reason: UnavailableReason::Connection,
        },
    }
}
#[derive(Deserialize)]
struct SabResponse {
    queue: SabQueue,
}
#[derive(Deserialize)]
struct SabQueue {
    diskspace1: String,
    diskspace2: String,
}
#[derive(Deserialize)]
struct PlexIdentity {
    #[serde(rename = "MediaContainer")]
    container: PlexContainer,
}
#[derive(Deserialize)]
struct PlexContainer {
    version: String,
}

#[cfg(test)]
mod tests {
    use crate::{
        config::MediaConfig,
        media::{
            model::*,
            service::{MediaService, RootObservation},
        },
        status::Status,
    };
    use tokio::sync::watch;

    #[tokio::test]
    async fn transfer_requires_fresh_native_downloader_checks_beside_queue_and_storage() {
        use crate::monitor::{Check, Checks, Outcome};
        let temp = tempfile::tempdir().unwrap();
        let now = crate::monitor::unix_ms();
        let passed = Check {
            outcome: Outcome::Passed,
            ..Check::default()
        };
        let mut status = Status::ready("fixture", None, true, true);
        status.diagnostics.checks = Checks {
            vpn_service: passed,
            vpn_health: passed,
            dns_service: passed,
            dns_lookup: passed,
            resolver: passed,
            firewall: passed,
        };
        status.diagnostics.checked_at_unix_ms = Some(now);
        status.diagnostics.downloader_check = passed;
        status.diagnostics.downloader_checked_at_unix_ms = Some(now);
        let (tx, rx) = watch::channel(status);
        let service = MediaService::open(
            MediaConfig::default(),
            temp.path().join("media"),
            rx,
            vec![],
        )
        .await
        .unwrap();
        *service.health.write().await = vec![super::observed(
            Component::Sabnzbd,
            HealthOutcome::Healthy,
            now,
            super::TTL,
        )];
        service.cache.write().await.download_storage = Some((true, now));
        let initial = service
            .health_snapshot(Some(Operation::Transfer), None)
            .await
            .unwrap();
        assert_eq!(initial.readiness[0].state, ReadinessState::Ready);
        for (outcome, at) in [
            (Outcome::ModulesUnavailable, Some(now)),
            (Outcome::Passed, Some(now.saturating_sub(30_001))),
            (Outcome::Passed, None),
            (Outcome::Passed, Some(now + 60_000)),
        ] {
            tx.send_modify(|status| {
                status.diagnostics.downloader_check.outcome = outcome;
                status.diagnostics.downloader_checked_at_unix_ms = at;
            });
            let snapshot = service
                .health_snapshot(Some(Operation::Transfer), None)
                .await
                .unwrap();
            assert_eq!(
                snapshot.readiness[0].state,
                ReadinessState::Blocked,
                "{outcome:?} at {at:?}"
            );
        }
    }

    #[tokio::test]
    async fn target_root_overrides_unrelated_storage_errors_but_probe_failure_invalidates_it() {
        let temp = tempfile::tempdir().unwrap();
        let (_tx, rx) = watch::channel(Status::needs_configuration("fixture"));
        let service = MediaService::open(
            MediaConfig::default(),
            temp.path().join("media"),
            rx,
            vec![],
        )
        .await
        .unwrap();
        let id: ProfileId = "276319ec-c797-460c-a49e-6b90fc1f6ae8".parse().unwrap();
        let profile = DownloadProfile::Movie {
            id: id.clone(),
            revision: Revision::new(1).unwrap(),
            name: "Movies".parse().unwrap(),
            is_default: true,
            settings: MovieSettings {
                quality_profile_id: QualityProfileId::try_from(1).unwrap(),
                root_folder_id: RootFolderId::try_from(1).unwrap(),
                tags: vec![],
                minimum_availability: MinimumAvailability::Released,
            },
        };
        service
            .store
            .update(|state| {
                state.profiles.push(profile);
                Ok(())
            })
            .await
            .unwrap();
        let now = crate::monitor::unix_ms();
        service.cache.write().await.roots.insert(
            id.clone(),
            RootObservation {
                root: RootFolderId::try_from(1).unwrap(),
                accessible: true,
                observed_at_unix_ms: now,
            },
        );
        *service.health.write().await = vec![super::observed(
            Component::Radarr,
            HealthOutcome::Degraded {
                issues: vec![super::issue(
                    HealthIssueCode::StorageUnavailable,
                    "An unrelated root is offline.",
                )],
            },
            now,
            super::TTL,
        )];
        let ready = service
            .health_snapshot(Some(Operation::RequestMovie), Some(&id))
            .await
            .unwrap();
        assert_eq!(ready.readiness[0].state, ReadinessState::Ready);
        // Native root lookup now fails. A prior success is history, not current
        // evidence, even when the manager's other health endpoints still work.
        service.storage_health().await;
        let blocked = service
            .health_snapshot(Some(Operation::RequestMovie), Some(&id))
            .await
            .unwrap();
        assert_eq!(blocked.readiness[0].state, ReadinessState::Blocked);
        assert!(
            blocked.readiness[0]
                .reasons
                .contains(&HealthIssueCode::StorageUnavailable)
        );
    }
}
