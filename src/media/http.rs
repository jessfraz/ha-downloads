//! HTTP is a thin authenticated boundary over the same domain service used by HA.
use crate::{
    api::{Access, Context},
    media::{model::*, service::MediaService},
};
use dropshot::{
    ApiDescription, Body, ErrorStatusCode, HttpError, HttpResponseAccepted, HttpResponseHeaders,
    HttpResponseOk, Path, Query, RequestContext, TypedBody, endpoint,
};
use http::Response;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;

pub(crate) struct Principal {
    pub actor: String,
    pub role: CredentialRole,
    pub capabilities: Capabilities,
    pub transport: Transport,
}
fn bearer_matches(actual: &[u8; 32], expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let expected: [u8; 32] = Sha256::digest(expected.as_bytes()).into();
    bool::from(actual.ct_eq(&expected))
}
pub(crate) fn authorize(
    r: &RequestContext<Arc<Context>>,
    request: bool,
    admin: bool,
    handshake: bool,
) -> Result<Principal, HttpError> {
    let context = r.context();
    let service = context
        .media
        .as_ref()
        .ok_or_else(|| error(ApiErrorCode::NotConfigured))?;
    let headers = r.request.headers();
    if r.request.method() != http::Method::GET && !service.accepting() {
        return Err(error(ApiErrorCode::UpstreamUnavailable));
    }
    let principal = match &context.access {
        Access::Ingress { peer } => {
            if r.request.remote_addr().ip() != *peer {
                return Err(error(ApiErrorCode::PermissionDenied));
            }
            let user = headers
                .get("x-remote-user-id")
                .and_then(|v| v.to_str().ok())
                .filter(|s| valid_actor(s))
                .ok_or_else(|| error(ApiErrorCode::PermissionDenied))?;
            let is_admin = headers
                .get("x-remote-user-groups")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|groups| groups.split(',').any(|g| g.trim() == "system-admin"));
            if r.request.method() != http::Method::GET {
                if headers
                    .get("sec-fetch-site")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|s| s != "same-origin")
                {
                    return Err(error(ApiErrorCode::PermissionDenied));
                }
                if let Some(origin) = headers.get("origin") {
                    let origin = origin
                        .to_str()
                        .ok()
                        .and_then(|s| reqwest::Url::parse(s).ok())
                        .ok_or_else(|| error(ApiErrorCode::PermissionDenied))?;
                    let host = headers
                        .get("host")
                        .and_then(|v| v.to_str().ok())
                        .ok_or_else(|| error(ApiErrorCode::PermissionDenied))?;
                    if origin[url::Position::BeforeHost..url::Position::AfterPort] != *host {
                        return Err(error(ApiErrorCode::PermissionDenied));
                    }
                }
            }
            let can_request = is_admin
                || service
                    .config
                    .requester_user_ids
                    .iter()
                    .any(|id| id == user);
            Principal {
                actor: format!("ha:{user}"),
                role: if is_admin {
                    CredentialRole::Administrator
                } else if can_request {
                    CredentialRole::Requester
                } else {
                    CredentialRole::ReadOnly
                },
                capabilities: Capabilities {
                    read: true,
                    request: can_request,
                    admin: is_admin,
                },
                transport: Transport::Ingress,
            }
        }
        Access::Direct { token_hash } => {
            let token = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .ok_or_else(|| error(ApiErrorCode::AuthenticationRequired))?;
            let actual: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            let role = if token_hash
                .as_ref()
                .is_some_and(|expected| bool::from(actual.ct_eq(expected)))
            {
                CredentialRole::ReadOnly
            } else if bearer_matches(&actual, &service.config.request_token) {
                CredentialRole::Requester
            } else if bearer_matches(&actual, &service.config.admin_token) {
                CredentialRole::Administrator
            } else if bearer_matches(&actual, &service.config.integration_token) {
                CredentialRole::Integration
            } else {
                return Err(error(ApiErrorCode::AuthenticationRequired));
            };
            let actor_header = headers.get("x-downloads-actor-id");
            let request_header = headers.get("x-downloads-actor-can-request");
            if role == CredentialRole::Integration {
                let delegated = match (actor_header, request_header) {
                    (Some(id), Some(permission)) => {
                        let id = id
                            .to_str()
                            .ok()
                            .filter(|id| valid_actor(id))
                            .ok_or_else(|| error(ApiErrorCode::PermissionDenied))?;
                        let can_request = match permission.to_str().ok() {
                            Some("true") => true,
                            Some("false") => false,
                            _ => return Err(error(ApiErrorCode::PermissionDenied)),
                        };
                        Some((format!("ha:{id}"), can_request))
                    }
                    (None, None) if handshake => None,
                    _ => return Err(error(ApiErrorCode::PermissionDenied)),
                };
                Principal {
                    actor: delegated
                        .as_ref()
                        .map_or_else(|| "integration:handshake".into(), |d| d.0.clone()),
                    role,
                    capabilities: Capabilities {
                        read: true,
                        request: delegated.as_ref().is_some_and(|d| d.1),
                        admin: false,
                    },
                    transport: if delegated.is_some() {
                        Transport::HomeAssistant
                    } else {
                        Transport::Native
                    },
                }
            } else {
                if actor_header.is_some() || request_header.is_some() {
                    return Err(error(ApiErrorCode::PermissionDenied));
                }
                Principal {
                    actor: format!(
                        "native:{}",
                        actual
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>()
                    ),
                    role,
                    capabilities: Capabilities {
                        read: true,
                        request: matches!(
                            role,
                            CredentialRole::Requester | CredentialRole::Administrator
                        ),
                        admin: role == CredentialRole::Administrator,
                    },
                    transport: Transport::Native,
                }
            }
        }
    };
    if (request && !principal.capabilities.request) || (admin && !principal.capabilities.admin) {
        return Err(error(ApiErrorCode::PermissionDenied));
    }
    Ok(principal)
}
fn valid_actor(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn service(r: &RequestContext<Arc<Context>>) -> Result<&Arc<MediaService>, HttpError> {
    r.context()
        .media
        .as_ref()
        .ok_or_else(|| error(ApiErrorCode::NotConfigured))
}
pub(crate) fn error(code: ApiErrorCode) -> HttpError {
    let status_code = match code {
        ApiErrorCode::AuthenticationRequired => ErrorStatusCode::UNAUTHORIZED,
        ApiErrorCode::PermissionDenied => ErrorStatusCode::FORBIDDEN,
        ApiErrorCode::NotFound => ErrorStatusCode::NOT_FOUND,
        ApiErrorCode::NotConfigured | ApiErrorCode::UpstreamUnavailable => {
            ErrorStatusCode::SERVICE_UNAVAILABLE
        }
        ApiErrorCode::InvalidUpstreamResponse => ErrorStatusCode::BAD_GATEWAY,
        ApiErrorCode::NativeSettingsRequired
        | ApiErrorCode::ExistingSettingsConflict
        | ApiErrorCode::IdempotencyConflict => ErrorStatusCode::CONFLICT,
        ApiErrorCode::PreconditionRequired => ErrorStatusCode::PRECONDITION_REQUIRED,
        ApiErrorCode::InvalidScope => ErrorStatusCode::UNPROCESSABLE_ENTITY,
        _ => ErrorStatusCode::BAD_REQUEST,
    };
    let message = match code {
        ApiErrorCode::AuthenticationRequired => "A valid API bearer token is required.",
        ApiErrorCode::PermissionDenied => "This credential or user cannot perform this operation.",
        ApiErrorCode::NotFound => "The requested item was not found.",
        ApiErrorCode::NotConfigured => "Configure the required media connection first.",
        ApiErrorCode::UpstreamUnavailable => {
            "The required service or storage is unavailable. An uncertain write must be reconciled, not repeated with a new key."
        }
        ApiErrorCode::InvalidUpstreamResponse => {
            "A service returned an invalid or oversized response."
        }
        ApiErrorCode::NativeSettingsRequired => {
            "Add or configure this title in Radarr or Sonarr before requesting it here."
        }
        ApiErrorCode::ExistingSettingsConflict => {
            "The existing title has different settings. Nothing was overwritten."
        }
        ApiErrorCode::IdempotencyConflict => "This idempotency key belongs to different input.",
        ApiErrorCode::InvalidScope => {
            "The requested episode scope is invalid or has no eligible episodes."
        }
        ApiErrorCode::PreconditionRequired => "Supply the required caller-owned idempotency key.",
        ApiErrorCode::UnsupportedTransportOperation => {
            "This operation is not supported by the selected transport."
        }
        ApiErrorCode::InvalidArguments => "Use the declared argument names, values, and types.",
    };
    HttpError {
        status_code,
        error_code: Some(code.to_string()),
        external_message: message.into(),
        internal_message: message.into(),
        headers: None,
    }
}
#[derive(Serialize, JsonSchema)]
struct CacheHeaders {
    #[serde(rename = "cache-control")]
    cache_control: String,
}
type ApiOk<T> = HttpResponseHeaders<HttpResponseOk<T>, CacheHeaders>;
fn ok<T: Serialize + JsonSchema + Send + Sync + 'static>(body: T) -> ApiOk<T> {
    HttpResponseHeaders::new(
        HttpResponseOk(body),
        CacheHeaders {
            cache_control: "no-store".into(),
        },
    )
}
#[derive(Serialize, JsonSchema)]
struct RequestHeaders {
    #[serde(rename = "cache-control")]
    cache_control: String,
    #[serde(rename = "location")]
    location: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Paging {
    limit: Option<u32>,
    page_token: Option<String>,
}
impl Paging {
    fn validate(&self) -> Result<(), HttpError> {
        if self.limit.is_some_and(|n| !(1..=100).contains(&n))
            || self.page_token.as_ref().is_some_and(|s| s.len() > 300)
        {
            Err(error(ApiErrorCode::InvalidArguments))
        } else {
            Ok(())
        }
    }
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchParams {
    query: SearchQuery,
    kind: Option<SearchKind>,
    limit: Option<u32>,
    page_token: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HealthParams {
    operation: Option<Operation>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EpisodeParams {
    season: Option<u16>,
    limit: Option<u32>,
    page_token: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
struct RequestPath {
    request_id: RequestId,
}
#[derive(Deserialize, JsonSchema)]
struct KeyPath {
    key: String,
}
#[derive(Deserialize, JsonSchema)]
struct MoviePath {
    tmdb_id: TmdbId,
}
#[derive(Deserialize, JsonSchema)]
struct SeriesPath {
    tvdb_id: TvdbId,
}
#[derive(Deserialize, JsonSchema)]
struct ComponentPath {
    component: Component,
}
#[derive(Deserialize, JsonSchema)]
struct ArtworkPath {
    kind: MediaKind,
    media_id: u32,
    artwork: ArtworkKind,
}
fn idempotency(r: &RequestContext<Arc<Context>>) -> Result<String, HttpError> {
    let key = r
        .request
        .headers()
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| error(ApiErrorCode::PreconditionRequired))?;
    validate_key(key)?;
    Ok(key.into())
}
fn validate_key(key: &str) -> Result<(), HttpError> {
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        Err(error(ApiErrorCode::InvalidArguments))
    } else {
        Ok(())
    }
}
#[endpoint{method=GET,path="/api/v1/system",tags=["media"]}]
async fn endpoint_system(
    r: RequestContext<Arc<Context>>,
) -> Result<ApiOk<SystemResponse>, HttpError> {
    let p = authorize(&r, false, false, true)?;
    Ok(ok(SystemResponse {
        version: env!("CARGO_PKG_VERSION").into(),
        capabilities: p.capabilities,
        transport: p.transport,
        credential_role: p.role,
    }))
}
#[endpoint{method=GET,path="/api/v1/health/stack",tags=["media"]}]
async fn endpoint_health(
    r: RequestContext<Arc<Context>>,
    q: Query<HealthParams>,
) -> Result<ApiOk<HealthSnapshot>, HttpError> {
    authorize(&r, false, false, false)?;
    let q = q.into_inner();
    Ok(ok(service(&r)?
        .health_snapshot(q.operation)
        .await
        .map_err(error)?))
}
#[endpoint{method=GET,path="/api/v1/health/components/{component}",tags=["media"]}]
async fn endpoint_component_health(
    r: RequestContext<Arc<Context>>,
    p: Path<ComponentPath>,
) -> Result<ApiOk<ComponentHealth>, HttpError> {
    authorize(&r, false, false, false)?;
    let component = p.into_inner().component;
    let snapshot = service(&r)?.health_snapshot(None).await.map_err(error)?;
    Ok(ok(snapshot
        .components
        .into_iter()
        .find(|c| c.component == component)
        .ok_or_else(|| error(ApiErrorCode::NotFound))?))
}
#[endpoint{method=GET,path="/api/v1/managers",tags=["media"]}]
async fn endpoint_managers(
    r: RequestContext<Arc<Context>>,
) -> Result<ApiOk<ManagerList>, HttpError> {
    authorize(&r, false, false, false)?;
    Ok(ok(service(&r)?.managers().await))
}
#[endpoint{method=GET,path="/api/v1/search",tags=["media"]}]
async fn endpoint_search(
    r: RequestContext<Arc<Context>>,
    q: Query<SearchParams>,
) -> Result<ApiOk<Page<MediaItem>>, HttpError> {
    authorize(&r, false, false, false)?;
    let q = q.into_inner();
    Paging {
        limit: q.limit,
        page_token: q.page_token.clone(),
    }
    .validate()?;
    Ok(ok(service(&r)?
        .search(
            q.query,
            q.kind.unwrap_or(SearchKind::All),
            q.limit,
            q.page_token.as_deref(),
        )
        .await
        .map_err(error)?))
}
#[endpoint{method=GET,path="/api/v1/movies",tags=["media"]}]
async fn endpoint_movies(
    r: RequestContext<Arc<Context>>,
    q: Query<Paging>,
) -> Result<ApiOk<Page<MediaItem>>, HttpError> {
    authorize(&r, false, false, false)?;
    let q = q.into_inner();
    q.validate()?;
    Ok(ok(service(&r)?
        .movies(q.limit, q.page_token.as_deref())
        .await
        .map_err(error)?))
}
#[endpoint{method=GET,path="/api/v1/movies/{tmdb_id}",tags=["media"]}]
async fn endpoint_movie(
    r: RequestContext<Arc<Context>>,
    p: Path<MoviePath>,
) -> Result<ApiOk<MediaItem>, HttpError> {
    authorize(&r, false, false, false)?;
    Ok(ok(service(&r)?
        .movie(p.into_inner().tmdb_id)
        .await
        .map_err(error)?))
}
#[endpoint{method=GET,path="/api/v1/series",tags=["media"]}]
async fn endpoint_series_list(
    r: RequestContext<Arc<Context>>,
    q: Query<Paging>,
) -> Result<ApiOk<Page<MediaItem>>, HttpError> {
    authorize(&r, false, false, false)?;
    let q = q.into_inner();
    q.validate()?;
    Ok(ok(service(&r)?
        .series_list(q.limit, q.page_token.as_deref())
        .await
        .map_err(error)?))
}
#[endpoint{method=GET,path="/api/v1/series/{tvdb_id}",tags=["media"]}]
async fn endpoint_series(
    r: RequestContext<Arc<Context>>,
    p: Path<SeriesPath>,
) -> Result<ApiOk<MediaItem>, HttpError> {
    authorize(&r, false, false, false)?;
    Ok(ok(service(&r)?
        .series(p.into_inner().tvdb_id)
        .await
        .map_err(error)?))
}
#[endpoint{method=GET,path="/api/v1/series/{tvdb_id}/episodes",tags=["media"]}]
async fn endpoint_episodes(
    r: RequestContext<Arc<Context>>,
    p: Path<SeriesPath>,
    q: Query<EpisodeParams>,
) -> Result<ApiOk<Page<Episode>>, HttpError> {
    authorize(&r, false, false, false)?;
    let q = q.into_inner();
    Paging {
        limit: q.limit,
        page_token: q.page_token.clone(),
    }
    .validate()?;
    Ok(ok(service(&r)?
        .episodes(
            p.into_inner().tvdb_id,
            q.season,
            q.limit,
            q.page_token.as_deref(),
        )
        .await
        .map_err(error)?))
}
#[endpoint{method=GET,path="/api/v1/queue",tags=["media"]}]
async fn endpoint_queue(
    r: RequestContext<Arc<Context>>,
    q: Query<Paging>,
) -> Result<ApiOk<Page<QueueItem>>, HttpError> {
    authorize(&r, false, false, false)?;
    let q = q.into_inner();
    q.validate()?;
    Ok(ok(service(&r)?
        .queue(q.limit, q.page_token.as_deref())
        .await
        .map_err(error)?))
}
#[derive(Deserialize, JsonSchema)]
struct QueuePath {
    id: QueueId,
}
#[endpoint{method=GET,path="/api/v1/imports",tags=["media"]}]
async fn endpoint_imports(
    r: RequestContext<Arc<Context>>,
    q: Query<ImportParams>,
) -> Result<ApiOk<Page<ImportedItem>>, HttpError> {
    authorize(&r, false, false, false)?;
    let q = q.into_inner();
    Paging {
        limit: q.limit,
        page_token: q.page_token.clone(),
    }
    .validate()?;
    Ok(ok(service(&r)?
        .imports(q.limit, q.page_token.as_deref(), q.since.as_deref())
        .await
        .map_err(error)?))
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImportParams {
    limit: Option<u32>,
    page_token: Option<String>,
    since: Option<String>,
}
#[endpoint{method=GET,path="/api/v1/queue/{id}",tags=["media"]}]
async fn endpoint_download_details(
    r: RequestContext<Arc<Context>>,
    p: Path<QueuePath>,
) -> Result<ApiOk<DownloadDetails>, HttpError> {
    authorize(&r, false, false, false)?;
    Ok(ok(service(&r)?
        .download_details(p.into_inner().id)
        .await
        .map_err(error)?))
}
#[endpoint{method=GET,path="/api/v1/requests",tags=["media"]}]
async fn endpoint_requests(
    r: RequestContext<Arc<Context>>,
    q: Query<Paging>,
) -> Result<ApiOk<Page<RequestRecord>>, HttpError> {
    let p = authorize(&r, false, false, false)?;
    let q = q.into_inner();
    q.validate()?;
    Ok(ok(service(&r)?
        .requests(
            &p.actor,
            p.capabilities.admin,
            q.limit,
            q.page_token.as_deref(),
        )
        .await
        .map_err(error)?))
}
#[endpoint{method=GET,path="/api/v1/requests/{request_id}",tags=["media"]}]
async fn endpoint_request(
    r: RequestContext<Arc<Context>>,
    id: Path<RequestPath>,
) -> Result<ApiOk<RequestRecord>, HttpError> {
    let p = authorize(&r, false, false, false)?;
    Ok(ok(service(&r)?
        .request(&p.actor, p.capabilities.admin, &id.into_inner().request_id)
        .await
        .map_err(error)?))
}
#[endpoint{method=GET,path="/api/v1/request-keys/{key}",tags=["media"]}]
async fn endpoint_request_by_key(
    r: RequestContext<Arc<Context>>,
    id: Path<KeyPath>,
) -> Result<ApiOk<RequestRecord>, HttpError> {
    let p = authorize(&r, false, false, false)?;
    let key = id.into_inner().key;
    validate_key(&key)?;
    Ok(ok(service(&r)?
        .request_by_key(&p.actor, &key)
        .await
        .map_err(error)?))
}
#[endpoint{method=POST,path="/api/v1/requests",tags=["media"],request_body_max_bytes=1048576}]
async fn endpoint_create_request(
    r: RequestContext<Arc<Context>>,
    body: TypedBody<CreateRequest>,
) -> Result<HttpResponseHeaders<HttpResponseAccepted<RequestRecord>, RequestHeaders>, HttpError> {
    let p = authorize(&r, true, false, false)?;
    let record = service(&r)?
        .create_request(&p.actor, &idempotency(&r)?, body.into_inner())
        .await
        .map_err(error)?;
    let prefix = if matches!(p.transport, Transport::Ingress) {
        crate::api::prefix(&r)?
    } else {
        String::new()
    };
    let location = format!("{prefix}/api/v1/requests/{}", record.id);
    Ok(HttpResponseHeaders::new(
        HttpResponseAccepted(record),
        RequestHeaders {
            cache_control: "no-store".into(),
            location,
        },
    ))
}
#[endpoint{method=GET,path="/api/v1/artwork/{kind}/{media_id}/{artwork}",tags=["media"]}]
async fn endpoint_artwork(
    r: RequestContext<Arc<Context>>,
    p: Path<ArtworkPath>,
) -> Result<Response<Body>, HttpError> {
    authorize(&r, false, false, false)?;
    let p = p.into_inner();
    let (kind, bytes) = service(&r)?
        .artwork(p.kind, p.media_id, p.artwork)
        .await
        .map_err(error)?;
    Response::builder()
        .header("content-type", kind)
        .header("cache-control", "private, max-age=300")
        .header("x-content-type-options", "nosniff")
        .body(Body::from(bytes))
        .map_err(|_| error(ApiErrorCode::InvalidUpstreamResponse))
}

pub fn register(api: &mut ApiDescription<Arc<Context>>) -> anyhow::Result<()> {
    api.register(endpoint_system)?;
    api.register(endpoint_health)?;
    api.register(endpoint_component_health)?;
    api.register(endpoint_managers)?;
    api.register(endpoint_search)?;
    api.register(endpoint_movies)?;
    api.register(endpoint_movie)?;
    api.register(endpoint_series_list)?;
    api.register(endpoint_series)?;
    api.register(endpoint_episodes)?;
    api.register(endpoint_queue)?;
    api.register(endpoint_download_details)?;
    api.register(endpoint_imports)?;
    api.register(endpoint_requests)?;
    api.register(endpoint_request_by_key)?;
    api.register(endpoint_request)?;
    api.register(endpoint_create_request)?;
    api.register(endpoint_artwork)?;
    Ok(())
}
