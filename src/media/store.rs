//! One private, atomic journal. Profiles and request receipts are not a library mirror.
use crate::media::{
    model::*,
    upstream::{UpMovie, UpSeries},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::{io::AsyncWriteExt, sync::RwLock};

const LIMIT: usize = 32 * 1024 * 1024;
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DurableState {
    pub schema_version: u32,
    pub profiles: Vec<DownloadProfile>,
    pub profile_creates: Vec<ProfileCreateReceipt>,
    pub requests: Vec<StoredRequest>,
}
impl Default for DurableState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            profiles: vec![],
            profile_creates: vec![],
            requests: vec![],
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
pub struct ProfileCreateReceipt {
    pub actor: String,
    pub key_hash: String,
    pub fingerprint: String,
    pub id: ProfileId,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct StoredRequest {
    pub actor: String,
    pub key_hash: String,
    pub fingerprint: String,
    pub record: RequestRecord,
    pub plan: Plan,
    pub checkpoint: Checkpoint,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Plan {
    Movie {
        media: UpMovie,
        settings: MovieSettings,
        root: String,
        search: bool,
        existing_id: Option<u32>,
    },
    Series {
        media: UpSeries,
        settings: SeriesSettings,
        root: String,
        search: bool,
        selected_seasons: Vec<u16>,
        as_of: String,
        existing_id: Option<u32>,
    },
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum Checkpoint {
    Prepared,
    Adding,
    Added {
        upstream_id: u32,
        effect: RequestEffect,
    },
    Monitoring {
        upstream_id: u32,
        effect: RequestEffect,
        episode_ids: Vec<u32>,
    },
    Monitored {
        upstream_id: u32,
        effect: RequestEffect,
        episode_ids: Vec<u32>,
    },
    Searching {
        upstream_id: u32,
        effect: RequestEffect,
        command: crate::media::upstream::Command,
    },
    Complete,
}
pub struct Store {
    directory: PathBuf,
    state: RwLock<DurableState>,
}
impl Store {
    pub async fn open(directory: PathBuf) -> Result<Self, ApiErrorCode> {
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
        let metadata = tokio::fs::symlink_metadata(&directory)
            .await
            .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ApiErrorCode::InvalidArguments);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // SAFETY: geteuid has no parameters and cannot fail.
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(ApiErrorCode::PermissionDenied);
            }
            tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
        }
        let path = directory.join("journal.json");
        let state: DurableState = match tokio::fs::symlink_metadata(&path).await {
            Ok(meta) => {
                if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > LIMIT as u64 {
                    return Err(ApiErrorCode::InvalidArguments);
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    // SAFETY: geteuid has no parameters and cannot fail.
                    if meta.nlink() != 1
                        || meta.uid() != unsafe { libc::geteuid() }
                        || meta.mode() & 0o077 != 0
                    {
                        return Err(ApiErrorCode::PermissionDenied);
                    }
                }
                let bytes = tokio::fs::read(&path)
                    .await
                    .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
                serde_json::from_slice(&bytes).map_err(|_| ApiErrorCode::InvalidArguments)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DurableState::default(),
            Err(_) => return Err(ApiErrorCode::UpstreamUnavailable),
        };
        if state.schema_version != 1 || state.profiles.len() > 100 || state.requests.len() > 5000 {
            return Err(ApiErrorCode::InvalidArguments);
        }
        Ok(Self {
            directory,
            state: RwLock::new(state),
        })
    }
    pub async fn snapshot(&self) -> DurableState {
        self.state.read().await.clone()
    }
    pub async fn update<T>(
        &self,
        f: impl FnOnce(&mut DurableState) -> Result<T, ApiErrorCode>,
    ) -> Result<T, ApiErrorCode> {
        // HTTP mutations run in Dropshot's detached handler mode, so losing a
        // client connection cannot cancel this commit. The owned worker may be
        // cancelled only during shutdown; its pre-effect checkpoints make an
        // unpublished transition safe to reconcile on restart.
        let mut guard = self.state.write().await;
        let mut next = guard.clone();
        let result = f(&mut next)?;
        if next.profiles.len() > 100 || next.requests.len() > 5000 {
            return Err(ApiErrorCode::InvalidArguments);
        }
        let bytes = serde_json::to_vec(&next).map_err(|_| ApiErrorCode::InvalidArguments)?;
        if bytes.len() > LIMIT {
            return Err(ApiErrorCode::InvalidArguments);
        }
        persist(&self.directory, &bytes).await?;
        *guard = next;
        Ok(result)
    }
    pub async fn save_request(&self, request: StoredRequest) -> Result<(), ApiErrorCode> {
        self.update(|state| {
            let slot = state
                .requests
                .iter_mut()
                .find(|r| r.record.id == request.record.id)
                .ok_or(ApiErrorCode::NotFound)?;
            *slot = request;
            Ok(())
        })
        .await
    }
}
async fn persist(directory: &Path, bytes: &[u8]) -> Result<(), ApiErrorCode> {
    let temporary = directory.join(".journal.tmp");
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(&temporary)
        .await
        .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
    file.write_all(bytes)
        .await
        .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
    file.sync_all()
        .await
        .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
    drop(file);
    tokio::fs::rename(temporary, directory.join("journal.json"))
        .await
        .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
    let directory = tokio::fs::File::open(directory)
        .await
        .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
    directory
        .sync_all()
        .await
        .map_err(|_| ApiErrorCode::UpstreamUnavailable)
}

#[cfg(test)]
mod tests {
    use crate::media::{model::*, store::Store};
    #[tokio::test]
    async fn committed_profiles_survive_reopen_and_failed_mutations_do_not_apply() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("media");
        let store = Store::open(path.clone()).await.unwrap();
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
        store
            .update(|state| {
                state.profiles.push(profile);
                Ok(())
            })
            .await
            .unwrap();
        let rejected: Result<(), _> = store
            .update(|state| {
                state.profiles.clear();
                Err(ApiErrorCode::RevisionConflict)
            })
            .await;
        assert!(rejected.is_err());
        drop(store);
        let reopened = Store::open(path).await.unwrap();
        assert_eq!(reopened.snapshot().await.profiles[0].id(), &id);
    }
}
