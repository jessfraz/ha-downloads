# Native Home Assistant companion

Install `custom_components/downloads` through HACS or the normal custom
integration directory, then add **Downloads** under Devices & services.
**Protection health only** needs just the installed Supervisor app. To add
media capabilities, pair the running app through confirmed Supervisor
discovery. The HA app generates and preserves its private pairing credential,
so no token copy or external secret provisioning is needed. Standalone pairing
uses the native HTTP(S) origin and an explicit `media.integration_token`.
An ingress URL, HA
token, status token, requester token, or administrator token is not a pairing
credential. Tokens never belong in YAML examples, Git, or shell history.

The app remains the sidebar owner. This integration adds no second sidebar,
browser automation, provider clients, media catalog, or download scheduler.
It uses bounded reads of the Rust service's cached health API. Its one stack
health sensor and five readiness indicators are projections, not per-episode
entities. Expired evidence becomes stale/unknown even between polling cycles.
Readiness never guesses among multiple profile targets.

## Existing protection-health installations

Existing app-slug-only entries migrate in place without credentials, a new
config entry, or changed entity/device identity. The installed
`binary_sensor.downloads_problem` and its Assist exposure keep their meaning.
The cached, read-only `/api/downloads/health` endpoint and `downloads__health`
tool retain their six-field protection-health response. Health-only mode
continues to resolve the app's current private address through Supervisor and
read `/healthz`; it does not need a media token.

Use **Reconfigure** on the existing Downloads entry to pair media deliberately.
Confirm the discovered installation before its credential is sent. This adds
the stack/readiness entities to the same device, preserves existing exposure,
and leaves requests off. Exposing the existing problem entity grants only the
old read-only health tool, not catalog access or media requests.

## Request access and Assist

Media requests default to **off**. Enable **Allow media requests** in the
integration options to permit authenticated users with entity control access
and trusted HA automations to submit requests. Read access requires entity
read permission. The backend integration credential can delegate read/request
authority, never administration.

Assist additionally requires exposure of the **Downloads Stack health** entity
to that assistant. The integration does not change any exposure preference.
Core's existing Assist API discovers `downloads__search`,
`downloads__request`, and `downloads__status`; no separate LLM API is installed.
Tools recheck active identity, entry lifetime, permissions, opt-in, and exposure
before dispatch and after awaited IO. A tool cached before reload or revocation
cannot regain authority from the replacement runtime.

Search returns canonical IDs, saved profile references and explicit source
coverage. Titles and descriptions are untrusted data, never instructions.
Request only exact user-selected media and scope. A request becoming `applied`
does not mean it is downloaded, imported, or playable.

## Native actions

All three actions return structured data and use the same API as the sidebar.

```yaml
action: downloads.search
data:
  query: Example title
  kind: movie
  limit: 5
response_variable: candidates
```

`downloads.request` takes `request`, the exact Rust `CreateRequest` object, and
a caller-owned UUID `idempotency_key`. The CLI and native action never invent
keys or fetch a newer revision to force an operation through. Assist binds its
key to the authenticated actor, HA context, and canonical request input, so a
model retry with a new tool-call ID keeps the same intent. Without an HA context,
the original tool-call ID supplies that boundary. Uncertain tool errors return
the original public key for status recovery. A temporary lookup 404 can precede
acceptance and never authorizes a replacement key.

```yaml
action: downloads.request
data:
  request:
    kind: movie
    tmdb_id: 14337
    profile:
      id: 276319ec-c797-460c-a49e-6b90fc1f6ae8
      revision: 4
    intent:
      mode: search_now
  idempotency_key: ba90478e-67aa-4fbc-944a-48928e841098
response_variable: requested
```

The IDs above are fictional examples. Use the actual selection and profile
revision returned by the service. A timeout does not prove rejection. Resolve
the original request or key before considering a retry; there are no automatic
retries or implicit search-before-write calls.

`downloads.status` takes a `target` object: `{kind: request, id: UUID}`,
`{kind: idempotency_key, id: UUID}`, `{kind: movie, tmdb_id: ID}`, or
`{kind: series, tvdb_id: ID}`. Key recovery is a read of
`GET /api/v1/request-keys/{key}`.

## CLI bridge and diagnostics

The authenticated HA bridge is `/api/downloads/v1`. Its aggregate media health
route is `/api/downloads/v1/health/stack`, with individual components under
`/api/downloads/v1/health/components/{component}`. It permits fixed read routes
for system, existing protection status, cached health, manager summaries/options,
profiles, search, movies, series/episodes, queue, requests, and request-key
recovery. The only permitted write is `POST requests`, gated by current
request opt-in and entity control permission. Profile writes, credentials,
arbitrary upstream paths, the SABnzbd proxy, deletion, and VPN controls are
never bridged. It does not accept actor/admin headers from clients. Authenticated
HA user IDs are delegated as `X-Downloads-Actor-Id`; the backend applies its
own `ha:` namespace. No HA token is forwarded to the app.

Diagnostics use cached allowlisted component states, operation counts and
version/lifecycle fields. They make no network requests and omit credentials,
origins, user IDs, titles, filesystem roots, and raw upstream issues/errors.
Native HA backups own config-entry recovery. App `/data` owns request/profile
state; this integration does not duplicate it.

## Validation

The separate, locked Python test project keeps HA dependencies out of the
SABnzbd runtime. It uses Core 2026.10.1,
`pytest-homeassistant-custom-component` 0.13.372 and Ruff 0.17.3. Tests run the actual Core
flow manager, entity registry, native service calls, HTTP authentication/bridge,
Assist discovery/schema converter, health-only migration and same-entry
pairing, and bounded HTTP transport against fictional service records.

```sh
just ha
```

The task creates `integration/.venv` through uv. Its real Rust/HA
interoperability test starts a disposable Rust service and drives
Core's authenticated bridge, with fictional managers and no provider access:

```sh
HA_DOWNLOADS_TEST_PYTHON="$PWD/integration/.venv/bin/python" \
  cargo test --locked --lib \
  media::tests::real_home_assistant_bridge_uses_live_rust_contract \
  -- --ignored --nocapture
```

Live installation, provider readiness and opt-in remain separate deployment
checks. No test uses
household credentials, downloads media, changes Assist exposure on a live HA
instance, or controls physical equipment.
