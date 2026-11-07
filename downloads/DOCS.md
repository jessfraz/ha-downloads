# Configure Downloads

Configure `vpn.provider` with a provider supported by Gluetun, and select
`openvpn` or `wireguard`. OpenVPN uses the provider's VPN service username and
password; these may differ from its website login. WireGuard uses the private
key and tunnel addresses supplied by the provider. `vpn.countries` is an
optional Gluetun country filter. Invalid settings leave the downloader stopped.

HA masks passwords, WireGuard private keys, and `api_token` in the app form.
These options and SABnzbd's configuration are sensitive persistent `/data`
contents, including in cold backups. Use Home Assistant's encrypted backups and
protect exported copies. Runtime VPN secret files belong in `/run/downloads`.
The `media` connection keys and role tokens are also masked and sensitive.

`network.local_subnets` allows narrowly scoped local app/API traffic to bypass
the tunnel. The default is Home Assistant's internal app network. Never add the
public internet, a default route, or an unnecessarily broad LAN range. The app
validates this list and provides no arbitrary Gluetun environment overrides.

## SABnzbd and media managers

Open Downloads through Home Assistant ingress, then use the SABnzbd interface
to configure Usenet servers, credentials, categories, and queues. Enable TLS
and certificate verification for each Usenet server. Provider
traffic stays inside the app's VPN namespace. SABnzbd owns its own API key and
login; Downloads does not import a provider or library automatically. The queue
uses SABnzbd's startup pause policy. Leave **Start paused** disabled and enable
**Preserve paused state** to resume active downloads after a restart while
retaining intentional pauses.

Persistent SABnzbd configuration uses `/data/sabnzbd`. Place completed media
under a chosen `/media` directory that is also mapped into the media manager.
Mount and verify remote storage in Home Assistant before using its paths. Do
not put the application configuration on a network share.

The optional direct port is 8080, with host mapping off by default. Set
`api_token` for the status API and use the actual app hostname or mapped address,
for example `http://downloads.example:8080`. Media managers use `/sabnzbd/api`
with SABnzbd's API key. The SABnzbd browser interface is available through Home
Assistant ingress.

Gluetun's HTTP CONNECT proxy listens internally on port 8888. Its host mapping
is also off by default. Configure only trusted internal applications to use it
for external indexer requests; keep local application API calls out of the
proxy. Do not expose an unauthenticated proxy to a LAN or the internet.

## Movie and TV requests

Set `media.radarr.url` and/or `media.sonarr.url` to reachable native manager
origins, with their native API keys. Optional base paths are supported. Do not
use a Home Assistant ingress URL. Local destinations must be inside the narrow
`network.local_subnets` exceptions; literal IPs in those exceptions remain
reachable without the VPN. Hostnames and external manager traffic require
current protection checks before lookup or connection, and stay subject to
the VPN firewall. Optional `media.prowlarr` and `media.plex` connections add
health observations, not a prerequisite for every request.

In the Downloads sidebar, create saved profiles from the manager's actual
quality profiles, roots and tags. A request pins the selected profile revision
and exact movie/TV identity. Selected-season requests exclude specials unless
explicitly selected, future episodes and episodes already imported. Existing
native quality/root settings are preserved; conflicts need a deliberate profile
selection, not a hidden overwrite. Future monitoring is a separate saved TV
policy, defaulting off.

All authenticated HA users can read the sidebar. HA administrators can request
and edit saved profiles. Add non-admin HA user IDs to
`media.requester_user_ids` only when they should be able to request media.
The SABnzbd browser/settings remain administrator-only.

For native CLI/API access, set independent, distinct role credentials of at
least 32 characters: `api_token` is read-only, `media.request_token` permits
requests, and `media.admin_token` permits saved-profile writes. The dedicated
`media.integration_token` is only for the optional HA companion. It cannot edit
profiles or access SABnzbd. Never reuse a HA token or manager key as a role token.

Install the [native companion](../integration/README.md) through HACS/manual
custom integration, then confirm discovery or pair the native app origin.
Requests default off in its options. Assist additionally requires exposing the
Downloads stack health entity to the assistant. No separate LLM API is needed.
The [CLI](../docs/cli.md) can use native or HA transport with caller-owned keys.

Health shows individual components, observation freshness and operation-specific
readiness. Optional components can be unavailable while requesting still works.
An accepted/applied request does not claim downloaded, imported or playable.
If acknowledgement is lost, recover by its original key before another request.
Do not delete `/data/media` to resolve uncertain work. The private journal holds
the receipts and checkpoints needed to prevent duplicate native writes.

## Startup, failures and updates

Status distinguishes missing configuration, startup, readiness, degradation,
and shutdown. A running process alone does not prove a working tunnel, firewall,
or DNS. A tunnel failure leaves Gluetun's firewall authoritative. An
operator-paused queue remains paused across tunnel recovery. After an app restart,
SABnzbd's **Start paused** setting controls whether the queue resumes. Leave it
disabled for automatic recovery. Protection checks must pass before SABnzbd starts.

Use Home Assistant to update the entire app. Gluetun, SABnzbd, Python packages,
and the Rust supervisor are pinned and tested together; do not install packages
or self-update individual components inside the running container.

Before relying on a deployment, confirm the chosen provider's tunnel and public
IP, DNS behavior, TLS server access, app-to-app requests, and media permissions.
Cut the tunnel in a controlled test and verify downloads and proxied external
requests fail while unrelated Home Assistant services remain reachable.

## Diagnostics

Expand **Diagnostic details** in the panel to inspect the VPN service, upstream
health check, DNS service, DNS lookup, resolver configuration, and firewall.
Results include elapsed time, HTTP status where applicable, and a fixed error
category. A health endpoint failure does not establish that the tunnel stopped.
Downloads remain gated on all required checks; the firewall remains authoritative.

The panel and authenticated status API retain the most recent failed-check
period and its duration after recovery. Counters and this in-memory history
reset on app restart. Home Assistant's journal retains timestamped transition
and recovery events according to its own log retention, including earlier app
runs. View them in this app's **Logs** tab.

If a protected service fails and cannot continue, the health endpoint returns
HTTP 503 so Home Assistant's enabled watchdog can restart the app. Setup and
recoverable tunnel outages remain healthy. Current checks are cleared after
terminal shutdown; earlier check history remains available. Logs identify
known failure reasons and include the final HTTP or transport result when
downloader startup times out, without logging response bodies or URLs.

The same internal `/healthz` response includes functional readiness, fixed reason
codes, the oldest check's timestamp and age, and the most recent terminal
failure's source and exit status. Its HTTP status retains the watchdog behavior
above: a recoverable outage returns 200 with `ready: false`. The response contains
no provider, public IP, configuration, credential or free-form service output.
The authenticated `/api/v1/health` endpoint returns the same projection.
Checks older than 30 seconds, missing checks and future timestamps cannot be
ready. The native HA integration polls this endpoint every 10 seconds through
the existing internal network; its problem sensor is independent of queue
activity. No host port mapping is required.

VPN initialization, reconnection, and known upstream warnings/errors become
fixed-message events. Unknown warnings use a generic code; unknown informational
messages are discarded. Raw upstream lines, URLs, settings, and response bodies
are never logged. Repeated upstream events are rate limited, and a failed or slow
log sink cannot block the VPN process.
The journal sink uses the Linux stderr pipe provided by Home Assistant/Docker;
non-pipe or unavailable outputs disable logging rather than risk blocking.
