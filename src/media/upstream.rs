//! Bounded native clients. Only configured origins receive their credentials.
use crate::{config::MediaConnection, media::model::*};
use reqwest::{Client, Method, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;
use tokio::sync::watch;

#[derive(Clone)]
pub struct EgressBoundary {
    local_subnets: Vec<ipnet::IpNet>,
    status: watch::Receiver<crate::status::Status>,
}
impl EgressBoundary {
    pub fn new(
        local_subnets: Vec<ipnet::IpNet>,
        status: watch::Receiver<crate::status::Status>,
    ) -> Result<Self, ApiErrorCode> {
        let network = crate::config::NetworkConfig { local_subnets };
        network
            .validate()
            .map_err(|_| ApiErrorCode::InvalidArguments)?;
        Ok(Self {
            local_subnets: network.local_subnets,
            status,
        })
    }
    pub fn authorize(&self, url: &Url) -> Result<(), ApiErrorCode> {
        // Literal bypass destinations need no external DNS or VPN. Hostnames
        // and all other origins require protection before any lookup or
        // credential-bearing connection. Gluetun remains the kernel-level
        // authority if protection changes after this preflight.
        let local = match url.host() {
            Some(url::Host::Ipv4(ip)) => Some(std::net::IpAddr::V4(ip)),
            Some(url::Host::Ipv6(ip)) => Some(std::net::IpAddr::V6(ip)),
            _ => None,
        }
        .is_some_and(|ip| {
            ip.is_loopback() || self.local_subnets.iter().any(|net| net.contains(&ip))
        });
        if local {
            return Ok(());
        }
        let status = self.status.borrow();
        let fresh = status.diagnostics.checked_at_unix_ms.is_some_and(|at| {
            crate::monitor::unix_ms()
                .checked_sub(at)
                .is_some_and(|age| age <= 30_000)
        });
        if fresh
            && status.vpn.state == crate::status::VpnState::Connected
            && status.diagnostics.checks.ready()
        {
            Ok(())
        } else {
            Err(ApiErrorCode::UpstreamUnavailable)
        }
    }
}

pub const RESPONSE_LIMIT: usize = 8 * 1024 * 1024;
#[derive(Clone)]
pub struct Upstream {
    client: Client,
    connection: MediaConnection,
    prefix: &'static str,
    egress: EgressBoundary,
}
impl Upstream {
    pub fn new(
        connection: MediaConnection,
        prefix: &'static str,
        egress: EgressBoundary,
    ) -> Result<Self, ApiErrorCode> {
        let client = Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
        Ok(Self {
            client,
            connection,
            prefix,
            egress,
        })
    }
    pub fn configured(&self) -> bool {
        !self.connection.url.is_empty() && !self.connection.api_key.is_empty()
    }
    fn url(&self, path: &str) -> Result<Url, ApiErrorCode> {
        if !self.configured() {
            return Err(ApiErrorCode::NotConfigured);
        }
        let mut base = Url::parse(&format!("{}/", self.connection.url.trim_end_matches('/')))
            .map_err(|_| ApiErrorCode::NotConfigured)?;
        base.set_query(None);
        let url = base
            .join(&format!(
                "{}/{}",
                self.prefix.trim_matches('/'),
                path.trim_start_matches('/')
            ))
            .map_err(|_| ApiErrorCode::InvalidArguments)?;
        self.egress.authorize(&url)?;
        Ok(url)
    }
    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ApiErrorCode> {
        let response = self
            .client
            .get(self.url(path)?)
            .header("X-Api-Key", &*self.connection.api_key)
            .query(query)
            .send()
            .await
            .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
        decode(response).await
    }
    pub async fn write<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> Result<T, ApiErrorCode> {
        let response = self
            .client
            .request(method, self.url(path)?)
            .header("X-Api-Key", &*self.connection.api_key)
            .json(body)
            .send()
            .await
            .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
        decode(response).await
    }
    pub async fn write_empty<B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> Result<(), ApiErrorCode> {
        let response = self
            .client
            .request(method, self.url(path)?)
            .header("X-Api-Key", &*self.connection.api_key)
            .json(body)
            .send()
            .await
            .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
        if !response.status().is_success() {
            return Err(ApiErrorCode::UpstreamUnavailable);
        }
        read_bytes(response, RESPONSE_LIMIT).await.map(|_| ())
    }
    pub async fn artwork(
        &self,
        image: &UpImage,
        allow_remote: bool,
    ) -> Result<(String, Vec<u8>), ApiErrorCode> {
        let base = Url::parse(&format!("{}/", self.connection.url.trim_end_matches('/')))
            .map_err(|_| ApiErrorCode::NotConfigured)?;
        let mut selected = None;
        for candidate in [&image.url, &image.remote_url] {
            let Ok(url) = base.join(candidate) else {
                continue;
            };
            let prefix = format!("{}/MediaCover/", base.path().trim_end_matches('/'));
            let local = url.origin() == base.origin()
                && (url.path().starts_with("/MediaCover/") || url.path().starts_with(&prefix));
            let remote = allow_remote
                && url.scheme() == "https"
                && url.host_str() == Some("image.tmdb.org")
                && url.port_or_known_default() == Some(443)
                && url.path().starts_with("/t/p/");
            if (local || remote) && url.username().is_empty() && url.password().is_none() {
                selected = Some((url, local));
                break;
            }
        }
        let (url, local) = selected.ok_or(ApiErrorCode::NotFound)?;
        self.egress.authorize(&url)?;
        let mut request = self.client.get(url);
        if local {
            request = request.header("X-Api-Key", &*self.connection.api_key)
        }
        let response = request
            .send()
            .await
            .map_err(|_| ApiErrorCode::UpstreamUnavailable)?;
        if !response.status().is_success() {
            return Err(ApiErrorCode::NotFound);
        }
        let content = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("");
        if !matches!(content, "image/jpeg" | "image/png" | "image/webp") {
            return Err(ApiErrorCode::InvalidUpstreamResponse);
        }
        let content = content.to_owned();
        let bytes = read_bytes(response, 2 * 1024 * 1024).await?;
        // Never turn an HTML error page with an image header into browser content.
        let valid = match content.as_str() {
            "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
            _ => false,
        };
        if !valid {
            return Err(ApiErrorCode::InvalidUpstreamResponse);
        }
        Ok((content, bytes))
    }
}
pub async fn read_bytes(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ApiErrorCode> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(ApiErrorCode::InvalidUpstreamResponse);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ApiErrorCode::UpstreamUnavailable)?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(ApiErrorCode::InvalidUpstreamResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, ApiErrorCode> {
    if !response.status().is_success() {
        return Err(if response.status() == reqwest::StatusCode::NOT_FOUND {
            ApiErrorCode::NotFound
        } else {
            ApiErrorCode::UpstreamUnavailable
        });
    }
    serde_json::from_slice(&read_bytes(response, RESPONSE_LIMIT).await?)
        .map_err(|_| ApiErrorCode::InvalidUpstreamResponse)
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpImage {
    #[serde(default)]
    pub cover_type: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub remote_url: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpMovie {
    #[serde(default)]
    pub id: u32,
    pub tmdb_id: u32,
    pub title: String,
    #[serde(default)]
    pub title_slug: String,
    #[serde(default)]
    pub year: u16,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub images: Vec<UpImage>,
    #[serde(default)]
    pub quality_profile_id: u32,
    #[serde(default)]
    pub root_folder_path: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub minimum_availability: String,
    #[serde(default)]
    pub monitored: bool,
    #[serde(default)]
    pub has_file: bool,
    #[serde(default)]
    pub movie_file_id: u32,
    #[serde(default)]
    pub tags: Vec<u32>,
    #[serde(flatten)]
    pub metadata: UpMovieMetadata,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpMovieMetadata {
    pub runtime: Option<u32>,
    pub genres: Vec<String>,
    pub studio: Option<String>,
    pub certification: Option<String>,
    pub movie_file: Option<UpMovieFile>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpMovieFile {
    #[serde(default)]
    pub quality: Option<UpDownloadQuality>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub media_info: Option<UpMediaInfo>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpMediaInfo {
    #[serde(default)]
    pub video_codec: Option<String>,
    #[serde(default)]
    pub audio_codec: Option<String>,
    #[serde(default)]
    pub audio_channels: Option<f64>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpStatistics {
    #[serde(default)]
    pub episode_count: u32,
    #[serde(default)]
    pub episode_file_count: u32,
    #[serde(default)]
    pub total_episode_count: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpSeason {
    pub season_number: u16,
    #[serde(default)]
    pub monitored: bool,
    #[serde(default)]
    pub statistics: Option<UpStatistics>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpSeries {
    #[serde(default)]
    pub id: u32,
    pub tvdb_id: u32,
    pub title: String,
    #[serde(default)]
    pub title_slug: String,
    #[serde(default)]
    pub year: u16,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub images: Vec<UpImage>,
    #[serde(default)]
    pub quality_profile_id: u32,
    #[serde(default)]
    pub root_folder_path: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub monitored: bool,
    #[serde(default)]
    pub season_folder: bool,
    #[serde(default)]
    pub monitor_new_items: String,
    #[serde(default)]
    pub series_type: String,
    #[serde(default)]
    pub tags: Vec<u32>,
    #[serde(default)]
    pub seasons: Vec<UpSeason>,
    #[serde(flatten)]
    pub metadata: UpSeriesMetadata,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpSeriesMetadata {
    pub runtime: Option<u32>,
    pub genres: Vec<String>,
    pub network: Option<String>,
    pub certification: Option<String>,
    pub next_airing: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpEpisode {
    pub id: u32,
    pub season_number: u16,
    pub episode_number: u16,
    pub title: String,
    #[serde(default)]
    pub air_date: Option<String>,
    #[serde(default)]
    pub air_date_utc: Option<String>,
    #[serde(default)]
    pub monitored: bool,
    #[serde(default)]
    pub has_file: bool,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpQueue {
    pub id: u32,
    pub title: String,
    #[serde(default)]
    pub movie_id: Option<u32>,
    #[serde(default)]
    pub series_id: Option<u32>,
    #[serde(default)]
    pub episode_id: Option<u32>,
    #[serde(default)]
    pub movie: Option<UpMovie>,
    #[serde(default)]
    pub series: Option<UpSeries>,
    #[serde(default)]
    pub episode: Option<UpQueueEpisode>,
    #[serde(default)]
    pub size: Option<f64>,
    #[serde(default)]
    pub sizeleft: Option<f64>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub tracked_download_status: String,
    #[serde(default)]
    pub tracked_download_state: String,
    #[serde(default)]
    pub quality: Option<UpDownloadQuality>,
    #[serde(default)]
    pub timeleft: Option<String>,
    #[serde(default)]
    pub download_client: Option<String>,
    #[serde(default)]
    pub indexer: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub languages: Vec<UpNamed>,
    #[serde(default)]
    pub custom_formats: Vec<UpNamed>,
    #[serde(default)]
    pub custom_format_score: Option<i32>,
    #[serde(default)]
    pub added: Option<String>,
    #[serde(default)]
    pub estimated_completion_time: Option<String>,
    #[serde(default)]
    pub status_messages: Vec<UpDownloadMessage>,
    #[serde(default)]
    pub error_message: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpDownloadQuality {
    pub quality: UpQuality,
    #[serde(default)]
    pub revision: Option<UpRevision>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpQuality {
    pub name: String,
    #[serde(default)]
    pub resolution: Option<u16>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub modifier: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpRevision {
    pub version: u32,
    pub real: u32,
    pub is_repack: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpDownloadMessage {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub messages: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpQueueEpisode {
    pub season_number: u16,
    pub episode_number: u16,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpQueuePage {
    pub records: Vec<UpQueue>,
    #[serde(default)]
    pub total_records: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpNamed {
    pub id: u32,
    #[serde(alias = "label")]
    pub name: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpTag {
    pub id: u32,
    pub label: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpRoot {
    pub id: u32,
    pub path: String,
    #[serde(default)]
    pub accessible: bool,
    #[serde(default, rename = "freeSpace")]
    pub free_space: Option<u64>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpHealth {
    pub source: String,
    #[serde(rename = "type")]
    pub severity: String,
    // Private input for exact release-notice recognition, never public health text.
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default, rename = "wikiUrl")]
    pub wiki_url: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpStatus {
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "name")]
pub enum Command {
    MoviesSearch {
        #[serde(rename = "movieIds")]
        movie_ids: Vec<u32>,
    },
    EpisodeSearch {
        #[serde(rename = "episodeIds")]
        episode_ids: Vec<u32>,
    },
    #[serde(other)]
    Other,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpCommand {
    pub id: u32,
    #[serde(default)]
    pub body: Option<Command>,
    #[serde(default)]
    pub queued: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMovie<'a> {
    pub title: &'a str,
    pub title_slug: &'a str,
    pub year: u16,
    pub tmdb_id: u32,
    pub quality_profile_id: u32,
    pub root_folder_path: &'a str,
    pub monitored: bool,
    pub minimum_availability: &'a str,
    pub tags: Vec<u32>,
    pub images: &'a [UpImage],
    pub add_options: MovieAddOptions,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovieAddOptions {
    pub search_for_movie: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddSeries<'a> {
    pub title: &'a str,
    pub title_slug: &'a str,
    pub year: u16,
    pub tvdb_id: u32,
    pub quality_profile_id: u32,
    pub root_folder_path: &'a str,
    pub monitored: bool,
    pub season_folder: bool,
    pub series_type: &'a str,
    pub monitor_new_items: &'a str,
    pub seasons: Vec<UpSeason>,
    pub tags: Vec<u32>,
    pub images: &'a [UpImage],
    pub add_options: SeriesAddOptions,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesAddOptions {
    pub monitor: &'static str,
    pub search_for_missing_episodes: bool,
    pub search_for_cutoff_unmet_episodes: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorEpisodes {
    pub episode_ids: Vec<u32>,
    pub monitored: bool,
}

#[cfg(test)]
mod tests {
    use crate::{
        config::MediaConnection,
        media::{
            model::ApiErrorCode,
            upstream::{EgressBoundary, UpStatus, Upstream},
        },
        monitor::{Check, Checks, Outcome},
        status::{Status, VpnState},
    };
    use tokio::sync::watch;

    #[tokio::test]
    async fn external_origins_are_blocked_before_lookup_until_protection_is_fresh() {
        let (status, rx) = watch::channel(Status::needs_configuration("fixture"));
        let boundary =
            EgressBoundary::new(vec!["192.168.20.0/24".parse().unwrap()], rx.clone()).unwrap();
        assert!(
            boundary
                .authorize(&"http://192.168.20.4:7878".parse().unwrap())
                .is_ok()
        );
        assert!(
            boundary
                .authorize(&"http://127.0.0.1:7878".parse().unwrap())
                .is_ok()
        );
        assert_eq!(
            boundary.authorize(&"https://manager.example".parse().unwrap()),
            Err(ApiErrorCode::UpstreamUnavailable)
        );
        let upstream = Upstream::new(
            MediaConnection {
                url: "http://never-resolve.invalid".into(),
                api_key: "fixture-key".to_owned().into(),
            },
            "api/v3",
            boundary.clone(),
        )
        .unwrap();
        assert!(matches!(
            upstream.get::<UpStatus>("system/status", &[]).await,
            Err(ApiErrorCode::UpstreamUnavailable)
        ));
        let passed = Check {
            outcome: Outcome::Passed,
            ..Check::default()
        };
        status.send_modify(|value| {
            value.vpn.state = VpnState::Connected;
            value.diagnostics.checked_at_unix_ms = Some(crate::monitor::unix_ms());
            value.diagnostics.checks = Checks {
                vpn_service: passed,
                vpn_health: passed,
                dns_service: passed,
                dns_lookup: passed,
                resolver: passed,
                firewall: passed,
            };
        });
        assert!(
            boundary
                .authorize(&"https://manager.example".parse().unwrap())
                .is_ok()
        );
        status.send_modify(|value| {
            value.diagnostics.checked_at_unix_ms = Some(crate::monitor::unix_ms() - 30_001)
        });
        assert_eq!(
            boundary.authorize(&"https://manager.example".parse().unwrap()),
            Err(ApiErrorCode::UpstreamUnavailable)
        );
        assert!(EgressBoundary::new(vec!["0.0.0.0/0".parse().unwrap()], rx).is_err());
    }
}
