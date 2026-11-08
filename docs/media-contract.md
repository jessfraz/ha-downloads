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
- `GET /queue/{queue_id}`: fresh, read-only `DownloadDetails`. IDs retain the
  canonical `radarr:123|sonarr:456` form with a positive native signed integer.
  A disappeared entry returns 404 only after a complete successful native read;
  unavailable or incomplete evidence returns 503.
- `GET /requests?limit=20&page_token=TOKEN`: actor-scoped receipt page.
- `GET /imports?limit=20&page_token=TOKEN&since=UTC`: recent successful native
  imports from Radarr and Sonarr, including titles requested directly in those
  apps. `since` is optional, inclusive and validated before provider reads;
  cursors bind its UTC instant. The window contains up to 1,000 imports per
  manager and reports partial coverage when older records are omitted.
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
New titles follow native Add defaults for availability and season folders.

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

Optional `MediaItem.metadata` contains observed runtime, genres and
certification. Movies also include studio and an optional imported `file`
with actual quality, size, video/audio codecs and audio channels. Series include
network and next airing. Missing evidence is null, never a default setting.

`DownloadDetails` contains `id,manager,media,title,episode,acquisition,status,
quality,progress,remaining_seconds,download_client,indexer,protocol,
import_status,warnings,languages,custom_formats,custom_format_score,added_at,
estimated_completion_at,observed_at_unix_ms`. Status is
`queued|paused|downloading|completed|failed|unknown`; import status is
`not_started|pending|importing|imported|failed|unknown`. Protocol is
`usenet|torrent|unknown` or null. A completed transfer does not imply import.
Progress contains completed/total/remaining bytes, or null when unknown.
Quality describes the actual release, including name, resolution, source,
modifier and revision. It never describes or edits a quality profile.
Warnings use closed code/severity values and bounded safe messages. Native
paths, download IDs, credentials and configuration are not exposed.

`ImportedItem` contains `id,manager,media,title,episode,imported_at,quality,
plex,subtitles,observed_at_unix_ms`. Its ID is a typed native history ID, not a
queue ID. Only successful native import events produce these records.
Plex evidence is `not_configured|unavailable|unknown|verified`; unknown and
unavailable states carry closed reasons. Verification requires canonical
identity, the same imported file and size, current Plex metadata, and file
availability. Subtitle evidence is `unknown|verified|missing`; verified
languages belong to that same imported file. These checks do not test playback
or establish that every preferred subtitle language is present.

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
When running under Supervisor, a Prowlarr available-version notice is shown
only when Home Assistant reports an available package for the same configured
installation. Unknown package evidence suppresses that availability notice;
functional warnings and failed update checks remain visible. The default
Supervisor role grants read-only info access for this check. Standalone
installations retain native update notices.

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

## Home Assistant completion notifications

The companion observes native imports independently of health polling and
request receipts. Its first enablement establishes a quiet baseline; existing
history remains visible in Activity without flooding Notifications. New
imports create a native persistent notification. Plex and subtitle evidence
update that same notification ID instead of creating a second notice.

Native import IDs, compact delivery ledgers and separate ready-event state
survive reload/restart. Out-of-order imports remain eligible. Store writes are
read back before further observations advance state. Identical evidence,
corrected titles and reordered language lists do not repeat completion events.
`downloads_media_imported` and `downloads_media_ready` include stable
`import_id`, canonical `media`, `imported_at`, `plex_verified` and
`subtitles_verified`. Ready requires both verification flags. Automation events
are delivered at least once: consumers can deduplicate `import_id` across a
crash between notification dispatch and persistence.

The read-only `downloads.imports` action and `downloads__imports` Assist tool
use the same bounded resource and existing user/exposure permissions. Neither
requests media or changes settings. An inaccessible Plex library stays
unverified even when the Plex server health check succeeds.
