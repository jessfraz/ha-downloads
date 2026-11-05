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
starts paused on each app start. Resume it in SABnzbd when ready.

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

## Startup, failures and updates

Status distinguishes missing configuration, startup, readiness, degradation,
and shutdown. A running process alone does not prove a working tunnel, firewall,
or DNS. A tunnel failure leaves Gluetun's firewall authoritative. An
operator-paused queue remains paused across tunnel recovery. Starting the app
again always starts the queue paused.

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

VPN initialization, reconnection, and known upstream warnings/errors become
fixed-message events. Unknown warnings use a generic code; unknown informational
messages are discarded. Raw upstream lines, URLs, settings, and response bodies
are never logged. Repeated upstream events are rate limited, and a failed or slow
log sink cannot block the VPN process.
The journal sink uses the Linux stderr pipe provided by Home Assistant/Docker;
non-pipe or unavailable outputs disable logging rather than risk blocking.
