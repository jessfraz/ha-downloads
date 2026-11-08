//! The media API's owned wire types. Upstream objects never cross this boundary.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    num::{NonZeroU32, NonZeroU64},
    str::FromStr,
};

macro_rules! positive_id {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
        #[serde(try_from="u32", into="u32")]
        pub struct $name(NonZeroU32);
        impl JsonSchema for $name {
            fn schema_name()->String{stringify!($name).into()}
            fn json_schema(_: &mut schemars::r#gen::SchemaGenerator)->schemars::schema::Schema{
                schemars::schema::SchemaObject{instance_type:Some(schemars::schema::InstanceType::Integer.into()),format:Some("uint32".into()),number:Some(Box::new(schemars::schema::NumberValidation{minimum:Some(1.0),maximum:Some(i32::MAX as f64),..Default::default()})),..Default::default()}.into()
            }
        }
        impl $name { pub fn get(self) -> u32 { self.0.get() } }
        impl TryFrom<u32> for $name {
            type Error = &'static str;
            fn try_from(value:u32)->Result<Self,Self::Error>{if value>i32::MAX as u32{return Err("ID exceeds the native API range")};NonZeroU32::new(value).map(Self).ok_or("ID must be positive")}
        }
        impl From<$name> for u32{fn from(value:$name)->u32{value.get()}}
        impl fmt::Display for $name { fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result{self.get().fmt(f)} }
        impl FromStr for $name { type Err=&'static str; fn from_str(s:&str)->Result<Self,Self::Err>{s.parse::<u32>().map_err(|_|"Invalid ID").and_then(Self::try_from)} }
    )+};
}
positive_id!(
    TmdbId,
    TvdbId,
    QualityProfileId,
    RootFolderId,
    TagId,
    EpisodeId
);

macro_rules! uuid_id {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
        #[serde(try_from="String", into="String")]
        pub struct $name(String);
        impl JsonSchema for $name {
            fn schema_name()->String{stringify!($name).into()}
            fn json_schema(_: &mut schemars::r#gen::SchemaGenerator)->schemars::schema::Schema{
                schemars::schema::SchemaObject{instance_type:Some(schemars::schema::InstanceType::String.into()),format:Some("uuid".into()),string:Some(Box::new(schemars::schema::StringValidation{min_length:Some(36),max_length:Some(36),pattern:Some("^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$".into())})),..Default::default()}.into()
            }
        }
        impl $name {
            pub fn as_str(&self)->&str{&self.0}
            pub fn from_digest(mut bytes:[u8;32])->Self{
                // RFC 9562 custom UUIDv8: deterministic opaque identity, not a credential.
                bytes[6]=(bytes[6]&0x0f)|0x80; bytes[8]=(bytes[8]&0x3f)|0x80;
                let hex=bytes[..16].iter().map(|b|format!("{b:02x}")).collect::<String>();
                Self(format!("{}-{}-{}-{}-{}",&hex[..8],&hex[8..12],&hex[12..16],&hex[16..20],&hex[20..]))
            }
        }
        impl TryFrom<String> for $name {
            type Error=&'static str;
            fn try_from(value:String)->Result<Self,Self::Error>{
                if value.len()!=36 || !value.bytes().enumerate().all(|(i,b)|if [8,13,18,23].contains(&i){b==b'-'}else{b.is_ascii_hexdigit()}) || value.bytes().all(|b|b==b'0'||b==b'-'){return Err("Invalid UUID")}
                Ok(Self(value.to_ascii_lowercase()))
            }
        }
        impl From<$name> for String {fn from(v:$name)->Self{v.0}}
        impl fmt::Display for $name {fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result{self.0.fmt(f)}}
        impl FromStr for $name {type Err=&'static str; fn from_str(v:&str)->Result<Self,Self::Err>{Self::try_from(v.to_owned())}}
    )+};
}
uuid_id!(ProfileId, RequestId);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(transparent)]
pub struct Revision(NonZeroU64);
impl Revision {
    pub fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }
    pub fn get(self) -> u64 {
        self.0.get()
    }
    pub fn next(self) -> Option<Self> {
        self.get().checked_add(1).and_then(Self::new)
    }
}
impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(f)
    }
}

macro_rules! bounded_text {
    ($name:ident,$max:expr) => {
        #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl JsonSchema for $name {
            fn schema_name() -> String {
                stringify!($name).into()
            }
            fn json_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
                schemars::schema::SchemaObject {
                    instance_type: Some(schemars::schema::InstanceType::String.into()),
                    string: Some(Box::new(schemars::schema::StringValidation {
                        min_length: Some(1),
                        max_length: Some($max),
                        ..Default::default()
                    })),
                    ..Default::default()
                }
                .into()
            }
        }
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = &'static str;
            fn try_from(v: String) -> Result<Self, Self::Error> {
                let s = v.trim();
                if s.is_empty() || s.chars().count() > $max || s.chars().any(char::is_control) {
                    Err("Invalid text")
                } else {
                    Ok(Self(s.into()))
                }
            }
        }
        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.0
            }
        }
        impl FromStr for $name {
            type Err = &'static str;
            fn from_str(v: &str) -> Result<Self, Self::Err> {
                Self::try_from(v.to_owned())
            }
        }
    };
}
bounded_text!(SearchQuery, 200);
bounded_text!(ProfileName, 80);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(try_from = "Vec<u16>", into = "Vec<u16>")]
pub struct SeasonSet(Vec<u16>);
impl JsonSchema for SeasonSet {
    fn schema_name() -> String {
        "SeasonSet".into()
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::Array.into()),
            array: Some(Box::new(schemars::schema::ArrayValidation {
                items: Some(generator.subschema_for::<u16>().into()),
                min_items: Some(1),
                max_items: Some(100),
                unique_items: Some(true),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}
impl SeasonSet {
    pub fn numbers(&self) -> &[u16] {
        &self.0
    }
}
impl TryFrom<Vec<u16>> for SeasonSet {
    type Error = &'static str;
    fn try_from(mut v: Vec<u16>) -> Result<Self, Self::Error> {
        if v.is_empty() || v.len() > 100 {
            return Err("Choose 1 to 100 seasons");
        };
        v.sort_unstable();
        if v.windows(2).any(|p| p[0] == p[1]) {
            return Err("Duplicate season");
        };
        Ok(Self(v))
    }
}
impl From<SeasonSet> for Vec<u16> {
    fn from(v: SeasonSet) -> Self {
        v.0
    }
}

macro_rules! string_enum {
    ($name:ident{$($variant:ident=>$wire:literal),+$(,)?})=>{
        #[derive(Clone,Copy,Debug,Deserialize,Serialize,JsonSchema,PartialEq,Eq,PartialOrd,Ord)]
        pub enum $name{$(#[serde(rename=$wire)] $variant),+}
        impl fmt::Display for $name{fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result{f.write_str(match self{$(Self::$variant=>$wire),+})}}
        impl FromStr for $name{type Err=&'static str;fn from_str(s:&str)->Result<Self,Self::Err>{match s{$($wire=>Ok(Self::$variant),)+_=>Err("Invalid enum value")}}}
    };
}
string_enum!(ManagerId{Radarr=>"radarr",Sonarr=>"sonarr"});
string_enum!(MediaKind{Movie=>"movie",Series=>"series"});
string_enum!(SearchKind{All=>"all",Movie=>"movie",Series=>"series"});
string_enum!(Component{Radarr=>"radarr",Sonarr=>"sonarr",Prowlarr=>"prowlarr",Sabnzbd=>"sabnzbd",Vpn=>"vpn",Firewall=>"firewall",Dns=>"dns",Storage=>"storage",Plex=>"plex"});
string_enum!(Operation{RequestMovie=>"request_movie",RequestSeries=>"request_series",Transfer=>"transfer",ImportMovie=>"import_movie",ImportSeries=>"import_series"});
string_enum!(Freshness{Fresh=>"fresh",Stale=>"stale"});
string_enum!(CoverageState{Available=>"available",Unavailable=>"unavailable",Unsupported=>"unsupported",Partial=>"partial"});
string_enum!(MinimumAvailability{Announced=>"announced",InCinemas=>"in_cinemas",Released=>"released"});
string_enum!(SeriesType{Standard=>"standard",Daily=>"daily",Anime=>"anime"});
string_enum!(FutureMonitoring{Off=>"off",Monitor=>"monitor"});
string_enum!(SeasonState{Unknown=>"unknown",Missing=>"missing",Partial=>"partial",InProgress=>"in_progress",Imported=>"imported"});
string_enum!(ArtworkKind{Poster=>"poster",Backdrop=>"backdrop"});
string_enum!(Transport{Native=>"native",Ingress=>"ingress",HomeAssistant=>"home_assistant"});
string_enum!(CredentialRole{ReadOnly=>"read_only",Requester=>"requester",Administrator=>"administrator",Integration=>"integration"});
string_enum!(UnavailableReason{Timeout=>"timeout",Connection=>"connection",Authentication=>"authentication",InvalidResponse=>"invalid_response",NotRunning=>"not_running"});
string_enum!(IssueSeverity{Warning=>"warning",Error=>"error"});
string_enum!(HealthIssueCode{UpdateAvailable=>"update_available",UpstreamHealth=>"upstream_health",IndexerUnavailable=>"indexer_unavailable",DownloadClientUnavailable=>"download_client_unavailable",StorageUnavailable=>"storage_unavailable",StorageLow=>"storage_low",ProtectionUnverified=>"protection_unverified",ChecksMissing=>"checks_missing",ChecksStale=>"checks_stale",NotConfigured=>"not_configured",UpstreamUnavailable=>"upstream_unavailable"});
string_enum!(ReadinessState{Ready=>"ready",Blocked=>"blocked"});
string_enum!(AcquisitionBlock{ImportFailed=>"import_failed",DownloadFailed=>"download_failed",ReleaseRejected=>"release_rejected",Unknown=>"unknown"});
string_enum!(RequestEffect{Added=>"added",AlreadyManaged=>"already_managed",AlreadySatisfied=>"already_satisfied"});
string_enum!(ReconciliationReason{UpstreamOutcomeUnknown=>"upstream_outcome_unknown",DependencyUnavailable=>"dependency_unavailable",RestartedDuringWrite=>"restarted_during_write"});
string_enum!(ApiErrorCode{AuthenticationRequired=>"authentication_required",InvalidArguments=>"invalid_arguments",PermissionDenied=>"permission_denied",NotFound=>"not_found",NotConfigured=>"not_configured",NativeSettingsRequired=>"native_settings_required",UpstreamUnavailable=>"upstream_unavailable",InvalidUpstreamResponse=>"invalid_upstream_response",ExistingSettingsConflict=>"existing_settings_conflict",IdempotencyConflict=>"idempotency_conflict",InvalidScope=>"invalid_scope",PreconditionRequired=>"precondition_required",UnsupportedTransportOperation=>"unsupported_transport_operation"});
string_enum!(DownloadStatus{Queued=>"queued",Paused=>"paused",Downloading=>"downloading",Completed=>"completed",Failed=>"failed",Unknown=>"unknown"});
string_enum!(DownloadProtocol{Usenet=>"usenet",Torrent=>"torrent",Unknown=>"unknown"});
string_enum!(DownloadImportStatus{NotStarted=>"not_started",Pending=>"pending",Importing=>"importing",Imported=>"imported",Failed=>"failed",Unknown=>"unknown"});
string_enum!(DownloadWarningCode{DownloadFailed=>"download_failed",ImportFailed=>"import_failed",ImportBlocked=>"import_blocked",ProviderWarning=>"provider_warning",ProviderError=>"provider_error"});

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(try_from = "String", into = "String")]
pub struct QueueId {
    manager: ManagerId,
    native_id: NonZeroU32,
}

/// A validated UTC filter shared by command-line and service callers.
#[derive(Clone, Debug)]
pub struct ImportSince(String);
impl ImportSince {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl FromStr for ImportSince {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        crate::media::requests::utc_instant(value).ok_or("Expected a valid UTC timestamp")?;
        Ok(Self(value.to_owned()))
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(transparent)]
pub struct ImportId(QueueId);
impl ImportId {
    pub(crate) fn new(manager: ManagerId, native_id: u32) -> Result<Self, &'static str> {
        format!("{manager}:{native_id}")
            .parse::<QueueId>()
            .map(Self)
    }
    pub fn manager(self) -> ManagerId {
        self.0.manager()
    }
    pub fn native_id(self) -> u32 {
        self.0.native_id()
    }
}
impl FromStr for ImportId {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse::<QueueId>().map(Self)
    }
}
impl fmt::Display for ImportId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl QueueId {
    pub fn manager(self) -> ManagerId {
        self.manager
    }
    pub fn native_id(self) -> u32 {
        self.native_id.get()
    }
}
impl FromStr for QueueId {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (manager, id) = value.split_once(':').ok_or("Invalid queue ID")?;
        let manager = ManagerId::from_str(manager)?;
        let id = id.parse::<u32>().map_err(|_| "Invalid queue ID")?;
        if id == 0 || id > i32::MAX as u32 {
            return Err("Invalid queue ID");
        }
        let parsed = Self {
            manager,
            native_id: NonZeroU32::new(id).ok_or("Invalid queue ID")?,
        };
        if parsed.to_string() != value {
            return Err("Invalid queue ID");
        }
        Ok(parsed)
    }
}
impl TryFrom<String> for QueueId {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<QueueId> for String {
    fn from(value: QueueId) -> Self {
        value.to_string()
    }
}
impl fmt::Display for QueueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.manager, self.native_id)
    }
}
impl JsonSchema for QueueId {
    fn schema_name() -> String {
        "QueueId".into()
    }
    fn json_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::String.into()),
            string: Some(Box::new(schemars::schema::StringValidation {
                pattern: Some("^(radarr|sonarr):[1-9][0-9]{0,9}$".into()),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub read: bool,
    pub request: bool,
    pub admin: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct SystemResponse {
    pub version: String,
    pub capabilities: Capabilities,
    pub transport: Transport,
    pub credential_role: CredentialRole,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct SourceCoverage {
    pub source: Component,
    pub state: CoverageState,
    pub observed_at_unix_ms: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_page: Option<String>,
    pub coverage: Vec<SourceCoverage>,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct ManagerSummary {
    pub manager: ManagerId,
    pub configured: bool,
    pub available: bool,
    pub version: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct ManagerList {
    pub items: Vec<ManagerSummary>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MovieSettings {
    pub quality_profile_id: QualityProfileId,
    pub root_folder_id: RootFolderId,
    pub tags: Vec<TagId>,
    pub minimum_availability: MinimumAvailability,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SeriesSettings {
    pub quality_profile_id: QualityProfileId,
    pub root_folder_id: RootFolderId,
    pub tags: Vec<TagId>,
    pub series_type: SeriesType,
    pub season_folders: bool,
    pub future_monitoring: FutureMonitoring,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum MovieIntent {
    SearchNow {},
    MonitorOnly {},
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EpisodeScope {
    AllAired {},
    Seasons { numbers: SeasonSet },
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum SeriesIntent {
    SearchNow { scope: EpisodeScope },
    MonitorOnly {},
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CreateRequest {
    Movie {
        tmdb_id: TmdbId,
        intent: MovieIntent,
    },
    Series {
        tvdb_id: TvdbId,
        intent: SeriesIntent,
    },
}
impl CreateRequest {
    pub fn identity(&self) -> MediaIdentity {
        match self {
            Self::Movie { tmdb_id, .. } => MediaIdentity::Movie { tmdb_id: *tmdb_id },
            Self::Series { tvdb_id, .. } => MediaIdentity::Series { tvdb_id: *tvdb_id },
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MediaIdentity {
    Movie { tmdb_id: TmdbId },
    Series { tvdb_id: TvdbId },
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LibraryState {
    NotAdded,
    Managed { upstream_id: u32 },
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Acquisition {
    Unknown,
    NotRequested,
    Waiting,
    Downloading {
        completed_bytes: u64,
        total_bytes: u64,
    },
    Importing,
    Imported,
    Blocked {
        reason: AcquisitionBlock,
    },
    Unavailable {
        component: Component,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct Season {
    pub number: u16,
    pub episode_count: Option<u32>,
    pub imported_count: Option<u32>,
    pub active_count: Option<u32>,
    pub monitored: bool,
    pub state: SeasonState,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MovieMetadata {
    pub runtime_minutes: Option<u32>,
    pub genres: Vec<String>,
    pub studio: Option<String>,
    pub certification: Option<String>,
    pub file: Option<MediaFileDetails>,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SeriesMetadata {
    pub runtime_minutes: Option<u32>,
    pub genres: Vec<String>,
    pub network: Option<String>,
    pub certification: Option<String>,
    pub next_airing: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaFileDetails {
    pub quality: Option<DownloadQuality>,
    pub size_bytes: Option<u64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub audio_channels: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestAvailability {
    Ready,
    ParentSetupRequired,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaItem {
    Movie {
        tmdb_id: TmdbId,
        title: String,
        year: Option<u16>,
        overview: String,
        poster: Option<String>,
        request: RequestAvailability,
        library: LibraryState,
        acquisition: Acquisition,
        #[serde(default)]
        metadata: Option<MovieMetadata>,
    },
    Series {
        tvdb_id: TvdbId,
        title: String,
        year: Option<u16>,
        overview: String,
        poster: Option<String>,
        request: RequestAvailability,
        library: LibraryState,
        acquisition: Acquisition,
        seasons: Vec<Season>,
        #[serde(default)]
        metadata: Option<SeriesMetadata>,
    },
}
impl MediaItem {
    pub fn identity(&self) -> MediaIdentity {
        match self {
            Self::Movie { tmdb_id, .. } => MediaIdentity::Movie { tmdb_id: *tmdb_id },
            Self::Series { tvdb_id, .. } => MediaIdentity::Series { tvdb_id: *tvdb_id },
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct Episode {
    pub id: EpisodeId,
    pub season: u16,
    pub number: u16,
    pub title: String,
    pub air_date: Option<String>,
    pub monitored: bool,
    pub acquisition: Acquisition,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct EpisodeNumber {
    pub season: u16,
    pub number: u16,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct QueueItem {
    pub id: String,
    pub media: MediaIdentity,
    pub title: String,
    pub episode: Option<EpisodeNumber>,
    pub acquisition: Acquisition,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DownloadRevision {
    pub version: u32,
    pub real: u32,
    pub is_repack: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DownloadQuality {
    pub name: String,
    pub resolution: Option<u16>,
    pub source: Option<String>,
    pub modifier: Option<String>,
    pub revision: Option<DownloadRevision>,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DownloadProgress {
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub remaining_bytes: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DownloadWarning {
    pub code: DownloadWarningCode,
    pub severity: IssueSeverity,
    pub message: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DownloadDetails {
    pub id: QueueId,
    pub manager: ManagerId,
    pub media: MediaIdentity,
    pub title: String,
    pub episode: Option<EpisodeNumber>,
    pub acquisition: Acquisition,
    pub status: DownloadStatus,
    pub quality: Option<DownloadQuality>,
    pub progress: Option<DownloadProgress>,
    pub remaining_seconds: Option<u64>,
    pub download_client: Option<String>,
    pub indexer: Option<String>,
    pub protocol: Option<DownloadProtocol>,
    pub import_status: DownloadImportStatus,
    pub warnings: Vec<DownloadWarning>,
    pub languages: Vec<String>,
    pub custom_formats: Vec<String>,
    pub custom_format_score: Option<i32>,
    pub added_at: Option<String>,
    pub estimated_completion_at: Option<String>,
    pub observed_at_unix_ms: u64,
}
string_enum!(ImportVerificationReason{IdentityUnavailable=>"identity_unavailable",FileEvidenceMissing=>"file_evidence_missing",NotMatched=>"not_matched",AmbiguousMatch=>"ambiguous_match",OlderVersion=>"older_version"});
string_enum!(SubtitleUnknownReason{PlexUnverified=>"plex_unverified",StreamsUnavailable=>"streams_unavailable"});
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlexImportStatus {
    NotConfigured,
    Unavailable { reason: UnavailableReason },
    Unknown { reason: ImportVerificationReason },
    Verified { observed_at_unix_ms: u64 },
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubtitleImportStatus {
    Unknown {
        reason: SubtitleUnknownReason,
    },
    Verified {
        languages: Vec<String>,
        observed_at_unix_ms: u64,
    },
    Missing {
        observed_at_unix_ms: u64,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportedItem {
    pub id: ImportId,
    pub manager: ManagerId,
    pub media: MediaIdentity,
    pub title: String,
    pub episode: Option<EpisodeNumber>,
    pub imported_at: String,
    pub quality: Option<DownloadQuality>,
    pub plex: PlexImportStatus,
    pub subtitles: SubtitleImportStatus,
    pub observed_at_unix_ms: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SearchDispatch {
    NotRequested,
    Queued { command_id: u32 },
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AppliedResult {
    pub effect: RequestEffect,
    pub media: MediaIdentity,
    pub manager: ManagerId,
    pub upstream_id: u32,
    pub search: SearchDispatch,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct RequestFailure {
    pub code: ApiErrorCode,
    pub message: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Submission {
    Pending,
    Applying,
    Applied { result: AppliedResult },
    Reconciling { reason: ReconciliationReason },
    Failed { error: RequestFailure },
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct RequestRecord {
    pub id: RequestId,
    pub created_at_unix_ms: u64,
    pub input: CreateRequest,
    pub submission: Submission,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct HealthIssue {
    pub code: HealthIssueCode,
    pub severity: IssueSeverity,
    pub message: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HealthOutcome {
    Healthy,
    Degraded { issues: Vec<HealthIssue> },
    Unavailable { reason: UnavailableReason },
    NotConfigured,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Observation {
    NeverChecked,
    Observed {
        outcome: HealthOutcome,
        freshness: Freshness,
        observed_at_unix_ms: u64,
        expires_at_unix_ms: u64,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct ComponentHealth {
    pub component: Component,
    pub observation: Observation,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct OperationReadiness {
    pub operation: Operation,
    pub state: ReadinessState,
    pub reasons: Vec<HealthIssueCode>,
    pub expires_at_unix_ms: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct HealthSnapshot {
    pub components: Vec<ComponentHealth>,
    pub readiness: Vec<OperationReadiness>,
    pub generated_at_unix_ms: u64,
}

#[cfg(test)]
mod tests {
    use crate::media::model::{CreateRequest, SeasonSet, TmdbId};
    #[test]
    fn request_decoder_rejects_cross_kind_scope_and_unknown_fields() {
        let valid = r#"{"kind":"movie","tmdb_id":14337,"intent":{"mode":"search_now"}}"#;
        assert!(serde_json::from_str::<CreateRequest>(valid).is_ok());
        assert!(
            serde_json::from_str::<CreateRequest>(
                &valid.replace("\"tmdb_id\":14337", "\"tvdb_id\":14337")
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<CreateRequest>(&valid.replace(
                "\"mode\":\"search_now\"",
                "\"mode\":\"search_now\",\"force\":true"
            ))
            .is_err()
        );
        assert!(
            serde_json::from_str::<CreateRequest>(
                &valid.replace("\"intent\":", "\"profile\":{},\"intent\":")
            )
            .is_err()
        );
    }
    #[test]
    fn validated_ids_and_seasons_reject_invalid_values() {
        assert!(serde_json::from_str::<TmdbId>("0").is_err());
        assert!(serde_json::from_str::<TmdbId>("2147483648").is_err());
        assert!(SeasonSet::try_from(vec![]).is_err());
        assert!(SeasonSet::try_from(vec![2, 2]).is_err());
        assert_eq!(SeasonSet::try_from(vec![2, 0]).unwrap().numbers(), [0, 2]);
    }
}
