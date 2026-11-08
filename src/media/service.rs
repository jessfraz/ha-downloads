use crate::{
    config::MediaConfig,
    media::{model::*, store::Store, upstream::*},
    status::Status,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, RwLock, watch};

#[derive(Default)]
pub(crate) struct Cache {
    pub movies: Vec<UpMovie>,
    pub series: Vec<UpSeries>,
    pub movie_at: u64,
    pub series_at: u64,
    pub movie_queue: Vec<UpQueue>,
    pub series_queue: Vec<UpQueue>,
    pub movie_queue_at: u64,
    pub series_queue_at: u64,
    pub searches: BTreeMap<String, (Vec<MediaItem>, u64)>,
    pub roots: BTreeMap<ProfileId, RootObservation>,
    pub download_storage: Option<(bool, u64)>,
}
pub(crate) struct RootObservation {
    pub root: RootFolderId,
    pub accessible: bool,
    pub observed_at_unix_ms: u64,
}
pub struct MediaService {
    pub(crate) config: MediaConfig,
    pub(crate) store: Store,
    pub(crate) radarr: Upstream,
    pub(crate) sonarr: Upstream,
    pub(crate) prowlarr: Upstream,
    pub(crate) status: watch::Receiver<Status>,
    pub(crate) cache: RwLock<Cache>,
    pub(crate) health: RwLock<Vec<ComponentHealth>>,
    pub(crate) operations: Mutex<()>,
    pub(crate) egress: crate::media::upstream::EgressBoundary,
    accepting: AtomicBool,
}
impl MediaService {
    pub async fn open(
        config: MediaConfig,
        directory: PathBuf,
        status: watch::Receiver<Status>,
        local_subnets: Vec<ipnet::IpNet>,
    ) -> Result<Arc<Self>, ApiErrorCode> {
        config
            .validate("")
            .map_err(|_| ApiErrorCode::InvalidArguments)?;
        let store = Store::open(directory).await?;
        let egress = crate::media::upstream::EgressBoundary::new(local_subnets, status.clone())?;
        Ok(Arc::new(Self {
            radarr: Upstream::new(config.radarr.clone(), "api/v3", egress.clone())?,
            sonarr: Upstream::new(config.sonarr.clone(), "api/v3", egress.clone())?,
            prowlarr: Upstream::new(config.prowlarr.clone(), "api/v1", egress.clone())?,
            config,
            store,
            status,
            cache: RwLock::new(Cache::default()),
            health: RwLock::new(Vec::new()),
            operations: Mutex::new(()),
            accepting: AtomicBool::new(true),
            egress,
        }))
    }
    pub fn stop_accepting(&self) {
        self.accepting.store(false, Ordering::Release)
    }
    pub fn accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
    }
    pub(crate) fn manager(&self, id: ManagerId) -> &Upstream {
        match id {
            ManagerId::Radarr => &self.radarr,
            ManagerId::Sonarr => &self.sonarr,
        }
    }
    pub async fn profiles(&self, kind: Option<MediaKind>) -> ProfileList {
        ProfileList {
            items: self
                .store
                .snapshot()
                .await
                .profiles
                .into_iter()
                .filter(|p| kind.is_none_or(|kind| p.kind() == kind))
                .collect(),
        }
    }
    pub async fn profile(&self, id: &ProfileId) -> Result<DownloadProfile, ApiErrorCode> {
        self.store
            .snapshot()
            .await
            .profiles
            .into_iter()
            .find(|p| p.id() == id)
            .ok_or(ApiErrorCode::NotFound)
    }
    pub async fn managers(&self) -> ManagerList {
        let (radarr, sonarr) = tokio::join!(
            self.radarr.get::<UpStatus>("system/status", &[]),
            self.sonarr.get::<UpStatus>("system/status", &[])
        );
        ManagerList {
            items: vec![
                ManagerSummary {
                    manager: ManagerId::Radarr,
                    configured: self.radarr.configured(),
                    available: radarr.is_ok(),
                    version: radarr.ok().map(|s| safe_text(&s.version, 80)),
                },
                ManagerSummary {
                    manager: ManagerId::Sonarr,
                    configured: self.sonarr.configured(),
                    available: sonarr.is_ok(),
                    version: sonarr.ok().map(|s| safe_text(&s.version, 80)),
                },
            ],
        }
    }
    pub async fn options(&self, id: ManagerId) -> Result<ManagerOptions, ApiErrorCode> {
        let up = self.manager(id);
        let (quality, roots, tags) = tokio::try_join!(
            up.get::<Vec<UpNamed>>("qualityprofile", &[]),
            up.get::<Vec<UpRoot>>("rootfolder", &[]),
            up.get::<Vec<UpTag>>("tag", &[])
        )?;
        if quality.len() > 200 || roots.len() > 100 || tags.len() > 1000 {
            return Err(ApiErrorCode::InvalidUpstreamResponse);
        }
        Ok(ManagerOptions {
            manager: id,
            quality_profiles: quality
                .into_iter()
                .map(|q| NamedOption {
                    id: q.id,
                    name: safe_text(&q.name, 100),
                })
                .collect(),
            root_folders: roots
                .into_iter()
                .map(|r| {
                    Ok(RootFolderOption {
                        id: RootFolderId::try_from(r.id)
                            .map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?,
                        name: format!("Library {}", r.id),
                        accessible: r.accessible,
                    })
                })
                .collect::<Result<_, ApiErrorCode>>()?,
            tags: tags
                .into_iter()
                .map(|t| NamedOption {
                    id: t.id,
                    name: safe_text(&t.label, 100),
                })
                .collect(),
            series_types: if id == ManagerId::Sonarr {
                vec![SeriesType::Standard, SeriesType::Daily, SeriesType::Anime]
            } else {
                vec![]
            },
        })
    }
    pub(crate) async fn validate_settings(
        &self,
        input: &ProfileInput,
    ) -> Result<String, ApiErrorCode> {
        let (manager, quality, root, tags) = match input {
            ProfileInput::Movie { settings, .. } => (
                ManagerId::Radarr,
                settings.quality_profile_id,
                settings.root_folder_id,
                &settings.tags,
            ),
            ProfileInput::Series { settings, .. } => (
                ManagerId::Sonarr,
                settings.quality_profile_id,
                settings.root_folder_id,
                &settings.tags,
            ),
        };
        let up = self.manager(manager);
        let (qualities, roots, available_tags) = tokio::try_join!(
            up.get::<Vec<UpNamed>>("qualityprofile", &[]),
            up.get::<Vec<UpRoot>>("rootfolder", &[]),
            up.get::<Vec<UpTag>>("tag", &[])
        )?;
        if !qualities.iter().any(|q| q.id == quality.get())
            || tags.len() > 100
            || tags.iter().enumerate().any(|(i, t)| {
                tags[..i].contains(t) || !available_tags.iter().any(|a| a.id == t.get())
            })
        {
            return Err(ApiErrorCode::InvalidArguments);
        }
        let folder = roots
            .into_iter()
            .find(|r| r.id == root.get())
            .ok_or(ApiErrorCode::InvalidArguments)?;
        if !folder.accessible {
            return Err(ApiErrorCode::UpstreamUnavailable);
        }
        Ok(folder.path)
    }
    pub async fn create_profile(
        &self,
        actor: &str,
        key: &str,
        input: ProfileInput,
    ) -> Result<DownloadProfile, ApiErrorCode> {
        let key_hash = hash(&[key]);
        let fingerprint = fingerprint(&input)?;
        let _guard = self.operations.lock().await;
        let snapshot = self.store.snapshot().await;
        if let Some(receipt) = snapshot
            .profile_creates
            .iter()
            .find(|r| r.actor == actor && r.key_hash == key_hash)
        {
            if receipt.fingerprint != fingerprint {
                return Err(ApiErrorCode::IdempotencyConflict);
            }
            return snapshot
                .profiles
                .into_iter()
                .find(|p| p.id() == &receipt.id)
                .ok_or(ApiErrorCode::NotFound);
        }
        self.validate_settings(&input).await?;
        let id = ProfileId::from_digest(digest(&["profile", actor, key]));
        let revision = Revision::new(1).ok_or(ApiErrorCode::InvalidArguments)?;
        let profile = make_profile(id.clone(), revision, input);
        self.store
            .update(|state| {
                if profile.is_default() {
                    clear_default(state, &profile)?
                }
                state.profiles.push(profile.clone());
                state
                    .profile_creates
                    .push(crate::media::store::ProfileCreateReceipt {
                        actor: actor.into(),
                        key_hash,
                        fingerprint,
                        id,
                    });
                Ok(profile)
            })
            .await
    }
    pub async fn replace_profile(
        &self,
        id: &ProfileId,
        revision: Revision,
        input: ProfileInput,
    ) -> Result<DownloadProfile, ApiErrorCode> {
        let _guard = self.operations.lock().await;
        let current = self.profile(id).await?;
        if current.revision() != revision {
            return Err(ApiErrorCode::RevisionConflict);
        }
        self.validate_settings(&input).await?;
        let profile = make_profile(
            id.clone(),
            revision.next().ok_or(ApiErrorCode::RevisionConflict)?,
            input,
        );
        if profile.kind() != current.kind() {
            return Err(ApiErrorCode::InvalidArguments);
        }
        self.store
            .update(|state| {
                if profile.is_default() {
                    clear_default(state, &profile)?
                }
                let slot = state
                    .profiles
                    .iter_mut()
                    .find(|p| p.id() == id)
                    .ok_or(ApiErrorCode::NotFound)?;
                *slot = profile.clone();
                Ok(profile)
            })
            .await
    }
    pub async fn run(self: Arc<Self>, shutdown: watch::Receiver<bool>) {
        tokio::join!(
            self.health_loop(shutdown.clone()),
            self.request_loop(shutdown)
        );
    }
}
fn make_profile(id: ProfileId, revision: Revision, input: ProfileInput) -> DownloadProfile {
    match input {
        ProfileInput::Movie {
            name,
            is_default,
            settings,
        } => DownloadProfile::Movie {
            id,
            revision,
            name,
            is_default,
            settings,
        },
        ProfileInput::Series {
            name,
            is_default,
            settings,
        } => DownloadProfile::Series {
            id,
            revision,
            name,
            is_default,
            settings,
        },
    }
}
fn clear_default(
    state: &mut crate::media::store::DurableState,
    profile: &DownloadProfile,
) -> Result<(), ApiErrorCode> {
    for existing in &mut state.profiles {
        if existing.kind() == profile.kind()
            && existing.id() != profile.id()
            && existing.is_default()
        {
            existing.set_default(false);
            match existing {
                DownloadProfile::Movie { revision, .. }
                | DownloadProfile::Series { revision, .. } => {
                    *revision = revision.next().ok_or(ApiErrorCode::RevisionConflict)?
                }
            }
        }
    }
    Ok(())
}
pub(crate) fn digest(parts: &[&str]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize().into()
}
pub(crate) fn hash(parts: &[&str]) -> String {
    digest(parts).iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) fn fingerprint<T: serde::Serialize>(value: &T) -> Result<String, ApiErrorCode> {
    let body = serde_json::to_string(value).map_err(|_| ApiErrorCode::InvalidArguments)?;
    Ok(hash(&[&body]))
}
pub(crate) fn safe_text(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(max)
        .collect()
}

pub fn page<T>(
    mut items: Vec<T>,
    limit: Option<u32>,
    token: Option<&str>,
    scope: &str,
    key: impl Fn(&T) -> String,
    coverage: Vec<SourceCoverage>,
) -> Result<Page<T>, ApiErrorCode> {
    let limit = limit.unwrap_or(20);
    if !(1..=100).contains(&limit) {
        return Err(ApiErrorCode::InvalidArguments);
    }
    let signature = hash(&[scope]);
    let after = if let Some(token) = token {
        let (prefix, after) = token
            .split_once('.')
            .ok_or(ApiErrorCode::InvalidArguments)?;
        if prefix != signature
            || after.len() > 200
            || after.is_empty()
            || !after
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'-' | b'_'))
        {
            return Err(ApiErrorCode::InvalidArguments);
        }
        Some(after)
    } else {
        None
    };
    items.sort_by_key(&key);
    if let Some(after) = after {
        items.retain(|item| key(item).as_str() > after)
    }
    let more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next_page = if more {
        items
            .last()
            .map(|item| format!("{signature}.{}", key(item)))
    } else {
        None
    };
    Ok(Page {
        items,
        next_page,
        coverage,
    })
}
