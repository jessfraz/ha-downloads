use crate::status::{DownloaderState, State, Status, StatusSnapshot};
use dropshot::{
    ApiDescription, Body, ClientErrorStatusCode, ConfigDropshot, HttpError, HttpResponseOk,
    HttpServer, HttpServerStarter, Path, RequestContext, UntypedBody, endpoint,
};
use http::{HeaderMap, HeaderName, Method, Response};
use regex::{Captures, Regex};
use schemars::JsonSchema;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::sync::watch;

const BODY_LIMIT: usize = 16 * 1024 * 1024;
pub enum Access {
    Ingress { peer: IpAddr },
    Direct { token_hash: Option<[u8; 32]> },
}
pub struct Context {
    access: Access,
    status: watch::Receiver<Status>,
    client: reqwest::Client,
    input: Regex,
    value: Regex,
    upstream: reqwest::Url,
}
impl Context {
    pub fn new(access: Access, status: watch::Receiver<Status>) -> anyhow::Result<Self> {
        Ok(Self {
            access,
            status,
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(60))
                .build()?,
            input: Regex::new(r#"(?i)<input\b[^>]*\bname\s*=\s*["']url_base["'][^>]*>"#)?,
            value: Regex::new(r#"(?i)\bvalue\s*=\s*(?:"[^"]*"|'[^']*')"#)?,
            upstream: reqwest::Url::parse("http://127.0.0.1:8081/")?,
        })
    }
    pub fn token_hash(token: &str) -> Option<[u8; 32]> {
        (!token.is_empty()).then(|| Sha256::digest(token.as_bytes()).into())
    }
}
fn denied(code: ClientErrorStatusCode, message: &str) -> HttpError {
    HttpError::for_client_error(None, code, message.into())
}
fn forbidden() -> HttpError {
    denied(ClientErrorStatusCode::FORBIDDEN, "Access denied.")
}
fn unauthorized() -> HttpError {
    denied(
        ClientErrorStatusCode::UNAUTHORIZED,
        "A valid status API bearer token is required.",
    )
}
fn ingress(r: &RequestContext<Arc<Context>>) -> Result<(), HttpError> {
    let Access::Ingress { peer } = r.context().access else {
        return Err(forbidden());
    };
    if r.request.remote_addr().ip() != peer {
        return Err(forbidden());
    }
    let headers = r.request.headers();
    if headers
        .get("x-remote-user-id")
        .and_then(|h| h.to_str().ok())
        .is_none_or(str::is_empty)
        || !headers
            .get("x-remote-user-groups")
            .and_then(|h| h.to_str().ok())
            .is_some_and(|g| g.split(',').any(|v| v.trim() == "system-admin"))
    {
        return Err(forbidden());
    }
    Ok(())
}
fn authenticate(r: &RequestContext<Arc<Context>>) -> Result<(), HttpError> {
    match &r.context().access {
        Access::Ingress { .. } => ingress(r),
        Access::Direct { token_hash } => {
            let expected = token_hash.as_ref().ok_or_else(unauthorized)?;
            let token = r
                .request
                .headers()
                .get("authorization")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer "))
                .ok_or_else(unauthorized)?;
            let actual: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            if bool::from(expected.ct_eq(&actual)) {
                Ok(())
            } else {
                Err(unauthorized())
            }
        }
    }
}
fn prefix(r: &RequestContext<Arc<Context>>) -> Result<String, HttpError> {
    let value = r
        .request
        .headers()
        .get("x-ingress-path")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .trim_end_matches('/');
    if !value.is_empty()
        && (!value.starts_with("/api/hassio_ingress/")
            || value.len() > 2048
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-')))
    {
        return Err(denied(
            ClientErrorStatusCode::BAD_REQUEST,
            "Invalid ingress prefix.",
        ));
    }
    Ok(value.into())
}
#[endpoint {method=GET,path="/healthz",tags=["system"]}]
async fn health(r: RequestContext<Arc<Context>>) -> Result<Response<Body>, HttpError> {
    let status = r.context().status.borrow();
    let code =
        if status.state == State::Degraded && status.downloader.state == DownloaderState::Stopped {
            http::StatusCode::SERVICE_UNAVAILABLE
        } else {
            http::StatusCode::OK
        };
    // This existing internal endpoint is also consumed by Supervisor's watchdog.
    // Return only typed health fields; status contains connection identifiers.
    let body = serde_json::to_vec(&status.health())
        .map_err(|_| HttpError::for_internal_error("Health unavailable.".into()))?;
    Response::builder()
        .status(code)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(Body::from(body))
        .map_err(|_| HttpError::for_internal_error("Health unavailable.".into()))
}
#[endpoint {method=GET,path="/api/v1/status",tags=["system"]}]
async fn app_status(
    r: RequestContext<Arc<Context>>,
) -> Result<HttpResponseOk<StatusSnapshot>, HttpError> {
    authenticate(&r)?;
    let status = r.context().status.borrow().clone();
    Ok(HttpResponseOk(StatusSnapshot {
        health: status.health(),
        status,
    }))
}
#[endpoint {method=GET,path="/api/v1/health",tags=["system"]}]
async fn readiness(
    r: RequestContext<Arc<Context>>,
) -> Result<HttpResponseOk<crate::status::Health>, HttpError> {
    authenticate(&r)?;
    Ok(HttpResponseOk(r.context().status.borrow().health()))
}
#[endpoint {method=GET,path="/",tags=["web"]}]
async fn index(r: RequestContext<Arc<Context>>) -> Result<Response<Body>, HttpError> {
    ingress(&r)?;
    let base = format!("{}/", prefix(&r)?);
    content(
        include_str!("../web/index.html").replace("__DOWNLOADS_BASE__", &base),
        "text/html; charset=utf-8",
    )
}
fn content(body: String, kind: &str) -> Result<Response<Body>, HttpError> {
    Response::builder().header("content-type",kind).header("cache-control","no-store")
        .header("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-src 'self'; frame-ancestors 'self'; object-src 'none'; base-uri 'self'")
        .header("x-content-type-options","nosniff").header("referrer-policy","no-referrer")
        .body(Body::from(body)).map_err(|_|HttpError::for_internal_error("Response unavailable.".into()))
}
#[derive(Deserialize, JsonSchema)]
struct Resource {
    #[serde(rename = "path")]
    _path: Vec<String>,
}
#[endpoint {method=GET,path="/app.js",tags=["web"]}]
async fn javascript(r: RequestContext<Arc<Context>>) -> Result<Response<Body>, HttpError> {
    ingress(&r)?;
    content(
        include_str!("../web/app.js").into(),
        "text/javascript; charset=utf-8",
    )
}
#[endpoint {method=GET,path="/style.css",tags=["web"]}]
async fn stylesheet(r: RequestContext<Arc<Context>>) -> Result<Response<Body>, HttpError> {
    ingress(&r)?;
    content(
        include_str!("../web/style.css").into(),
        "text/css; charset=utf-8",
    )
}
#[endpoint {method=GET,path="/sabnzbd",unpublished=true,tags=["downloader"]}]
async fn downloader_root(r: RequestContext<Arc<Context>>) -> Result<Response<Body>, HttpError> {
    proxy(r, "sabnzbd".into(), Vec::new()).await
}
fn downloader_path(r: &RequestContext<Arc<Context>>) -> String {
    // Reassembling decoded router segments loses directory slashes and escapes.
    r.request.uri().path().trim_start_matches('/').to_owned()
}
#[endpoint {method=GET,path="/sabnzbd/{path:.*}",unpublished=true,tags=["downloader"]}]
async fn get(
    r: RequestContext<Arc<Context>>,
    _path: Path<Resource>,
) -> Result<Response<Body>, HttpError> {
    let path = downloader_path(&r);
    proxy(r, path, Vec::new()).await
}
#[endpoint {method=POST,path="/sabnzbd/{path:.*}",unpublished=true,tags=["downloader"]}]
async fn post(
    r: RequestContext<Arc<Context>>,
    _path: Path<Resource>,
    body: UntypedBody,
) -> Result<Response<Body>, HttpError> {
    let path = downloader_path(&r);
    proxy(r, path, body.as_bytes().to_vec()).await
}
#[endpoint {method=PUT,path="/sabnzbd/{path:.*}",unpublished=true,tags=["downloader"]}]
async fn put(
    r: RequestContext<Arc<Context>>,
    _path: Path<Resource>,
    body: UntypedBody,
) -> Result<Response<Body>, HttpError> {
    let path = downloader_path(&r);
    proxy(r, path, body.as_bytes().to_vec()).await
}
#[endpoint {method=DELETE,path="/sabnzbd/{path:.*}",unpublished=true,tags=["downloader"]}]
async fn delete(
    r: RequestContext<Arc<Context>>,
    _path: Path<Resource>,
    body: UntypedBody,
) -> Result<Response<Body>, HttpError> {
    let path = downloader_path(&r);
    proxy(r, path, body.as_bytes().to_vec()).await
}
fn connection_headers(headers: &HeaderMap) -> HashSet<String> {
    headers
        .get_all("connection")
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|s| s.split(','))
        .map(|s| s.trim().to_ascii_lowercase())
        .collect()
}
fn hop(name: &HeaderName, extra: &HashSet<String>) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "proxy-connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    ) || extra.contains(name.as_str())
}
async fn proxy(
    r: RequestContext<Arc<Context>>,
    path: String,
    body: Vec<u8>,
) -> Result<Response<Body>, HttpError> {
    if path != "sabnzbd" && !path.starts_with("sabnzbd/") {
        return Err(denied(ClientErrorStatusCode::NOT_FOUND, "Page not found."));
    }
    let is_ingress = matches!(r.context().access, Access::Ingress { .. });
    if is_ingress {
        ingress(&r)?;
    } else if path.trim_end_matches('/') != "sabnzbd/api" {
        return Err(forbidden());
    }
    if r.context().status.borrow().downloader.state != DownloaderState::Running {
        return Err(HttpError::for_unavail(
            None,
            "Downloader is stopped while protection is unavailable.".into(),
        ));
    }
    if is_ingress && r.request.method() != Method::GET {
        if r.request
            .headers()
            .get("sec-fetch-site")
            .and_then(|h| h.to_str().ok())
            .is_some_and(|v| v != "same-origin")
        {
            return Err(forbidden());
        }
        if let Some(origin) = r
            .request
            .headers()
            .get("origin")
            .and_then(|h| h.to_str().ok())
        {
            let origin = reqwest::Url::parse(origin).map_err(|_| forbidden())?;
            let host = r
                .request
                .headers()
                .get("host")
                .and_then(|h| h.to_str().ok())
                .ok_or_else(forbidden)?;
            if origin[url::Position::BeforeHost..url::Position::AfterPort] != *host {
                return Err(forbidden());
            }
        }
    }
    let mut target = r.context().upstream.join(&path).map_err(|_| {
        denied(
            ClientErrorStatusCode::BAD_REQUEST,
            "Invalid downloader path.",
        )
    })?;
    target.set_query(r.request.uri().query());
    let skip = connection_headers(r.request.headers());
    let mut request = r
        .context()
        .client
        .request(r.request.method().clone(), target);
    for (name, value) in r.request.headers() {
        if !hop(name, &skip)
            && !matches!(
                name.as_str(),
                "host"
                    | "content-length"
                    | "accept-encoding"
                    | "x-ingress-path"
                    | "x-remote-user-id"
                    | "x-remote-user-groups"
                    | "authorization"
            )
        {
            request = request.header(name, value);
        }
    }
    let mut response = request
        .body(body)
        .send()
        .await
        .map_err(|_| HttpError::for_unavail(None, "Downloader is unavailable.".into()))?;
    let code = response.status();
    let headers = response.headers().clone();
    if response
        .content_length()
        .is_some_and(|size| size > BODY_LIMIT as u64)
    {
        return Err(denied(
            ClientErrorStatusCode::PAYLOAD_TOO_LARGE,
            "Downloader response is too large.",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| HttpError::for_unavail(None, "Downloader response interrupted.".into()))?
    {
        if bytes.len().saturating_add(chunk.len()) > BODY_LIMIT {
            return Err(denied(
                ClientErrorStatusCode::PAYLOAD_TOO_LARGE,
                "Downloader response is too large.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let base = if is_ingress {
        prefix(&r)?
    } else {
        String::new()
    };
    if !base.is_empty() {
        let kind = headers
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        if kind.starts_with("text/html")
            || kind.starts_with("text/css")
            || kind.contains("javascript")
        {
            let source = String::from_utf8(bytes).map_err(|_| {
                HttpError::for_internal_error("Invalid downloader page encoding.".into())
            })?;
            bytes = rewrite_page(r.context(), &source, &base, kind).into_bytes();
        }
    }
    let mut output = Response::builder().status(code);
    let skip = connection_headers(&headers);
    for (name, value) in &headers {
        if hop(name, &skip)
            || matches!(
                name.as_str(),
                "content-length" | "content-encoding" | "content-security-policy"
            )
        {
            continue;
        }
        if !base.is_empty() && name == "location" {
            let location = value.to_str().map_err(|_| {
                HttpError::for_internal_error("Invalid downloader redirect.".into())
            })?;
            // CherryPy emits absolute redirects using its loopback upstream
            // address. Keep those and root-relative native routes in ingress.
            let target = if location.starts_with('/') {
                r.context().upstream.join(location).ok()
            } else {
                reqwest::Url::parse(location).ok()
            };
            if let Some(target) = target
                && target.origin() == r.context().upstream.origin()
                && (target.path() == "/sabnzbd" || target.path().starts_with("/sabnzbd/"))
            {
                output = output.header(
                    name,
                    format!("{base}{}", &target[url::Position::BeforePath..]),
                );
                continue;
            }
        }
        if !base.is_empty() && name == "set-cookie" {
            let cookie = value
                .to_str()
                .map_err(|_| HttpError::for_internal_error("Invalid downloader cookie.".into()))?;
            output = output.header(
                name,
                cookie.replace("Path=/sabnzbd", &format!("Path={base}/sabnzbd")),
            );
            continue;
        }
        output = output.header(name, value);
    }
    output
        .header("cache-control", "no-store")
        .header("x-content-type-options", "nosniff")
        .header("referrer-policy", "no-referrer")
        .body(Body::from(bytes))
        .map_err(|_| HttpError::for_internal_error("Downloader response unavailable.".into()))
}
fn rewrite_page(context: &Context, source: &str, base: &str, kind: &str) -> String {
    let rewritten = source.replace("/sabnzbd", &format!("{base}/sabnzbd"));
    if !kind.starts_with("text/html") {
        return rewritten;
    }
    // Navigation needs the ingress prefix; the editable native setting must
    // keep the upstream base path rather than saving a session-specific URL.
    context
        .input
        .replace_all(&rewritten, |caps: &Captures<'_>| {
            let Some(tag) = caps.get(0) else {
                return String::new();
            };
            context
                .value
                .replace_all(tag.as_str(), "value=\"/sabnzbd\"")
                .into_owned()
        })
        .into_owned()
}
pub fn description() -> anyhow::Result<ApiDescription<Arc<Context>>> {
    let mut api = ApiDescription::new();
    api.register(health)?;
    api.register(app_status)?;
    api.register(readiness)?;
    api.register(index)?;
    api.register(javascript)?;
    api.register(stylesheet)?;
    api.register(downloader_root)?;
    api.register(get)?;
    api.register(post)?;
    api.register(put)?;
    api.register(delete)?;
    Ok(api)
}
pub fn start(bind: SocketAddr, context: Arc<Context>) -> anyhow::Result<HttpServer<Arc<Context>>> {
    let config = ConfigDropshot {
        bind_address: bind,
        default_request_body_max_bytes: BODY_LIMIT,
        ..Default::default()
    };
    // Requests may contain the native downloader's API key. Never log URLs or
    // request bodies through the HTTP framework.
    let log = slog::Logger::root(slog::Discard, slog::o!());
    HttpServerStarter::new(&config, description()?, context, &log)
        .map(|s| s.start())
        .map_err(|_| anyhow::anyhow!("Cannot bind app listener."))
}
#[cfg(test)]
mod tests {
    use crate::{
        api::{Access, Context, rewrite_page},
        status::Status,
    };
    #[test]
    fn ingress_navigation_preserves_the_editable_upstream_setting() {
        let (_, rx) = tokio::sync::watch::channel(Status::needs_configuration("setup"));
        let context = Context::new(Access::Direct { token_hash: None }, rx).expect("context");
        let page = r#"<a href="/sabnzbd/">Open</a><input value="/sabnzbd" name="url_base"><script>var base="/sabnzbd";</script>"#;
        let value = rewrite_page(&context, page, "/api/hassio_ingress/test", "text/html");
        assert!(value.contains(r#"href="/api/hassio_ingress/test/sabnzbd/""#));
        assert!(value.contains(r#"value="/sabnzbd" name="url_base""#));
        assert!(value.contains(r#"var base="/api/hassio_ingress/test/sabnzbd""#));
    }
}

#[cfg(test)]
mod http_tests {
    use crate::{
        api::{self, Access, Context},
        status::{Health, HealthReason, State, Status},
    };
    use http::{Method, StatusCode};
    use serde::Deserialize;
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::Arc,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::watch,
    };

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client")
    }
    fn admin(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header("x-remote-user-id", "test-user")
            .header("x-remote-user-groups", "system-users,system-admin")
    }
    fn listener() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 0))
    }
    fn context(access: Access) -> Context {
        let (_, rx) = watch::channel(Status::needs_configuration("Configure the VPN."));
        Context::new(access, rx).expect("context")
    }
    #[tokio::test]
    async fn readiness_is_authenticated_and_never_accepts_a_ready_label_alone() {
        let (_, rx) = watch::channel(Status::ready("test", None, true, true));
        let server = api::start(
            listener(),
            Arc::new(
                Context::new(
                    Access::Direct {
                        token_hash: Context::token_hash("test-secret"),
                    },
                    rx,
                )
                .expect("context"),
            ),
        )
        .expect("server");
        let url = format!("http://{}/api/v1/health", server.local_addr());
        let client = client();
        assert_eq!(
            client
                .get(&url)
                .send()
                .await
                .expect("unauthenticated")
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let health: Health = client
            .get(url)
            .bearer_auth("test-secret")
            .send()
            .await
            .expect("health")
            .json()
            .await
            .expect("typed health");
        assert!(!health.ready);
        assert_eq!(health.state, State::Ready);
        assert!(health.reasons.contains(&HealthReason::ChecksMissing));
        assert!(
            health
                .reasons
                .contains(&HealthReason::DownloaderUnavailable)
        );
        server.close().await.expect("close");
    }
    #[tokio::test]
    async fn health_reports_terminal_failure_without_restarting_recoverable_states() {
        #[derive(Deserialize)]
        struct HealthState {
            state: State,
        }
        let (tx, rx) = watch::channel(Status::needs_configuration("Configure the VPN."));
        let server = api::start(
            listener(),
            Arc::new(Context::new(Access::Direct { token_hash: None }, rx).expect("context")),
        )
        .expect("server");
        let url = format!("http://{}/healthz", server.local_addr());
        let client = client();
        for (status, expected) in [
            (
                Status::needs_configuration("Configure the VPN."),
                StatusCode::OK,
            ),
            (Status::starting("test"), StatusCode::OK),
            (Status::ready("test", None, true, true), StatusCode::OK),
            (
                Status::degraded("test", "The tunnel is reconnecting.", true),
                StatusCode::OK,
            ),
            (Status::stopping("test"), StatusCode::OK),
            (
                Status::degraded("test", "The protection runtime stopped.", false),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (Status::ready("test", None, true, true), StatusCode::OK),
        ] {
            let state = status.state.clone();
            tx.send_replace(status);
            let response = client.get(&url).send().await.expect("health response");
            assert_eq!(response.status(), expected, "health for {state:?}");
            if expected == StatusCode::OK {
                let body: HealthState = response.json().await.expect("health state");
                assert_eq!(body.state, state);
            }
        }
        server.close().await.expect("close");
    }
    #[tokio::test]
    async fn real_listeners_enforce_ingress_peer_admin_and_direct_token() {
        let direct = api::start(
            listener(),
            Arc::new(context(Access::Direct {
                token_hash: Context::token_hash("test-secret"),
            })),
        )
        .expect("direct");
        let url = format!("http://{}", direct.local_addr());
        let client = client();
        for route in ["/api/v1/status", "/api/v1/health"] {
            let request = client.get(format!("{url}{route}")).bearer_auth("wrong");
            assert_eq!(
                request.send().await.expect("response").status(),
                StatusCode::UNAUTHORIZED
            );
        }
        for route in ["/", "/sabnzbd/"] {
            assert_eq!(
                admin(client.get(format!("{url}{route}")))
                    .send()
                    .await
                    .expect("response")
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
        let response = client
            .get(format!("{url}/api/v1/status"))
            .bearer_auth("test-secret")
            .send()
            .await
            .expect("status");
        assert_eq!(response.status(), StatusCode::OK);
        let state: Status = response.json().await.expect("typed status");
        assert_eq!(state.state, State::NeedsConfiguration);
        assert_eq!(
            client
                .get(format!("{url}/healthz"))
                .send()
                .await
                .expect("health")
                .status(),
            StatusCode::OK
        );
        direct.close().await.expect("close");

        let denied = api::start(
            listener(),
            Arc::new(context(Access::Ingress {
                peer: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
            })),
        )
        .expect("denied");
        assert_eq!(
            admin(client.get(format!("http://{}/", denied.local_addr())))
                .send()
                .await
                .expect("denied")
                .status(),
            StatusCode::FORBIDDEN
        );
        denied.close().await.expect("close");
        let ingress = api::start(
            listener(),
            Arc::new(context(Access::Ingress {
                peer: IpAddr::V4(Ipv4Addr::LOCALHOST),
            })),
        )
        .expect("ingress");
        let url = format!("http://{}", ingress.local_addr());
        assert_eq!(
            client
                .get(format!("{url}/"))
                .send()
                .await
                .expect("unauthenticated")
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(format!("{url}/"))
                .header("x-remote-user-id", "user")
                .header("x-remote-user-groups", "system-users")
                .send()
                .await
                .expect("nonadmin")
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            admin(client.get(format!("{url}/")))
                .header("x-ingress-path", "/api/hassio_ingress/test\"bad")
                .send()
                .await
                .expect("invalid prefix")
                .status(),
            StatusCode::BAD_REQUEST
        );
        let page = admin(client.get(format!("{url}/")))
            .header("x-ingress-path", "/api/hassio_ingress/test")
            .send()
            .await
            .expect("page");
        assert_eq!(page.status(), StatusCode::OK);
        assert!(page.headers().contains_key("content-security-policy"));
        assert!(
            page.text()
                .await
                .expect("html")
                .contains("<base href=\"/api/hassio_ingress/test/\">")
        );
        ingress.close().await.expect("close");
    }

    struct Captured {
        head: String,
        body: Vec<u8>,
    }
    async fn upstream(
        response: impl FnOnce(&reqwest::Url) -> Vec<u8>,
    ) -> (reqwest::Url, tokio::task::JoinHandle<Captured>) {
        let listener = TcpListener::bind(listener()).await.expect("upstream bind");
        let url = reqwest::Url::parse(&format!(
            "http://{}/",
            listener.local_addr().expect("address")
        ))
        .expect("url");
        let response = response(&url);
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut bytes = Vec::new();
            let mut chunk = [0; 4096];
            let split = loop {
                let size = socket.read(&mut chunk).await.expect("read");
                assert!(size > 0, "request ended before headers");
                bytes.extend_from_slice(&chunk[..size]);
                if let Some(pos) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let head = String::from_utf8(bytes[..split].to_vec()).expect("header text");
            let length = head
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().expect("length"))
                })
                .unwrap_or(0);
            while bytes.len() - split < length {
                let size = socket.read(&mut chunk).await.expect("body");
                assert!(size > 0);
                bytes.extend_from_slice(&chunk[..size]);
            }
            socket.write_all(&response).await.expect("reply");
            socket.shutdown().await.expect("shutdown");
            Captured {
                head,
                body: bytes[split..].to_vec(),
            }
        });
        (url, task)
    }
    async fn proxy_server(upstream: reqwest::Url) -> dropshot::HttpServer<Arc<Context>> {
        let (_, rx) = watch::channel(Status::ready("test", None, true, true));
        let mut context = Context::new(
            Access::Ingress {
                peer: IpAddr::V4(Ipv4Addr::LOCALHOST),
            },
            rx,
        )
        .expect("context");
        context.upstream = upstream;
        api::start(listener(), Arc::new(context)).expect("proxy server")
    }
    #[derive(Deserialize)]
    struct NativeSettings {
        url_base: String,
    }
    #[tokio::test]
    async fn proxy_preserves_native_api_query_body_and_json_without_forwarding_identity() {
        let json = b"{\"url_base\":\"/sabnzbd\"}";
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            json.len()
        );
        let (target, captured) = upstream(|_| [wire.as_bytes(), json].concat()).await;
        let server = proxy_server(target).await;
        let response = admin(client().post(format!(
            "http://{}/sabnzbd/api?apikey=native-key&mode=set_config",
            server.local_addr()
        )))
        .header("x-ingress-path", "/api/hassio_ingress/test")
        .header("authorization", "Bearer private-token")
        .header("cookie", "native=session")
        .header("connection", "x-remove")
        .header("x-remove", "private-hop")
        .header("content-type", "application/x-www-form-urlencoded")
        .body("name=category&dir=Movies%2F")
        .send()
        .await
        .expect("proxy response");
        assert_eq!(response.status(), StatusCode::OK);
        let config: NativeSettings = response.json().await.expect("native JSON");
        assert_eq!(config.url_base, "/sabnzbd");
        let captured = captured.await.expect("upstream");
        let lower = captured.head.to_ascii_lowercase();
        assert!(
            captured
                .head
                .starts_with("POST /sabnzbd/api?apikey=native-key&mode=set_config HTTP/1.1")
        );
        assert_eq!(captured.body, b"name=category&dir=Movies%2F");
        assert!(lower.contains("cookie: native=session"));
        for name in [
            "authorization:",
            "x-remote-user-id:",
            "x-remote-user-groups:",
            "x-ingress-path:",
            "x-remove:",
        ] {
            assert!(!lower.contains(name), "leaked {name}");
        }
        server.close().await.expect("close");
    }
    #[tokio::test]
    async fn proxy_rewrites_redirect_and_each_cookie_path() {
        let (target,captured)=upstream(|_| b"HTTP/1.1 303 See Other\r\nLocation: /sabnzbd/login/\r\nSet-Cookie: login=one; Path=/sabnzbd; HttpOnly\r\nSet-Cookie: csrf=two; Path=/sabnzbd/; SameSite=Strict\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()).await;
        let server = proxy_server(target).await;
        let response = admin(client().get(format!("http://{}/sabnzbd/", server.local_addr())))
            .header("x-ingress-path", "/api/hassio_ingress/test")
            .send()
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()["location"],
            "/api/hassio_ingress/test/sabnzbd/login/"
        );
        let cookies = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|h| h.to_str().expect("cookie"))
            .collect::<Vec<_>>();
        assert_eq!(cookies.len(), 2);
        assert!(
            cookies
                .iter()
                .all(|c| c.contains("Path=/api/hassio_ingress/test/sabnzbd"))
        );
        captured.await.expect("capture");
        server.close().await.expect("close");
    }
    #[tokio::test]
    async fn native_absolute_redirect_stays_inside_ingress() {
        let (target,captured)=upstream(|url|format!("HTTP/1.1 303 See Other\r\nLocation: {url}sabnzbd/login/?next=%2Fsabnzbd%2F\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()).await;
        let server = proxy_server(target).await;
        let response = admin(client().get(format!("http://{}/sabnzbd/", server.local_addr())))
            .header("x-ingress-path", "/api/hassio_ingress/test")
            .send()
            .await
            .expect("redirect");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()["location"],
            "/api/hassio_ingress/test/sabnzbd/login/?next=%2Fsabnzbd%2F"
        );
        captured.await.expect("capture");
        server.close().await.expect("close");
    }
    #[tokio::test]
    async fn unrelated_redirects_keep_their_destination() {
        for location in [
            "sabnzbd/login/",
            "https://downloads.example/sabnzbd/login/",
            "//downloads.example/sabnzbd/login/",
            "/other/login/",
        ] {
            let (target,captured)=upstream(|_|format!("HTTP/1.1 303 See Other\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()).await;
            let server = proxy_server(target).await;
            let response =
                admin(client().get(format!("http://{}/sabnzbd/config/", server.local_addr())))
                    .header("x-ingress-path", "/api/hassio_ingress/test")
                    .send()
                    .await
                    .expect("redirect");
            assert_eq!(response.headers()["location"], location);
            captured.await.expect("capture");
            server.close().await.expect("close");
        }
    }
    #[tokio::test]
    async fn proxy_keeps_directory_slashes_for_get_and_post() {
        for method in [Method::GET, Method::POST] {
            let (target, captured) = upstream(|_| {
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            })
            .await;
            let server = proxy_server(target).await;
            let response = admin(client().request(
                method.clone(),
                format!(
                    "http://{}/sabnzbd/config/general/?skip_wizard=1",
                    server.local_addr()
                ),
            ))
            .header("x-ingress-path", "/api/hassio_ingress/test")
            .send()
            .await
            .expect("directory response");
            assert_eq!(response.status(), StatusCode::OK);
            let request = captured.await.expect("capture");
            assert!(
                request.head.starts_with(&format!(
                    "{method} /sabnzbd/config/general/?skip_wizard=1 HTTP/1.1"
                )),
                "{}",
                request.head
            );
            server.close().await.expect("close");
        }
    }
    #[tokio::test]
    async fn cross_origin_mutation_is_rejected_before_contacting_downloader() {
        let (_, rx) = watch::channel(Status::ready("test", None, true, true));
        let server = api::start(
            listener(),
            Arc::new(
                Context::new(
                    Access::Ingress {
                        peer: IpAddr::V4(Ipv4Addr::LOCALHOST),
                    },
                    rx,
                )
                .expect("context"),
            ),
        )
        .expect("server");
        let response = admin(client().post(format!("http://{}/sabnzbd/api", server.local_addr())))
            .header("origin", "https://untrusted.example")
            .header("sec-fetch-site", "cross-site")
            .body("mode=resume")
            .send()
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        server.close().await.expect("close");
    }
}
