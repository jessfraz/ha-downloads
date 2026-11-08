# Downloads media contract

The Rust models in `src/media/model.rs` own these JSON shapes. All paths below
are relative to `/api/v1`; HA transport exposes the read/request subset at
`/api/downloads/v1`. Unknown write fields and unknown enum values are rejected.
No automatic retry, redirect, implicit pagination, or credential fallback.

## Existing endpoints (preserved)

| Method | Absolute path | Contract |
| --- | --- | --- |
| GET | `/healthz` | Local watchdog with a safe functional-health body |
| GET | `/api/v1/status` | Authenticated VPN/downloader/protection status |
| GET | `/api/v1/health` | Existing functional readiness summary |
| GET | `/`, `/app.js`, `/style.css` | Authenticated sidebar UI/assets |
| GET | `/sabnzbd` | SABnzbd browser redirect |
| GET, POST, PUT, DELETE | `/sabnzbd/{path…}` | Bounded native SABnzbd proxy; ingress administrator or native SAB API-key policy |

The media API adds resources; it does not replace the watchdog, status schema
or native SABnzbd API. No arbitrary manager passthrough is exposed.

## Identity and authorization

`api_token` remains read-only. New `media.request_token`, `media.admin_token`,
and `media.integration_token` are independent masked secrets. The integration
token may access read/request routes only, never profiles writes, manager
credentials, or the SAB proxy. Native requester/admin tokens need no actor
headers. Admin ingress remains admin; optional non-admin ingress requesting
requires its user ID in `media.requester_user_ids`.

The native HA app generates a stable private integration credential when its
option is blank, following the other HA apps' Supervisor pairing pattern.
Standalone deployments require an explicit integration token. Generated app
state is owner-only and covered by native backups; options are never rewritten.

The HA integration alone supplies `X-Downloads-Actor-Id` and
`X-Downloads-Actor-Can-Request: true|false` with its dedicated integration
credential, after resolving the active HA user and checking entity permission
and Assist exposure. The backend ignores/rejects actor headers for other
credentials. Integration credentials cannot assert administrator authority.
The backend binds idempotency to this authenticated actor, not a body field.

## Read routes

- `GET /system`: `{version, capabilities:{read,request,admin}, transport,
  credential_role}`. Role is `read_only|requester|administrator|integration`;
  pairing accepts only `integration`.
- `GET /health/stack`: `HealthSnapshot {components, readiness, generated_at_unix_ms}`.
  Optional `operation` and `profile_id` select target-scoped readiness.
- `GET /health/components/{component}`: one `ComponentHealth`.
- `GET /managers`: `{items: ManagerSummary[]}`.
- `GET /managers/{manager}/options`: `{manager, quality_profiles, root_folders,
  tags, series_types}`. Options are `{id,name}`, root folders `{id,name,
  accessible}`. Names are safe labels, not filesystem paths.
- `GET /profiles?kind=movie|series`: `{items: DownloadProfile[]}`.
- `GET /profiles/{profile_id}`: profile plus ETag `"REVISION"`.
- `GET /search?query=TEXT&kind=all|movie|series&limit=20&page_token=TOKEN`:
  `Page<MediaItem>`.
- `GET /movies?limit=20&page_token=TOKEN`: `Page<MediaItem>` (movie only).
- `GET /movies/{tmdb_id}`: `MediaItem` (movie).
- `GET /series?limit=20&page_token=TOKEN`: `Page<MediaItem>` (series only).
- `GET /series/{tvdb_id}`: `MediaItem` (series with seasons).
- `GET /series/{tvdb_id}/episodes?season=2&limit=20&page_token=TOKEN`:
  `Page<Episode>`.
- `GET /queue?limit=20&page_token=TOKEN`: `Page<QueueItem>`.
- `GET /requests?limit=20&page_token=TOKEN`: `Page<RequestRecord>`.
- `GET /requests/{request_id}`: `RequestRecord`, scoped to the actor unless
  administrator.
- `GET /request-keys/{idempotency_key}`: actor-scoped original receipt, or
  404 if no receipt is committed yet. An in-flight submission can still accept
  later. Retain its original key and input; never infer permission for a new
  key from this lookup. Never creates or retries work.
- `GET /artwork/{kind}/{media_id}/{artwork}`: image bytes; `kind=movie|series`,
  `artwork=poster|backdrop`. Authenticated, bounded, server-resolved origin only.

`Page<T>` is `{items:T[],next_page:string|null,coverage:SourceCoverage[]}`.
Coverage is `{source:Component,state:"available"|"unavailable"|"unsupported"|"partial",
observed_at_unix_ms:number}`. An unavailable source is not an empty result.
Page limits are 1..100. Tokens bind the route and query/filter scope.

`ManagerId` is `radarr|sonarr` in this version (one connection per kind).
`Component` is `radarr|sonarr|prowlarr|sabnzbd|vpn|firewall|dns|storage|plex`.

`MediaItem` is an internally tagged `kind` union with common fields `title`,
`year` (number|null), `overview`, `poster` (relative API path|null),
`default_profile` (`{id,revision}`|null), `library`, and `acquisition`:

- movie: `kind:"movie",tmdb_id:number`.
- series: `kind:"series",tvdb_id:number,seasons:Season[]`.

Library is `{state:"not_added"}` or `{state:"managed",upstream_id:number,
quality_profile_id:number,quality_profile_name:string}`. Existing settings are
preserved. `Season` is `{number,episode_count,imported_count,active_count,
monitored,state}`; counts can be null when upstream lookup cannot prove them.
Season state is `unknown|missing|partial|in_progress|imported`.
Acquisition is tagged by `state`: `unknown`, `not_requested`, `waiting`,
`downloading` (with `completed_bytes,total_bytes`), `importing`, `imported`,
`blocked` (with typed `reason`), or `unavailable` (with `component`).
`Episode` is `{id,season,number,title,air_date,monitored,acquisition}`.
`QueueItem` is `{id,media:MediaIdentity,title,episode:null|{season,number},
acquisition}`. `MediaIdentity` is `{kind:"movie",tmdb_id}` or
`{kind:"series",tvdb_id}`.

## Profiles and requesting

`POST /profiles` requires native/ingress administrator and Idempotency-Key.
`PUT /profiles/{id}` requires administrator and exact If-Match. Profiles are a
tagged `kind` union: `{kind,id,revision,name,is_default,settings}`. Create and
replace inputs omit `id,revision`.

Movie settings: `{quality_profile_id,root_folder_id,tags,minimum_availability}`
where availability is `announced|in_cinemas|released`.
Series settings: `{quality_profile_id,root_folder_id,tags,series_type,
season_folders,future_monitoring}` where series_type is `standard|daily|anime`
and future_monitoring is `off|monitor`.

`POST /requests` requires request capability and caller Idempotency-Key.
The JSON is one of:

```json
{"kind":"movie","tmdb_id":14337,"profile":{"id":"UUID","revision":1},"intent":{"mode":"search_now"}}
```

```json
{"kind":"series","tvdb_id":123456,"profile":{"id":"UUID","revision":1},"intent":{"mode":"search_now","scope":{"kind":"seasons","numbers":[2]}}}
```

Movie intent: `{mode:"search_now"}` or `{mode:"monitor_only"}`.
Series intent: `{mode:"search_now",scope:{kind:"all_aired"}}`,
`{mode:"search_now",scope:{kind:"seasons",numbers:[2]}}`, or
`{mode:"monitor_only"}`. Nonempty unique seasons, canonical positive IDs,
validated UUIDs, and positive revisions are enforced by Rust types.

An existing title's effective quality must agree with the selected profile;
otherwise return `existing_settings_conflict`, never silently replace it.
Existing unrelated season monitoring remains unchanged. New series monitor
only selected aired, missing episodes, plus future monitoring only if the saved
profile opts in. Monitor-only with future monitoring off is invalid for series.

Acceptance returns 202 and Location. Before acceptance, validate manager
relationships and persist an immutable plan, actor binding, input and key.
Replay same key/body returns the original request before current-profile
validation. Different body returns `idempotency_conflict`. Lost upstream
responses reconcile without blindly replaying effects. A manager outage before
validation returns unavailable; accepted work waits/reconciles after outages.

`RequestRecord` is `{id,created_at_unix_ms,input:CreateRequest,
submission:Submission}`. Submission is tagged by `state`: `pending`, `applying`,
`applied` with `{result:{effect,media,manager,upstream_id,search}}`,
`reconciling` with `{reason}`, or `failed` with `{error:RequestFailure}`.
Effects: `added|already_managed|already_satisfied`. Search: tagged `state`
`not_requested|queued` (queued carries `command_id`). Applied does not mean
downloaded. Request failures expose fixed error codes, never upstream bodies.

Errors use Dropshot's `{request_id,error_code,message}` envelope. Codes are
closed Rust enums: `authentication_required`, `invalid_arguments`,
`permission_denied`, `not_found`,
`not_configured`, `upstream_unavailable`, `invalid_upstream_response`,
`profile_revision_conflict`, `existing_settings_conflict`,
`idempotency_conflict`, `invalid_scope`, `ambiguous_operation_target`,
`precondition_required`, `revision_conflict`, `unsupported_transport_operation`.

## Health

`ComponentHealth` is `{component,observation}`. Observation tagged `state` is
`never_checked`, or `observed` with `{outcome,freshness,observed_at_unix_ms,
expires_at_unix_ms}`. Freshness is `fresh|stale`. Outcome tagged `state` is
`healthy`, `degraded` with `{issues}`, `unavailable` with `{reason}`, or
`not_configured`. Issues contain safe fixed `{code,severity,message}`.

Readiness items: `{operation,profile_id,state,reasons,expires_at_unix_ms}`.
Expiry is the minimum expiry of only the required operation/target evidence;
HA must expire a cached ready result locally, without duplicating dependencies.
Operations are
`request_movie|request_series|transfer|import_movie|import_series`; state is
`ready|blocked`. Profile/root-specific operations require an unambiguous profile
target. Reasons are typed fixed codes. Optional Plex and Prowlarr are not
blanket prerequisites. /healthz retains local watchdog semantics unchanged.

## CLI / HA details

`ha-downloads` uses the same models. Native origin selects DOWNLOADS_URL plus
DOWNLOADS_API_TOKEN (or DOWNLOADS_TOKEN_FILE); HA origin selects
HOME_ASSISTANT_URL/TOKEN. --token-file is a credential-file override, never a
token flag. Native prefix `/api/v1`, HA prefix `/api/downloads/v1`.
CLI success stdout: `{status,etag,body}`. Readiness check unmet is exit 3 with
valid snapshot stdout. Other errors have no stdout, safe JSON stderr; usage 2,
other failures 1. No daemon startup or implicit credential lookup.

HA companion config uses `backend_url` and `integration_token`; its client
adds actor headers only after deriving the actor from authenticated HA context.
Backend read/request proxy is an allowlist, never arbitrary passthrough.
Services search/request/status map to the same API, tools use
downloads__search, downloads__request, downloads__status. JSON input for request
is the exact CreateRequest object. No tool can edit profiles or force releases.
