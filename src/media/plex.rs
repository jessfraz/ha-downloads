//! Optional proof that Plex observes this exact imported file, not an older
//! edition with the same title. Paths remain private comparison inputs only.
use crate::{
    config::MediaConnection,
    media::{
        details::label,
        model::*,
        requests::{utc_instant, utc_timestamp},
        upstream::{EgressBoundary, read_bytes},
    },
};
use reqwest::{Client, Url};
use serde::{Deserialize, de::DeserializeOwned};
use std::time::Duration;

pub(crate) struct ProofInput {
    pub guid: Option<String>,
    pub plex_type: u8,
    pub episode: Option<EpisodeNumber>,
    pub imported_at: String,
    pub path: Option<String>,
    pub size: Option<u64>,
}
pub(crate) struct Plex<'a> {
    connection: &'a MediaConnection,
    egress: &'a EgressBoundary,
    client: Client,
}
#[derive(Deserialize)]
struct Envelope<T> {
    #[serde(rename = "MediaContainer")]
    container: T,
}
#[derive(Deserialize)]
struct Sections {}
#[derive(Deserialize)]
struct MetadataContainer {
    #[serde(default, rename = "Metadata")]
    metadata: Vec<Metadata>,
    #[serde(default, rename = "totalSize")]
    total_size: Option<Number>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Number {
    Integer(u64),
    Text(String),
}
impl Number {
    fn get(&self) -> Option<u64> {
        match self {
            Self::Integer(value) => Some(*value),
            Self::Text(value) => value.parse().ok(),
        }
    }
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Boolean {
    Bool(bool),
    Integer(u8),
}
impl Boolean {
    fn get(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            Self::Integer(0) => Some(false),
            Self::Integer(1) => Some(true),
            _ => None,
        }
    }
}
#[derive(Deserialize)]
struct Guid {
    id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    #[serde(default)]
    rating_key: Option<String>,
    #[serde(default)]
    guid: Option<String>,
    #[serde(default, rename = "Guid")]
    guids: Vec<Guid>,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    updated_at: Option<Number>,
    #[serde(default)]
    parent_index: Option<u16>,
    #[serde(default)]
    index: Option<u16>,
    #[serde(default, rename = "Media")]
    media: Vec<Media>,
}
#[derive(Deserialize)]
struct Media {
    #[serde(default, rename = "Part")]
    parts: Vec<Part>,
}
#[derive(Deserialize)]
struct Part {
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    size: Option<Number>,
    #[serde(default)]
    exists: Option<Boolean>,
    #[serde(default)]
    accessible: Option<Boolean>,
    #[serde(default, rename = "Stream")]
    streams: Option<Vec<Stream>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stream {
    stream_type: u8,
    #[serde(default)]
    language_code: Option<String>,
    #[serde(default)]
    language: Option<String>,
}
impl<'a> Plex<'a> {
    pub fn new(
        connection: &'a MediaConnection,
        egress: &'a EgressBoundary,
    ) -> Result<Self, UnavailableReason> {
        Ok(Self {
            connection,
            egress,
            client: Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(4))
                .build()
                .map_err(|_| UnavailableReason::Connection)?,
        })
    }
    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, UnavailableReason> {
        let base = Url::parse(&format!("{}/", self.connection.url.trim_end_matches('/')))
            .map_err(|_| UnavailableReason::InvalidResponse)?;
        let url = base
            .join(path)
            .map_err(|_| UnavailableReason::InvalidResponse)?;
        self.egress
            .authorize(&url)
            .map_err(|_| UnavailableReason::Connection)?;
        let mut request = self
            .client
            .get(url)
            .query(query)
            .header("Accept", "application/json")
            .header("X-Plex-Container-Start", "0")
            .header("X-Plex-Container-Size", "8");
        if !self.connection.api_key.is_empty() {
            request = request.header("X-Plex-Token", &*self.connection.api_key);
        }
        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                UnavailableReason::Timeout
            } else {
                UnavailableReason::Connection
            }
        })?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(UnavailableReason::Authentication);
        }
        if !response.status().is_success() {
            return Err(UnavailableReason::Connection);
        }
        let bytes = read_bytes(response, 2 * 1024 * 1024)
            .await
            .map_err(|_| UnavailableReason::InvalidResponse)?;
        let envelope: Envelope<T> =
            serde_json::from_slice(&bytes).map_err(|_| UnavailableReason::InvalidResponse)?;
        Ok(envelope.container)
    }
    pub async fn available(&self) -> Result<(), UnavailableReason> {
        self.get::<Sections>("library/sections", &[])
            .await
            .map(|_| ())
    }
    pub async fn verify(&self, proof: &ProofInput) -> (PlexImportStatus, SubtitleImportStatus) {
        let Some(guid) = proof.guid.as_deref() else {
            return unknown(ImportVerificationReason::IdentityUnavailable);
        };
        let (Some(path), Some(size)) = (
            proof.path.as_deref().filter(|path| !path.is_empty()),
            proof.size,
        ) else {
            return unknown(ImportVerificationReason::FileEvidenceMissing);
        };
        let query = [
            ("type", proof.plex_type.to_string()),
            ("guid", guid.to_owned()),
            ("includeGuids", "1".into()),
        ];
        let candidates = match self.get::<MetadataContainer>("library/all", &query).await {
            Ok(value) => value,
            Err(reason) => return unavailable(reason),
        };
        if candidates.metadata.len() > 8
            || candidates
                .total_size
                .as_ref()
                .and_then(Number::get)
                .is_some_and(|total| total > candidates.metadata.len() as u64)
        {
            return unknown(ImportVerificationReason::AmbiguousMatch);
        }
        let mut verified = vec![];
        let mut older = false;
        let mut missing = false;
        let mut seen = std::collections::BTreeSet::new();
        for candidate in candidates.metadata {
            let Some(key) = candidate.rating_key.filter(|key| {
                !key.is_empty()
                    && key.len() <= 20
                    && key.bytes().all(|byte| byte.is_ascii_digit())
                    && key.parse::<u64>().is_ok_and(|key| key > 0)
            }) else {
                missing = true;
                continue;
            };
            if !seen.insert(key.clone()) {
                continue;
            }
            // PMS documents that metadata GET can refresh analysis by default.
            // Explicitly skip that work; request only current file availability.
            // https://developer.plex.tv/pms/ (GET /library/metadata/{ids})
            let query = [
                ("includeGuids", "1".into()),
                ("skipRefresh", "1".into()),
                ("checkFileAvailability", "1".into()),
            ];
            let details = match self
                .get::<MetadataContainer>(&format!("library/metadata/{key}"), &query)
                .await
            {
                Ok(value) => value,
                Err(reason) => return unavailable(reason),
            };
            if details.metadata.len() > 8 {
                return unknown(ImportVerificationReason::AmbiguousMatch);
            }
            for metadata in details.metadata {
                let identity = metadata.guid.as_deref() == Some(guid)
                    || metadata.guids.iter().any(|value| value.id == guid);
                let kind = match proof.plex_type {
                    1 => metadata.kind == "movie",
                    4 => {
                        metadata.kind == "episode"
                            && proof.episode.as_ref().is_some_and(|episode| {
                                metadata.parent_index == Some(episode.season)
                                    && metadata.index == Some(episode.number)
                            })
                    }
                    _ => false,
                };
                if !identity || !kind {
                    continue;
                }
                let updated = metadata
                    .updated_at
                    .as_ref()
                    .and_then(Number::get)
                    .and_then(|seconds| seconds.checked_mul(1000))
                    .filter(|ms| *ms <= crate::monitor::unix_ms())
                    .and_then(|ms| utc_timestamp(ms).ok())
                    .and_then(|value| utc_instant(&value));
                let current = updated
                    .zip(utc_instant(&proof.imported_at))
                    .map(|(updated, imported)| updated >= imported);
                for part in metadata.media.into_iter().flat_map(|media| media.parts) {
                    if part.file.as_deref() != Some(path)
                        || part.size.as_ref().and_then(Number::get) != Some(size)
                    {
                        continue;
                    }
                    match current {
                        Some(false) => {
                            older = true;
                            continue;
                        }
                        None => {
                            missing = true;
                            continue;
                        }
                        Some(true) => {}
                    }
                    if part.exists.as_ref().and_then(Boolean::get) != Some(true)
                        || part.accessible.as_ref().and_then(Boolean::get) == Some(false)
                    {
                        missing = true;
                        continue;
                    }
                    verified.push(subtitles(part.streams));
                }
            }
        }
        if verified.len() > 1 {
            return unknown(ImportVerificationReason::AmbiguousMatch);
        }
        if let Some(subtitles) = verified.pop() {
            return (
                PlexImportStatus::Verified {
                    observed_at_unix_ms: crate::monitor::unix_ms(),
                },
                subtitles,
            );
        }
        if older {
            unknown(ImportVerificationReason::OlderVersion)
        } else if missing {
            unknown(ImportVerificationReason::FileEvidenceMissing)
        } else {
            unknown(ImportVerificationReason::NotMatched)
        }
    }
}
fn unknown(reason: ImportVerificationReason) -> (PlexImportStatus, SubtitleImportStatus) {
    (
        PlexImportStatus::Unknown { reason },
        SubtitleImportStatus::Unknown {
            reason: SubtitleUnknownReason::PlexUnverified,
        },
    )
}
fn unavailable(reason: UnavailableReason) -> (PlexImportStatus, SubtitleImportStatus) {
    (
        PlexImportStatus::Unavailable { reason },
        SubtitleImportStatus::Unknown {
            reason: SubtitleUnknownReason::PlexUnverified,
        },
    )
}
fn subtitles(streams: Option<Vec<Stream>>) -> SubtitleImportStatus {
    let Some(streams) = streams.filter(|streams| streams.len() <= 256) else {
        return SubtitleImportStatus::Unknown {
            reason: SubtitleUnknownReason::StreamsUnavailable,
        };
    };
    if streams.iter().any(|stream| stream.stream_type == 3) {
        let mut languages = streams
            .iter()
            .filter(|stream| stream.stream_type == 3)
            .filter_map(|stream| {
                stream
                    .language_code
                    .as_deref()
                    .or(stream.language.as_deref())
                    .and_then(label)
            })
            .collect::<Vec<_>>();
        languages.sort();
        languages.dedup();
        SubtitleImportStatus::Verified {
            languages,
            observed_at_unix_ms: crate::monitor::unix_ms(),
        }
    } else if streams
        .iter()
        .any(|stream| matches!(stream.stream_type, 1 | 2))
    {
        SubtitleImportStatus::Missing {
            observed_at_unix_ms: crate::monitor::unix_ms(),
        }
    } else {
        SubtitleImportStatus::Unknown {
            reason: SubtitleUnknownReason::StreamsUnavailable,
        }
    }
}
