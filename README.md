# Downloads

Search movies and TV, request exact seasons with saved Radarr/Sonarr settings,
and follow acquisition and stack health in Home Assistant's Downloads sidebar.
SABnzbd downloads through a Gluetun VPN. The downloader waits for the VPN,
firewall, and DNS checks to pass. SABnzbd's **Start paused** setting controls the
queue after an app restart; its default allows downloads to resume automatically.

The app uses NET_ADMIN, NET_RAW and `/dev/net/tun`. It does not use host networking,
the Docker socket, Home Assistant's API, or Supervisor management APIs. Scoped
Supervisor discovery pairs the optional HA companion. Media
is available under `/media`; private options and SABnzbd state live in `/data`.

## Install

Add `https://github.com/jessfraz/ha-downloads` as a Home Assistant app repository,
install **Downloads**, then configure the VPN in the app settings. Set the
provider and protocol, plus the provider's service username and password for
OpenVPN or its private key and addresses for WireGuard. Provider identifiers
are listed in [Gluetun’s provider guide](https://github.com/qdm12/gluetun-wiki/tree/main/setup/providers).
Open the sidebar panel
to configure SABnzbd and resume the queue when ready.

Missing credentials leave the setup page available and the downloader stopped.
Verify the chosen provider, media paths, and tunnel failure behavior before
relying on a deployment.

See [app documentation](downloads/DOCS.md) for options, storage, and connecting
media managers. No indexer, Usenet account, or VPN subscription is included.

## Media, Assist and CLI

Configure manager origins and keys in the masked `media` app settings.
Quality rules, library folders and downloader configuration belong to Radarr,
Sonarr and SABnzbd. Downloads uses the configured native selections automatically;
it has no profile editor or settings API. Existing titles keep their native
configuration. A lost acknowledgement is recovered by its original request/key,
not a duplicate write. Accepted and applied requests are distinct from
completed downloads and imports.

The optional [native HA companion](integration/README.md) adds cached health and
readiness entities, actions, scoped Assist tools and an authenticated HA bridge.
Media requests default off; Assist also requires explicit entity exposure.
The sidebar remains the app UI, with the existing shared header/footer.

[`ha-downloads`](docs/cli.md) provides native and HA transports with the same
typed contract, explicit idempotency and JSON output. The daemon keeps its
existing `downloads serve`, `doctor` and `openapi` commands.
See the [complete API contract](docs/media-contract.md), including existing
status, watchdog and SABnzbd endpoints.

## Diagnostics

Install this repository as a HACS custom integration, then add **Downloads** in
Home Assistant's integration settings to expose its problem sensor and optional
read-only Assist health query. Select the installed app; no address, token,
port mapping or additional app permission is needed. The adapter follows the
app's current internal address through Supervisor. Expose its problem sensor
to Assist to opt in; queries still require the user's entity read permission.

Functional readiness combines current VPN, firewall, DNS and downloader API
checks with SAB's own decoder compatibility check. Idle or deliberately paused
queues stay healthy. Failed status reads make the entity unavailable instead of
claiming the downloads are protected.

Open **Diagnostic details** in the panel for individual check results and the
most recent failed-check period, including after recovery. A stopped VPN
service, a failed upstream health check, and an unavailable status request are
reported separately.

Enable Home Assistant's watchdog to restart the app after a terminal service
failure. Setup and recoverable VPN outages do not trigger a restart.

Home Assistant's app **Logs** tab records timestamped check changes, recovery
durations, and allowlisted VPN lifecycle events and warnings. Raw upstream
output is never forwarded: proxy logs can contain credentials unknown to this
app. See [diagnostic behavior](downloads/DOCS.md#diagnostics) for retention and
failure details.

## CLI and compatible status

`ha-downloads` is available from this repository's default Nix package. Help
works offline. Use the existing Home Assistant credential wrapper for HA access:

```sh
ha-downloads --help
fetch-home-assistant -- ha-downloads --transport home-assistant status
```

The HA route requires app and integration version 0.1.6 or newer and reads the
integration's cached, safe health snapshot. It requires a loaded Downloads
integration and the active user's entity read permission. Assist tools also
require the problem sensor exposed to Assist. It does not forward credentials
to the app, use ingress sessions, change the queue, or start any service.

A standalone app can instead use its existing native API and token file:

```sh
ha-downloads status --url https://downloads.example/ --token-file /path/to/token
```

`DOWNLOADS_URL` and `DOWNLOADS_TOKEN_FILE` supply the same native options. Inside
the app, `downloads doctor` or `ha-downloads status` can read the private local
status file. Native output preserves the status fields and adds `transport` and
`health`; HA output contains only `transport` and the safe `health` projection.
Remote health uses the server's check age, independent of the client's clock.
HA access requires HTTPS, except for HTTP on loopback. The client ignores ambient
proxy configuration and does not retry requests.

Status output is typed JSON. Exit 0 means ready, 2 means health is not ready
(including stale checks), and 1 means the request could not be completed. Errors
are redacted JSON on stderr, with a fixed code and optional HTTP status. Only the
`downloads serve` command starts app services, within its existing private Linux
container boundary. The `ha-downloads` client has no `serve` command; media
requests use the explicit commands in the
[CLI contract](docs/cli.md).

## Develop

```sh
nix develop
just check
just security
just openapi
just container
just network
```

Build `downloads.zip` with `just integration-zip`. Attach the ZIP from
`work/` before publishing the release.

Rust 1.99.1, cargo, just, uv, actionlint, ShellCheck, and the Docker CLI are in
the development shell. Docker needs a running Linux daemon with TUN support.
The network suite uses disposable local fixtures and no real VPN credentials.
The Linux shell also includes locked Playwright and Chromium for `just web`.
Other platforms can use existing browser tools as described in
[the browser test instructions](tests_web/README.md).

The Dockerfile pins Gluetun v3.41.3 and SABnzbd 5.1.3, verifies the SABnzbd source
archive SHA256, and installs Python dependencies from `uv.lock`. The Rust binary
is built with musl on each architecture. Release tags are published only after
both native architectures pass the same container and network checks. Local
fixtures cover DNS and tunnel failure; live provider, ingress, and storage
checks remain necessary for each installation.

See [architecture](ARCHITECTURE.md), [contributing](CONTRIBUTING.md), and
[security boundaries](SECURITY.md). Downloads' code is MIT licensed; bundled
components retain their upstream licenses listed in [NOTICE](NOTICE).
