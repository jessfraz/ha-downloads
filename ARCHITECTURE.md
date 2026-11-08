# Downloads architecture

Downloads is a Home Assistant app that runs SABnzbd behind Gluetun in the same
private container network namespace. A small Rust service owns configuration,
process lifecycle, status, media requests, and authenticated ingress. Gluetun owns the VPN,
firewall kill switch, and DNS. No custom VPN protocol or firewall is implemented.

The app never uses host networking, the Docker socket, host PID/IPC namespaces,
Home Assistant's API, or Supervisor management APIs. The scoped Supervisor
`addons/self/info` and discovery endpoints announce only the companion origin
and dedicated integration credential. NET_ADMIN and NET_RAW apply only to
this app's namespace. NET_RAW supports Gluetun's ICMP health checks. Persistent state belongs in `/data`; media uses the normal
Home Assistant `/media` mapping. No physical installation details or credentials
belong in this repository.

## Startup and failure behavior

1. Validate `/data/options.json` without logging secrets. Invalid or missing VPN
   credentials leave a useful setup/status page; the downloader stays stopped.
2. Require the private Linux app container with this service as PID 1.
   Configured native host execution is rejected before starting VPN services.
   Write private runtime secret files under `/run/downloads` and start Gluetun.
   VPN and firewall cannot be disabled through the supported configuration.
3. Require Gluetun's local health endpoint and DNS configuration to be ready
   before starting SABnzbd. Use Gluetun's resolver inside the namespace.
4. Run SABnzbd on loopback port 8081 with persistent config under `/data/sabnzbd`.
   Its upstream API key and login remain owned by SABnzbd.
5. A tunnel failure cannot create a direct WAN route: Gluetun's firewall remains
   authoritative. Runtime health displays the failure. Downloader lifecycle
   must not silently resume an operator-paused queue.
6. Stop and reap owned children and adopted descendants on termination.
   OpenVPN starts its own process group; final cleanup uses the private
   container PID 1 child list after both direct services have stopped.
   Unexpected child exit is a
   visible failure, never permission to run without the VPN.

## Interfaces

- Rust binary: `downloads serve` (default), `downloads doctor`, `downloads openapi`.
  Use clap derives. Serve reads `/data/options.json` by default. Doctor reports
  sanitized status in JSON; no token or password appears in command arguments.
- Ingress: port 8099, trusted HA ingress peer and authenticated HA identity.
  Read access is separate from allowlisted requesting and administrator access.
  The embedded/proxied SABnzbd browser remains administrator-only.
- Optional direct API: port 8080, no host mapping by default. `/api/v1/status`
  requires a bearer token; SABnzbd API routes retain its own API-key validation.
- SABnzbd reverse proxy: `/sabnzbd/…` to loopback port 8081, preserving query,
  body, content type, cookies and redirects. Request and response bodies are bounded to 16 MiB.
- HTTP CONNECT proxy: Gluetun port 8888, internal apps only by default. Media
  manager external requests can use this tunnel; local HA API calls bypass it.
- Health: `/healthz`, sanitized, no controls or credential information.
- Native HA adapter: a problem sensor, Assist tool, and authenticated
  `GET /api/downloads/health` share active-user read permission and loaded-entry
  checks. Assist additionally requires explicit entity exposure.
  The view returns a cached safe projection, never app
  credentials, ingress routes or connection identifiers.
- Read-only client: `ha-downloads --transport home-assistant status` uses the
  existing HA credential environment. Native status can use a token file or
  the app's local snapshot. The client binary cannot start services.
- Media: typed `/api/v1` resources documented in
  [the media contract](docs/media-contract.md). Native manager clients use fixed
  configured origins, bounded responses, no redirects or automatic retries.
  Only literal IPs in validated local bypass subnets (or loopback) can be used
  before protection is fresh. Hostname lookup and external requests are gated
  before credentials can leave the process. No configured remote manager can
  bypass this boundary when VPN configuration is missing or startup is pending.
- CLI: separate `ha-downloads` binary, without daemon startup or implicit
  provider credentials. One call/page per command.
- Optional HA companion: config entries, cached health entities, native actions,
  scoped Assist tools, and fixed read/request bridge. No second scheduler or
  library mirror. Request opt-in and Assist exposure remain HA-owned.
  The native app preserves its owner-only pairing credential under `/data/media`
  and announces it only through Supervisor; standalone pairing is explicit.

## Durable media requests

Private native-object references and actor-scoped receipts use a versioned
journal under
`/data/media`. Writes sync a temporary file, atomically rename, then sync the
directory before acknowledging. An invalid journal disables media features;
it is never reset or replaced with an empty store. Native HA cold backups cover
this state. Receipts are bounded and never silently pruned.

Acceptance validates canonical identity and native settings, then saves an
immutable execution plan before any native effect. Checkpoints precede
add, episode-monitoring and search writes. Ambiguous effects are reconciled by
canonical library/command readback, never repeated merely because a request
timed out or the app restarted. Selected aired, missing episode IDs are frozen;
no full-series update can overwrite unrelated native settings.

Background health is cached with observation and expiry times. GET does not
refresh evidence timestamps. Readiness is operation scoped; per-title request
eligibility is separate. Optional
Plex and Prowlarr are not universal dependencies. Request readiness does not
claim transfer or import readiness. Imports additionally need current staging
and destination observations. The watchdog remains local and independent.

## Frontend status contract

`GET api/v1/status` relative to ingress base returns:

```json
{
  "version": "0.1.0",
  "state": "needs_configuration",
  "message": "Configure the VPN in the app settings to get started.",
  "vpn": {"state": "disconnected", "provider": "", "public_ip": null},
  "downloader": {"state": "stopped", "url": "sabnzbd/", "paused": null},
  "protection": {"kill_switch": "unverified", "dns": "unverified"}
}
```

States: `needs_configuration`, `starting`, `ready`, `degraded`, `stopping`.
Status never says protected merely because a process exists. Firewall/DNS
verification and tunnel availability are distinct. The UI has no VPN-off or
kill-switch-off control. Upstream SABnzbd owns queue and provider settings.

## Configuration contract

Native HA options are a small nested structure:

```json
{
  "vpn": {
    "provider": "",
    "protocol": "openvpn",
    "username": "",
    "password": "",
    "wireguard_private_key": "",
    "wireguard_addresses": "",
    "countries": ""
  },
  "network": {"local_subnets": ["172.30.32.0/23"]},
  "api_token": ""
}
```

Secrets use HA password fields, redact Debug/error output, and go only into
private runtime files. Configuration rejects public/broad bypass networks and
unsafe provider/filter values. Never provide arbitrary environment overrides.
The HTTP proxy is internal; no unauthenticated LAN proxy is exposed by default.

## Verification

Rust tests cover configuration, redaction, lifecycle boundaries, and real HTTP
behavior. Container tests run the real binary and upstream services. Network
integration tests exercise a disposable local VPN, DNS and origin server,
cut the tunnel, and prove no direct fallback including IPv6. Published images
must be tested on amd64 and arm64. Live deployment additionally verifies the
chosen provider tunnel, TLS, app-to-app requests, NAS access, and a controlled
VPN outage while unrelated HA services remain reachable.
