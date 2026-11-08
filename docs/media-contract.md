# Downloads media contract

Rust models in `src/media/model.rs` own the public JSON contract. Paths below
are relative to `/api/v1`; the HA companion exposes the read/request subset at
`/api/downloads/v1`. Requests reject unknown fields and enum values. Clients do
not retry writes, follow redirects, or invent settings.

Downloads searches, requests and reports progress and health. Radarr, Sonarr
and SABnzbd own configuration. No Downloads resource edits quality profiles,
library folders, tags or downloader settings. Previously configured native
selections are private references to those native objects; their definitions
are resolved from the managers. Existing titles always use their own native
configuration. No profile IDs or setting selections appear in request bodies.

## Existing endpoints

| Method | Absolute path | Contract |
| --- | --- | --- |
| GET | `/healthz` | Local watchdog with safe functional health |
| GET | `/api/v1/status` | VPN/downloader/protection status |
| GET | `/api/v1/health` | Existing functional readiness summary |
| GET | `/`, `/app.js`, `/style.css` | Authenticated sidebar UI/assets |
| GET | `/sabnzbd` | Native SABnzbd browser redirect |
| GET, POST, PUT, DELETE | `/sabnzbd/{path…}` | Existing bounded native SABnzbd protocol proxy |

The SAB protocol remains available to configured download clients. The sidebar
no longer embeds its settings console. The watchdog and native SAB API keep
their existing authentication policies.

## Identity and authorization

`api_token` is read-only. Independent masked `media.request_token` and
`media.admin_token` credentials permit requesting; administrators can also
inspect other actors' receipts. The dedicated `media.integration_token` can
read/request through delegated HA actor context, never administer or access
SABnzbd. An HA app generates its stable private integration credential when
that option is blank and pairs it through scoped Supervisor discovery.
Standalone pairing uses an explicit integration credential.

The companion derives `X-Downloads-Actor-Id` and
`X-Downloads-Actor-Can-Request` from the authenticated HA user, current options,
entity permission and Assist exposure. Other credentials cannot assert those
headers. Body fields never choose the actor. Non-admin ingress requests require
an explicit user-ID allowlist; administrators can request through ingress.

## Read routes

- `GET /system`: `{version,capabilities:{read,request,admin},transport,
  credential_role}`. Roles: `read_only|requester|administrator|integration`.
- `GET /health/stack?operation=OP`: complete `HealthSnapshot`, optionally scoped
  to one operation. No profile or configuration target arguments.
- `GET /health/components/{component}`: one `ComponentHealth`.
- `GET /managers`: `{items:ManagerSummary[]}` with configured/available/version.
- `GET /search?query=TEXT&kind=all|movie|series&limit=20&page_token=TOKEN`:
  `Page<MediaItem>`.
- `GET /movies?limit=20&page_token=TOKEN`: movie library page.
- `GET /movies/{tmdb_id}`: movie detail.
- `GET /series?limit=20&page_token=TOKEN`: series library page.
- `GET /series/{tvdb_id}`: series detail, including seasons.
- `GET /series/{tvdb_id}/episodes?season=2&limit=20&page_token=TOKEN`: episode page.
- `GET /queue?limit=20&page_token=TOKEN`: acquisition queue page.
- `GET /requests?limit=20&page_token=TOKEN`: actor-scoped receipt page.
- `GET /requests/{request_id}`: actor-scoped receipt, or administrator inspection.
- `GET /request-keys/{key}`: original actor-scoped receipt, or 404. An in-flight
  submission may still accept later; preserve its original key and input.
- `GET /artwork/{kind}/{media_id}/{artwork}`: authenticated bounded image bytes;
  kind is `movie|series`, artwork is `poster|backdrop`.

There are no `/profiles` resources or `/managers/{manager}/options` routes.
No manager configuration passthrough is exposed.

`Page<T>` is `{items:T[],next_page:string|null,coverage:SourceCoverage[]}`.
Coverage is `{source,state,observed_at_unix_ms}`; state is
`available|unavailable|unsupported|partial`. An unavailable source is not an
empty library. Page limits are 1..100; tokens bind route and query scope.

`MediaItem` is tagged by `kind`:

- movie: `kind:"movie",tmdb_id:ID`.
- series: `kind:"series",tvdb_id:ID,seasons:Season[]`.

Both contain `title,year,overview,poster,request,library,acquisition`. `poster`
is a relative authenticated API path or null; `year` is a number or null.
`request` is exactly `{state:"ready"}` or `{state:"parent_setup_required"}`.
The latter means the existing native selection is missing or unusable; the UI
cannot substitute or edit it. With a configured native binding, adding a new
title uses it automatically. An installation without a binding can proceed
only when the native choice is unambiguous; it never guesses by ordering/name.

Library state is `{state:"not_added"}` or `{state:"managed",upstream_id:ID}`.
No quality-profile or folder metadata is included. Acquisition is tagged by
`state`: `unknown|not_requested|waiting|downloading|importing|imported|blocked|
unavailable`. Downloading includes completed/total bytes; blocked includes a
closed reason; unavailable identifies a component. Imported does not establish
Plex playback.

`Season` contains `number,episode_count,imported_count,active_count,monitored,
state`; unproven counts are null. State is
`unknown|missing|partial|in_progress|imported`. `Episode` contains
`id,season,number,title,air_date,monitored,acquisition`. `QueueItem` contains
`id,media,title,episode,acquisition`; media is a canonical movie/series identity.

## Requests

`POST /requests` requires requesting capability and caller-owned
`Idempotency-Key`. Inputs contain only identity and acquisition intent:

```json
{"kind":"movie","tmdb_id":14337,"intent":{"mode":"search_now"}}
```

```json
{"kind":"series","tvdb_id":371980,"intent":{"mode":"search_now","scope":{"kind":"seasons","numbers":[1]}}}
```

Movie intent is `search_now|monitor_only`. Series intent is `search_now` with
`{kind:"all_aired"}` or `{kind:"seasons",numbers:[1]}`, or `monitor_only`.
Explicit seasons are nonempty, unique and bounded. Canonical IDs are positive
and within the native API's signed-integer range. Monitoring requests still
respect native configuration; no request silently changes existing settings.

Acceptance validates current native objects and saves an immutable execution
plan before any effect, returning 202 and Location. The same actor/key/input
returns its original receipt; a different input returns `idempotency_conflict`.
A lost upstream response reconciles by native readback, never blind replay.
Selected-season searches change only missing, aired selected episodes and
preserve unrelated monitoring. No full-series PUT overwrites native settings.

`RequestRecord` is `{id,created_at_unix_ms,input,submission}`. Submission is
`pending|applying|applied|reconciling|failed`. Applied includes
`{result:{effect,media,manager,upstream_id,search}}`; effects are
`added|already_managed|already_satisfied`. Search is `not_requested` or queued
with a command ID. Applied is not proof of download or import completion.

Errors use Dropshot's safe `{request_id,error_code,message}` envelope. Codes
are closed Rust enums. `native_settings_required` is 409; the app never creates
a replacement quality configuration to force a request through. Missing replay
keys are 428. Invalid names, types, enums and obsolete profile fields are 400.

## Health and recovery

Components: `radarr|sonarr|prowlarr|sabnzbd|vpn|firewall|dns|storage|plex`.
Operations: `request_movie|request_series|transfer|import_movie|import_series`.
Observations distinguish never-checked, fresh/stale, healthy, degraded,
unavailable and not-configured. Issues have fixed safe code/severity/message.
Update warnings remain visible without blocking unrelated operations.

Readiness is `{operation,state,reasons,expires_at_unix_ms}`. Expiry uses only
required evidence. Destination proof comes from current native roots; staging
proof remains separate. Per-title request eligibility is independent of generic
operation health. HA expires cached ready verdicts locally. The watchdog keeps
its existing local semantics.

The private v2 journal migrates existing native-object references and accepted
plans, checkpoints, actor/key bindings and original fingerprints. Retired
profile resources disappear. An uncertain older profile-bearing draft keeps
its original key for receipt lookup and cannot be rewritten/reposted under the
new schema. Restore a native HA backup to roll back the journal format.

CLI and HA use these same resources and models. The companion exposes no
settings controls; Assist resolves canonical IDs, preserves native choices,
and reports actual status. No tool edits profiles, forces releases, or controls
VPN/power/access.
