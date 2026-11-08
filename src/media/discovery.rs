//! Publish only the adapter credential through Supervisor's scoped discovery API.
//!
//! Supervisor documents these fixed endpoints as HTTP on its private app
//! network: https://contributors.home-assistant.io/apps/guides/communication/
//! Its scoped token and native network are the platform trust boundary. This
//! client disables ambient proxies and redirects for both platform requests.
use crate::media::upstream::read_bytes;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::watch;

#[derive(Deserialize)]
struct Envelope<T> {
    result: String,
    data: T,
}
#[derive(Deserialize)]
struct AppInfo {
    hostname: String,
}
#[derive(Serialize)]
struct Announcement<'a> {
    service: &'static str,
    config: Connection<'a>,
}
#[derive(Serialize)]
struct Connection<'a> {
    backend_url: String,
    integration_token: &'a str,
}
#[derive(Deserialize)]
struct Discovery {
    uuid: String,
}

pub async fn run(integration_token: zeroize::Zeroizing<String>, mut stop: watch::Receiver<bool>) {
    if integration_token.is_empty() {
        return;
    }
    let Ok(token) = std::env::var("SUPERVISOR_TOKEN") else {
        return;
    };
    let token = zeroize::Zeroizing::new(token);
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return;
    };
    loop {
        let result = tokio::select! {_=stop.changed()=>return,result=announce(&client,&token,&integration_token)=>result};
        if result.is_ok() {
            return;
        }
        tokio::select! {_=stop.changed()=>return,_=tokio::time::sleep(Duration::from_secs(30))=>{}}
    }
}
async fn announce(
    client: &reqwest::Client,
    token: &str,
    integration_token: &str,
) -> Result<(), ()> {
    let response = client
        .get("http://supervisor/addons/self/info")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    let info: Envelope<AppInfo> =
        serde_json::from_slice(&read_bytes(response, 64 * 1024).await.map_err(|_| ())?)
            .map_err(|_| ())?;
    if info.result != "ok"
        || info.data.hostname.is_empty()
        || info.data.hostname.len() > 253
        || !info
            .data
            .hostname
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return Err(());
    }
    let response = client
        .post("http://supervisor/discovery")
        .bearer_auth(token)
        .json(&Announcement {
            service: "downloads",
            config: Connection {
                backend_url: format!("http://{}:8080", info.data.hostname),
                integration_token,
            },
        })
        .send()
        .await
        .map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    let result: Envelope<Discovery> =
        serde_json::from_slice(&read_bytes(response, 64 * 1024).await.map_err(|_| ())?)
            .map_err(|_| ())?;
    if result.result != "ok" || result.data.uuid.is_empty() {
        return Err(());
    }
    Ok(())
}
