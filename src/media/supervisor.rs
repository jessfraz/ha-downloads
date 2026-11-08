//! Optional, read-only app update evidence. All token-bearing requests stay on
//! Supervisor's documented private HTTP API; proxies and redirects are disabled.
//! Default hassio role permits /info reads, not app management operations.
use crate::media::upstream::read_bytes;
use reqwest::{Client, Url};
use serde::{Deserialize, de::DeserializeOwned};
use std::{collections::BTreeMap, time::Duration};
use zeroize::Zeroizing;

pub(crate) enum UpdateSource {
    Standalone,
    HomeAssistant(Option<Supervisor>),
}
#[derive(Clone, Copy)]
pub(crate) enum UpdateAvailability {
    Standalone,
    HomeAssistant(Option<bool>),
}
impl UpdateAvailability {
    pub fn allows_notice(self) -> bool {
        matches!(self, Self::Standalone | Self::HomeAssistant(Some(true)))
    }
}
pub(crate) struct Supervisor {
    client: Client,
    base: Url,
    token: Zeroizing<String>,
}
#[derive(Deserialize)]
struct Envelope<T> {
    result: String,
    data: T,
}
#[derive(Deserialize)]
struct SupervisorInfo {
    addons: Vec<AppSummary>,
}
#[derive(Deserialize)]
struct AppSummary {
    slug: String,
    name: String,
}
#[derive(Deserialize)]
struct AppInfo {
    hostname: String,
    ip_address: String,
    network: BTreeMap<String, Option<u16>>,
    host_network: bool,
    version: String,
    version_latest: String,
    update_available: bool,
}
#[derive(Deserialize)]
struct CoreInfo {
    ip_address: String,
}
impl UpdateSource {
    pub fn from_environment() -> Self {
        match std::env::var("SUPERVISOR_TOKEN") {
            Err(std::env::VarError::NotPresent) => Self::Standalone,
            Ok(token) if !token.is_empty() => {
                Self::HomeAssistant(Supervisor::new("http://supervisor/", token))
            }
            _ => Self::HomeAssistant(None),
        }
    }
    pub async fn prowlarr(&self, origin: &str) -> UpdateAvailability {
        match self {
            Self::Standalone => UpdateAvailability::Standalone,
            Self::HomeAssistant(Some(client)) => UpdateAvailability::HomeAssistant(
                tokio::time::timeout(Duration::from_secs(5), client.prowlarr(origin))
                    .await
                    .ok()
                    .flatten(),
            ),
            Self::HomeAssistant(None) => UpdateAvailability::HomeAssistant(None),
        }
    }
    #[cfg(test)]
    pub fn for_test(base: &str) -> Self {
        Self::HomeAssistant(Supervisor::new(base, "fixture-supervisor-token".into()))
    }
}
impl Supervisor {
    fn new(base: &str, token: String) -> Option<Self> {
        Some(Self {
            client: Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()
                .ok()?,
            base: Url::parse(base).ok()?,
            token: Zeroizing::new(token),
        })
    }
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ()> {
        let response = self
            .client
            .get(self.base.join(path).map_err(|_| ())?)
            .bearer_auth(&*self.token)
            .send()
            .await
            .map_err(|_| ())?;
        if !response.status().is_success() {
            return Err(());
        }
        let envelope: Envelope<T> =
            serde_json::from_slice(&read_bytes(response, 256 * 1024).await.map_err(|_| ())?)
                .map_err(|_| ())?;
        if envelope.result != "ok" {
            return Err(());
        }
        Ok(envelope.data)
    }
    async fn prowlarr(&self, origin: &str) -> Option<bool> {
        let origin = Url::parse(origin).ok()?;
        let host = origin.host_str()?;
        let configured_ip = match origin.host() {
            Some(url::Host::Ipv4(ip)) => Some(std::net::IpAddr::V4(ip)),
            Some(url::Host::Ipv6(ip)) => Some(std::net::IpAddr::V6(ip)),
            _ => None,
        };
        let port = origin.port_or_known_default()?;
        let inventory: SupervisorInfo = self.get("supervisor/info").await.ok()?;
        if inventory.addons.len() > 200 {
            return None;
        }
        let candidates = inventory
            .addons
            .into_iter()
            .filter(|app| {
                (app.slug.ends_with("_prowlarr") || app.name.eq_ignore_ascii_case("prowlarr"))
                    && !app.slug.is_empty()
                    && app.slug.len() <= 100
                    && app
                        .slug
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            })
            .collect::<Vec<_>>();
        if candidates.len() > 8 {
            return None;
        }
        let core = self.get::<CoreInfo>("core/info").await.ok();
        let mut matching = vec![];
        for candidate in candidates {
            let info: AppInfo = self
                .get(&format!("addons/{}/info", candidate.slug))
                .await
                .ok()?;
            let direct = port == 9696
                && (host.eq_ignore_ascii_case(&info.hostname)
                    || configured_ip
                        .zip(info.ip_address.parse::<std::net::IpAddr>().ok())
                        .is_some_and(|(host, app)| host == app));
            let mapped = !info.host_network
                && core.as_ref().is_some_and(|core| {
                    configured_ip
                        .zip(core.ip_address.parse::<std::net::IpAddr>().ok())
                        .is_some_and(|(host, core)| host == core)
                })
                && info.network.get("9696/tcp").copied().flatten() == Some(port);
            if direct || mapped {
                if info.version.is_empty() || info.version_latest.is_empty() {
                    return None;
                }
                if info.update_available && info.version == info.version_latest {
                    return None;
                }
                matching.push(info.update_available);
            }
        }
        match matching.as_slice() {
            [available] => Some(*available),
            _ => None,
        }
    }
}
