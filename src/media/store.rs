//! One private, atomic request journal. Accepted plans outlive settings changes.
use crate::media::{
    model::*,
    upstream::{UpMovie, UpSeries},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::RwLock,
};

const LIMIT: usize = 32 * 1024 * 1024;
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DurableState {
    pub schema_version: u32,
    #[serde(default)]
    pub defaults: NativeDefaults,
    pub requests: Vec<StoredRequest>,
}

/// Foreign IDs only. Native managers remain authoritative for rules and paths.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeBinding {
    pub quality_profile_id: QualityProfileId,
    pub root_folder_id: RootFolderId,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct NativeDefaults {
    pub movie: Option<NativeBinding>,
    pub series: Option<NativeBinding>,
}
impl Default for DurableState {
    fn default() -> Self {
        Self {
            schema_version: 2,
            defaults: NativeDefaults::default(),
            requests: vec![],
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct StoredRequest {
    pub actor: String,
    pub key_hash: String,
    pub fingerprint: String,
    pub record: RequestRecord,
    pub plan: Plan,
    pub checkpoint: Checkpoint,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Plan {
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
pub(crate) enum Checkpoint {
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

#[derive(Deserialize)]
struct Version {
    schema_version: u32,
}

fn decode(bytes: &[u8]) -> Result<(DurableState, bool), ApiErrorCode> {
    let version: Version =
        serde_json::from_slice(bytes).map_err(|_| ApiErrorCode::InvalidArguments)?;
    match version.schema_version {
        2 => serde_json::from_slice(bytes)
            .map(|state| (state, false))
            .map_err(|_| ApiErrorCode::InvalidArguments),
        1 => {
            let legacy: LegacyState =
                serde_json::from_slice(bytes).map_err(|_| ApiErrorCode::InvalidArguments)?;
            if legacy.profiles.len() > 100 || legacy.requests.len() > 5000 {
                return Err(ApiErrorCode::InvalidArguments);
            }
            let defaults = native_defaults(&legacy.profiles);
            Ok((
                DurableState {
                    schema_version: 2,
                    defaults,
                    requests: legacy
                        .requests
                        .into_iter()
                        .map(StoredRequest::from)
                        .collect(),
                },
                true,
            ))
        }
        _ => Err(ApiErrorCode::InvalidArguments),
    }
}

fn native_defaults(profiles: &[LegacyProfile]) -> NativeDefaults {
    let mut movies = Vec::new();
    let mut series = Vec::new();
    for profile in profiles {
        match profile {
            LegacyProfile::Movie {
                is_default,
                settings,
                ..
            } => movies.push((
                *is_default,
                NativeBinding {
                    quality_profile_id: settings.quality_profile_id,
                    root_folder_id: settings.root_folder_id,
                },
            )),
            LegacyProfile::Series {
                is_default,
                settings,
                ..
            } => series.push((
                *is_default,
                NativeBinding {
                    quality_profile_id: settings.quality_profile_id,
                    root_folder_id: settings.root_folder_id,
                },
            )),
        }
    }
    NativeDefaults {
        movie: select_binding(&movies),
        series: select_binding(&series),
    }
}

fn select_binding(candidates: &[(bool, NativeBinding)]) -> Option<NativeBinding> {
    let mut declared = candidates
        .iter()
        .filter(|(is_default, _)| *is_default)
        .map(|(_, binding)| *binding);
    match (declared.next(), declared.next()) {
        (Some(binding), None) => Some(binding),
        (None, None) if candidates.len() == 1 => Some(candidates[0].1),
        _ => None,
    }
}

// These closed v1 shapes describe data produced by the previous journal, not a
// public settings API. Only retired profile data is dropped during migration.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyState {
    schema_version: u32,
    profiles: Vec<LegacyProfile>,
    profile_creates: Vec<LegacyProfileCreate>,
    requests: Vec<LegacyStoredRequest>,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum LegacyProfile {
    Movie {
        id: ProfileId,
        revision: Revision,
        name: ProfileName,
        is_default: bool,
        settings: MovieSettings,
    },
    Series {
        id: ProfileId,
        revision: Revision,
        name: ProfileName,
        is_default: bool,
        settings: SeriesSettings,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyProfileCreate {
    actor: String,
    key_hash: String,
    fingerprint: String,
    id: ProfileId,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyStoredRequest {
    actor: String,
    key_hash: String,
    fingerprint: String,
    record: LegacyRequestRecord,
    plan: Plan,
    checkpoint: Checkpoint,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyRequestRecord {
    id: RequestId,
    created_at_unix_ms: u64,
    input: LegacyInput,
    submission: LegacySubmission,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyProfileRef {
    id: ProfileId,
    revision: Revision,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum LegacyInput {
    Movie {
        tmdb_id: TmdbId,
        profile: LegacyProfileRef,
        intent: MovieIntent,
    },
    Series {
        tvdb_id: TvdbId,
        profile: LegacyProfileRef,
        intent: SeriesIntent,
    },
}

impl From<LegacyInput> for CreateRequest {
    fn from(input: LegacyInput) -> Self {
        match input {
            LegacyInput::Movie {
                tmdb_id, intent, ..
            } => Self::Movie { tmdb_id, intent },
            LegacyInput::Series {
                tvdb_id, intent, ..
            } => Self::Series { tvdb_id, intent },
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum LegacySubmission {
    Pending,
    Applying,
    Applied { result: AppliedResult },
    Reconciling { reason: ReconciliationReason },
    Failed { error: LegacyRequestFailure },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyRequestFailure {
    code: LegacyErrorCode,
    message: String,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum LegacyErrorCode {
    Retired(RetiredPolicyError),
    Current(ApiErrorCode),
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum RetiredPolicyError {
    ProfileRevisionConflict,
    RevisionConflict,
    AmbiguousOperationTarget,
}

impl From<LegacySubmission> for Submission {
    fn from(submission: LegacySubmission) -> Self {
        match submission {
            LegacySubmission::Pending => Self::Pending,
            LegacySubmission::Applying => Self::Applying,
            LegacySubmission::Applied { result } => Self::Applied { result },
            LegacySubmission::Reconciling { reason } => Self::Reconciling { reason },
            LegacySubmission::Failed { error } => Self::Failed {
                error: RequestFailure {
                    code: match error.code {
                        LegacyErrorCode::Retired(_) => ApiErrorCode::NativeSettingsRequired,
                        LegacyErrorCode::Current(code) => code,
                    },
                    message: error.message,
                },
            },
        }
    }
}

impl From<LegacyStoredRequest> for StoredRequest {
    fn from(request: LegacyStoredRequest) -> Self {
        Self {
            actor: request.actor,
            key_hash: request.key_hash,
            // Keep the original binding. A new profileless body is not the old
            // accepted input and must not silently replay its upstream effects.
            fingerprint: request.fingerprint,
            record: RequestRecord {
                id: request.record.id,
                created_at_unix_ms: request.record.created_at_unix_ms,
                input: request.record.input.into(),
                submission: request.record.submission.into(),
            },
            plan: request.plan,
            checkpoint: request.checkpoint,
        }
    }
}

pub(crate) struct Store {
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
        let (state, migrated) = match tokio::fs::symlink_metadata(&path).await {
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
                let mut options = tokio::fs::OpenOptions::new();
                options.read(true);
                #[cfg(unix)]
                options.custom_flags(libc::O_NOFOLLOW);
                let file = options
                    .open(&path)
                    .await
                    .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
                let mut bytes = Vec::new();
                file.take((LIMIT + 1) as u64)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
                if bytes.len() > LIMIT {
                    return Err(ApiErrorCode::InvalidArguments);
                }
                decode(&bytes)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (DurableState::default(), false),
            Err(_) => return Err(ApiErrorCode::UpstreamUnavailable),
        };
        if state.schema_version != 2 || state.requests.len() > 5000 {
            return Err(ApiErrorCode::InvalidArguments);
        }
        if migrated {
            let bytes = serde_json::to_vec(&state).map_err(|_| ApiErrorCode::InvalidArguments)?;
            if bytes.len() > LIMIT {
                return Err(ApiErrorCode::InvalidArguments);
            }
            // Commit migration before exposing a worker or request handler.
            // Original actor/key/fingerprint bindings and in-flight plans remain
            // authoritative; removing an input field never authorizes a replay.
            persist(&directory, &bytes).await?;
        }
        Ok(Self {
            directory,
            state: RwLock::new(state),
        })
    }
    pub(crate) async fn native_defaults(&self) -> NativeDefaults {
        self.state.read().await.defaults.clone()
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
        if next.schema_version != 2 || next.requests.len() > 5000 {
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
    use crate::media::{
        model::*,
        service::hash,
        store::{
            Checkpoint, DurableState, LegacyErrorCode, LegacyInput, LegacyProfile,
            LegacyProfileCreate, LegacyProfileRef, LegacyRequestFailure, LegacyRequestRecord,
            LegacyState, LegacyStoredRequest, LegacySubmission, NativeBinding, NativeDefaults,
            Plan, RetiredPolicyError, Store,
        },
        upstream::{Command, UpMovie, UpSeries},
    };
    use serde::{Deserialize, Serialize};
    use std::{fs, path::Path};

    fn profile_id() -> ProfileId {
        "276319ec-c797-460c-a49e-6b90fc1f6ae8".parse().unwrap()
    }
    fn reference() -> LegacyProfileRef {
        LegacyProfileRef {
            id: profile_id(),
            revision: Revision::new(7).unwrap(),
        }
    }
    fn movie_settings() -> MovieSettings {
        MovieSettings {
            quality_profile_id: 2.try_into().unwrap(),
            root_folder_id: 3.try_into().unwrap(),
            tags: vec![4.try_into().unwrap()],
            minimum_availability: MinimumAvailability::Released,
        }
    }
    fn series_settings() -> SeriesSettings {
        SeriesSettings {
            quality_profile_id: 5.try_into().unwrap(),
            root_folder_id: 6.try_into().unwrap(),
            tags: vec![7.try_into().unwrap()],
            series_type: SeriesType::Standard,
            season_folders: true,
            future_monitoring: FutureMonitoring::Monitor,
        }
    }
    fn movie_plan() -> Plan {
        let media: UpMovie = serde_json::from_str(r#"{"id":7,"tmdbId":14337,"title":"Fixture Movie","qualityProfileId":2,"rootFolderPath":"/media/movies","path":"/media/movies/fixture","monitored":true,"tags":[4]}"#).unwrap();
        Plan::Movie {
            media,
            settings: movie_settings(),
            root: "/media/movies".into(),
            search: true,
            existing_id: Some(7),
        }
    }
    fn stored(
        index: u8,
        submission: LegacySubmission,
        checkpoint: Checkpoint,
    ) -> LegacyStoredRequest {
        let input = LegacyInput::Movie {
            tmdb_id: 14337.try_into().unwrap(),
            profile: reference(),
            intent: MovieIntent::SearchNow {},
        };
        LegacyStoredRequest {
            actor: format!("ha:actor-{index}"),
            key_hash: format!("{index:064x}"),
            fingerprint: hash(&[serde_json::to_string(&input).unwrap().as_str()]),
            record: LegacyRequestRecord {
                id: RequestId::from_digest([index; 32]),
                created_at_unix_ms: u64::from(index) * 100,
                input,
                submission,
            },
            plan: movie_plan(),
            checkpoint,
        }
    }
    fn legacy() -> LegacyState {
        let mut series = stored(
            2,
            LegacySubmission::Reconciling {
                reason: ReconciliationReason::RestartedDuringWrite,
            },
            Checkpoint::Monitoring {
                upstream_id: 8,
                effect: RequestEffect::AlreadyManaged,
                episode_ids: vec![40, 42],
            },
        );
        series.record.input = LegacyInput::Series {
            tvdb_id: 12345.try_into().unwrap(),
            profile: reference(),
            intent: SeriesIntent::SearchNow {
                scope: EpisodeScope::Seasons {
                    numbers: vec![0, 2].try_into().unwrap(),
                },
            },
        };
        series.fingerprint = hash(&[serde_json::to_string(&series.record.input)
            .unwrap()
            .as_str()]);
        let media: UpSeries = serde_json::from_str(r#"{"id":8,"tvdbId":12345,"title":"Fixture Series","qualityProfileId":5,"rootFolderPath":"/media/series","path":"/media/series/fixture","monitored":true,"seasonFolder":true,"monitorNewItems":"all","seriesType":"standard","tags":[7],"seasons":[{"seasonNumber":0,"monitored":true},{"seasonNumber":2,"monitored":true}]}"#).unwrap();
        series.plan = Plan::Series {
            media,
            settings: series_settings(),
            root: "/media/series".into(),
            search: true,
            selected_seasons: vec![0, 2],
            as_of: "2026-11-08T00:00:00.000Z".into(),
            existing_id: Some(8),
        };
        let mut requests = vec![
            stored(
                1,
                LegacySubmission::Applying,
                Checkpoint::Searching {
                    upstream_id: 7,
                    effect: RequestEffect::Added,
                    command: Command::MoviesSearch { movie_ids: vec![7] },
                },
            ),
            series,
            stored(3, LegacySubmission::Pending, Checkpoint::Prepared),
            stored(
                4,
                LegacySubmission::Applied {
                    result: AppliedResult {
                        effect: RequestEffect::AlreadyManaged,
                        media: MediaIdentity::Movie {
                            tmdb_id: 14337.try_into().unwrap(),
                        },
                        manager: ManagerId::Radarr,
                        upstream_id: 7,
                        search: SearchDispatch::Queued { command_id: 99 },
                    },
                },
                Checkpoint::Complete,
            ),
        ];
        for (index, code) in [
            (
                5,
                LegacyErrorCode::Retired(RetiredPolicyError::ProfileRevisionConflict),
            ),
            (
                6,
                LegacyErrorCode::Retired(RetiredPolicyError::RevisionConflict),
            ),
            (
                7,
                LegacyErrorCode::Retired(RetiredPolicyError::AmbiguousOperationTarget),
            ),
            (8, LegacyErrorCode::Current(ApiErrorCode::PermissionDenied)),
            (
                9,
                LegacyErrorCode::Current(ApiErrorCode::PreconditionRequired),
            ),
        ] {
            requests.push(stored(
                index,
                LegacySubmission::Failed {
                    error: LegacyRequestFailure {
                        code,
                        message: format!("Historical failure {index}."),
                    },
                },
                Checkpoint::Complete,
            ));
        }
        LegacyState {
            schema_version: 1,
            profiles: vec![
                LegacyProfile::Movie {
                    id: profile_id(),
                    revision: Revision::new(7).unwrap(),
                    name: "Retired movie preset".parse().unwrap(),
                    is_default: true,
                    settings: movie_settings(),
                },
                LegacyProfile::Series {
                    id: "376319ec-c797-460c-a49e-6b90fc1f6ae8".parse().unwrap(),
                    revision: Revision::new(9).unwrap(),
                    name: "Retired series preset".parse().unwrap(),
                    is_default: true,
                    settings: series_settings(),
                },
            ],
            profile_creates: vec![LegacyProfileCreate {
                actor: "admin".into(),
                key_hash: "old-key-hash".into(),
                fingerprint: "old-profile-fingerprint".into(),
                id: profile_id(),
            }],
            requests,
        }
    }
    fn write_bytes(directory: &Path, bytes: &[u8]) {
        use std::io::Write as _;
        fs::create_dir_all(directory).unwrap();
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options
            .open(directory.join("journal.json"))
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
    fn write_state(directory: &Path, state: &impl Serialize) {
        write_bytes(directory, &serde_json::to_vec(state).unwrap());
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PublicRequestReceipt {
        id: RequestId,
        created_at_unix_ms: u64,
        input: CreateRequest,
        submission: Submission,
    }

    #[tokio::test]
    async fn version_one_migration_commits_and_preserves_all_accepted_plans_and_bindings() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("media");
        let original = legacy();
        write_state(&directory, &original);
        fs::write(
            directory.join("integration-token"),
            b"unchanged-app-owned-pairing-fixture",
        )
        .unwrap();
        fs::write(
            temp.path().join("native-config.json"),
            b"unchanged-parent-owned-config",
        )
        .unwrap();
        let store = Store::open(directory.clone()).await.unwrap();
        let snapshot = store.snapshot().await;
        assert_eq!(snapshot.schema_version, 2);
        assert_eq!(
            snapshot.defaults,
            NativeDefaults {
                movie: Some(NativeBinding {
                    quality_profile_id: 2.try_into().unwrap(),
                    root_folder_id: 3.try_into().unwrap()
                }),
                series: Some(NativeBinding {
                    quality_profile_id: 5.try_into().unwrap(),
                    root_folder_id: 6.try_into().unwrap()
                }),
            }
        );
        assert_eq!(snapshot.requests.len(), original.requests.len());
        for (actual, expected) in snapshot.requests.iter().zip(&original.requests) {
            assert_eq!(actual.actor, expected.actor);
            assert_eq!(actual.key_hash, expected.key_hash);
            assert_eq!(actual.fingerprint, expected.fingerprint);
            assert_eq!(actual.record.id, expected.record.id);
            assert_eq!(
                actual.record.created_at_unix_ms,
                expected.record.created_at_unix_ms
            );
            assert!(
                serde_json::to_vec(&actual.plan).unwrap()
                    == serde_json::to_vec(&expected.plan).unwrap()
            );
            assert!(
                serde_json::to_vec(&actual.checkpoint).unwrap()
                    == serde_json::to_vec(&expected.checkpoint).unwrap()
            );
            let new_fingerprint = hash(&[serde_json::to_string(&actual.record.input)
                .unwrap()
                .as_str()]);
            assert_ne!(
                actual.fingerprint, new_fingerprint,
                "migration must not rebind an accepted key to the new body"
            );
            // The public receipt producer must remain consumable without any
            // private binding, native settings snapshot or plan fields.
            let public: PublicRequestReceipt =
                serde_json::from_slice(&serde_json::to_vec(&actual.record).unwrap()).unwrap();
            assert_eq!(public.id, actual.record.id);
            assert_eq!(public.created_at_unix_ms, actual.record.created_at_unix_ms);
            assert_eq!(public.input, actual.record.input);
            assert!(
                serde_json::to_vec(&public.submission).unwrap()
                    == serde_json::to_vec(&actual.record.submission).unwrap()
            );
        }
        assert_eq!(
            snapshot.requests[0].record.input,
            CreateRequest::Movie {
                tmdb_id: 14337.try_into().unwrap(),
                intent: MovieIntent::SearchNow {}
            }
        );
        assert_eq!(
            snapshot.requests[1].record.input,
            CreateRequest::Series {
                tvdb_id: 12345.try_into().unwrap(),
                intent: SeriesIntent::SearchNow {
                    scope: EpisodeScope::Seasons {
                        numbers: vec![0, 2].try_into().unwrap()
                    }
                }
            }
        );
        assert!(matches!(
            &snapshot.requests[0].record.submission,
            Submission::Applying
        ));
        assert!(matches!(
            &snapshot.requests[1].record.submission,
            Submission::Reconciling {
                reason: ReconciliationReason::RestartedDuringWrite
            }
        ));
        assert!(matches!(
            &snapshot.requests[2].record.submission,
            Submission::Pending
        ));
        assert!(matches!(
            &snapshot.requests[3].record.submission,
            Submission::Applied {
                result: AppliedResult {
                    effect: RequestEffect::AlreadyManaged,
                    upstream_id: 7,
                    search: SearchDispatch::Queued { command_id: 99 },
                    ..
                }
            }
        ));
        for (offset, request) in snapshot.requests[4..7].iter().enumerate() {
            let Submission::Failed { error } = &request.record.submission else {
                panic!("failed receipt lost");
            };
            assert_eq!(error.code, ApiErrorCode::NativeSettingsRequired);
            assert_eq!(error.message, format!("Historical failure {}.", offset + 5));
        }
        assert!(matches!(
            &snapshot.requests[7].record.submission,
            Submission::Failed {
                error: RequestFailure {
                    code: ApiErrorCode::PermissionDenied,
                    ..
                }
            }
        ));
        assert!(matches!(
            &snapshot.requests[8].record.submission,
            Submission::Failed {
                error: RequestFailure {
                    code: ApiErrorCode::PreconditionRequired,
                    ..
                }
            }
        ));
        // The actual v2 producer is readable by its strict consumer before open
        // returns; profile data and create receipts are no longer in that shape.
        let committed = fs::read(directory.join("journal.json")).unwrap();
        let disk: DurableState = serde_json::from_slice(&committed).unwrap();
        assert_eq!(disk.schema_version, 2);
        assert_eq!(disk.defaults, snapshot.defaults);
        assert_eq!(disk.requests.len(), original.requests.len());
        drop(store);
        let reopened = Store::open(directory.clone()).await.unwrap();
        assert!(
            serde_json::to_vec(&reopened.snapshot().await).unwrap()
                == serde_json::to_vec(&snapshot).unwrap()
        );
        assert!(fs::read(directory.join("journal.json")).unwrap() == committed);
        assert!(
            fs::read(directory.join("integration-token")).unwrap()
                == b"unchanged-app-owned-pairing-fixture"
        );
        assert!(
            fs::read(temp.path().join("native-config.json")).unwrap()
                == b"unchanged-parent-owned-config"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(directory.join("journal.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn native_bindings_follow_declared_or_sole_choices_without_guessing() {
        for case in [
            "declared",
            "sole",
            "ambiguous_declared",
            "ambiguous_unselected",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let directory = temp.path().join("media");
            let mut original = legacy();
            if case == "sole" || case == "ambiguous_unselected" {
                for profile in &mut original.profiles {
                    match profile {
                        LegacyProfile::Movie { is_default, .. }
                        | LegacyProfile::Series { is_default, .. } => *is_default = false,
                    }
                }
            }
            if case != "sole" {
                let mut settings = movie_settings();
                settings.quality_profile_id = 9.try_into().unwrap();
                settings.root_folder_id = 10.try_into().unwrap();
                original.profiles.push(LegacyProfile::Movie {
                    id: "476319ec-c797-460c-a49e-6b90fc1f6ae8".parse().unwrap(),
                    revision: Revision::new(1).unwrap(),
                    name: "Another native selection".parse().unwrap(),
                    is_default: case == "ambiguous_declared",
                    settings,
                });
            }
            write_state(&directory, &original);
            let store = Store::open(directory.clone()).await.unwrap();
            let snapshot = store.snapshot().await;
            let expected_movie = matches!(case, "declared" | "sole").then_some(NativeBinding {
                quality_profile_id: 2.try_into().unwrap(),
                root_folder_id: 3.try_into().unwrap(),
            });
            assert_eq!(snapshot.defaults.movie, expected_movie, "{case}");
            assert_eq!(
                snapshot.defaults.series,
                Some(NativeBinding {
                    quality_profile_id: 5.try_into().unwrap(),
                    root_folder_id: 6.try_into().unwrap(),
                }),
                "{case}"
            );
            assert_eq!(snapshot.requests.len(), original.requests.len());
            drop(store);
            assert_eq!(
                Store::open(directory)
                    .await
                    .unwrap()
                    .snapshot()
                    .await
                    .defaults,
                snapshot.defaults
            );
        }
    }

    #[tokio::test]
    async fn version_two_without_bindings_remains_readable_without_inventing_choices() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("media");
        let original = br#"{"schema_version":2,"requests":[]}"#;
        write_bytes(&directory, original);
        let store = Store::open(directory.clone()).await.unwrap();
        assert_eq!(store.snapshot().await.defaults, NativeDefaults::default());
        assert!(store.snapshot().await.requests.is_empty());
        assert!(fs::read(directory.join("journal.json")).unwrap() == original);
    }

    #[tokio::test]
    async fn rejected_mutations_and_invalid_versions_leave_memory_and_disk_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("media");
        let store = Store::open(directory.clone()).await.unwrap();
        assert_eq!(store.snapshot().await.schema_version, 2);
        assert_eq!(store.snapshot().await.defaults, NativeDefaults::default());
        let accepted = stored(1, LegacySubmission::Pending, Checkpoint::Prepared).into();
        store
            .update(|state| {
                state.requests.push(accepted);
                Ok(())
            })
            .await
            .unwrap();
        let before = fs::read(directory.join("journal.json")).unwrap();
        let rejected: Result<(), _> = store
            .update(|state| {
                state.requests.clear();
                Err(ApiErrorCode::PermissionDenied)
            })
            .await;
        assert_eq!(rejected, Err(ApiErrorCode::PermissionDenied));
        let rejected: Result<(), _> = store
            .update(|state| {
                state.schema_version = 3;
                Ok(())
            })
            .await;
        assert_eq!(rejected, Err(ApiErrorCode::InvalidArguments));
        let rejected: Result<(), _> = store
            .update(|state| {
                state.requests.resize(5001, state.requests[0].clone());
                Ok(())
            })
            .await;
        assert_eq!(rejected, Err(ApiErrorCode::InvalidArguments));
        assert_eq!(store.snapshot().await.schema_version, 2);
        assert_eq!(store.snapshot().await.requests.len(), 1);
        assert!(fs::read(directory.join("journal.json")).unwrap() == before);
        drop(store);
        let reopened = Store::open(directory).await.unwrap();
        assert_eq!(reopened.snapshot().await.requests.len(), 1);
    }

    #[tokio::test]
    async fn invalid_legacy_data_or_failed_migration_publication_never_overwrites_the_journal() {
        for version in [0, 3] {
            let temp = tempfile::tempdir().unwrap();
            let directory = temp.path().join("media");
            let mut original = legacy();
            original.schema_version = version;
            let bytes = serde_json::to_vec(&original).unwrap();
            write_bytes(&directory, &bytes);
            assert_eq!(
                Store::open(directory.clone()).await.err(),
                Some(ApiErrorCode::InvalidArguments)
            );
            assert!(fs::read(directory.join("journal.json")).unwrap() == bytes);
        }
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("media");
        let bytes = serde_json::to_string(&legacy())
            .unwrap()
            .replace("profile_revision_conflict", "unknown_historical_failure")
            .into_bytes();
        write_bytes(&directory, &bytes);
        assert_eq!(
            Store::open(directory.clone()).await.err(),
            Some(ApiErrorCode::InvalidArguments)
        );
        assert!(fs::read(directory.join("journal.json")).unwrap() == bytes);

        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("media");
        let original = legacy();
        let bytes = serde_json::to_vec(&original).unwrap();
        write_bytes(&directory, &bytes);
        fs::create_dir(directory.join(".journal.tmp")).unwrap();
        assert_eq!(
            Store::open(directory.clone()).await.err(),
            Some(ApiErrorCode::UpstreamUnavailable)
        );
        assert!(fs::read(directory.join("journal.json")).unwrap() == bytes);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_limits_and_private_file_checks_apply_before_migration() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("media");
        let bytes = serde_json::to_vec(&legacy()).unwrap();
        write_bytes(&directory, &bytes);
        let path = directory.join("journal.json");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            Store::open(directory.clone()).await.err(),
            Some(ApiErrorCode::PermissionDenied)
        );
        assert!(fs::read(&path).unwrap() == bytes);
        fs::rename(&path, directory.join("saved.json")).unwrap();
        symlink(directory.join("saved.json"), &path).unwrap();
        assert_eq!(
            Store::open(directory.clone()).await.err(),
            Some(ApiErrorCode::InvalidArguments)
        );
        assert!(fs::read(directory.join("saved.json")).unwrap() == bytes);
        fs::remove_file(&path).unwrap();
        write_bytes(&directory, b"{}");
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len((super::LIMIT + 1) as u64)
            .unwrap();
        assert_eq!(
            Store::open(directory).await.err(),
            Some(ApiErrorCode::InvalidArguments)
        );
    }
}
