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
    pub roots: BTreeMap<ManagerId, RootObservation>,
    pub download_storage: Option<(bool, u64)>,
    pub imports: BTreeMap<ManagerId, (Vec<crate::media::imports::NativeImport>, u64)>,
}
pub(crate) struct RootObservation {
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
    pub(crate) updates: crate::media::supervisor::UpdateSource,
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
            updates: crate::media::supervisor::UpdateSource::from_environment(),
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
    pub async fn run(self: Arc<Self>, shutdown: watch::Receiver<bool>) {
        tokio::join!(
            self.health_loop(shutdown.clone()),
            self.request_loop(shutdown)
        );
    }
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
