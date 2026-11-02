# Downloads architecture

Downloads is a Home Assistant app that runs SABnzbd behind Gluetun in the same
private container network namespace. A small Rust service owns configuration,
process lifecycle, status, and authenticated ingress. Gluetun owns the VPN,
firewall kill switch, and DNS. No custom VPN protocol or firewall is implemented.

The app never uses host networking, the Docker socket, host PID/IPC namespaces,
Home Assistant's API, or Supervisor management APIs. NET_ADMIN applies only to
this app's namespace. Persistent state belongs in `/data`; media uses the normal
Home Assistant `/media` mapping. No physical installation details or credentials
belong in this repository.

## Startup and failure behavior

1. Validate `/data/options.json` without logging secrets. Invalid or missing VPN
   credentials leave a useful setup/status page; the downloader stays stopped.
2. Write private runtime secret files under `/run/downloads` and start Gluetun.
   VPN and firewall cannot be disabled through the supported configuration.
3. Require Gluetun's local health endpoint and DNS configuration to be ready
   before starting SABnzbd. Use Gluetun's resolver inside the namespace.
4. Run SABnzbd on loopback port 8081 with persistent config under `/data/sabnzbd`.
   Its upstream API key and login remain owned by SABnzbd.
5. A tunnel failure cannot create a direct WAN route: Gluetun's firewall remains
   authoritative. Runtime health displays the failure. Downloader lifecycle
   must not silently resume an operator-paused queue.
6. Stop and reap owned children on termination. Unexpected child exit is a
   visible failure, never permission to run without the VPN.

## Interfaces

- Rust binary: `downloads serve` (default), `downloads doctor`, `downloads openapi`.
  Use clap derives. Serve reads `/data/options.json` by default. Doctor reports
  sanitized status in JSON; no token or password appears in command arguments.
- Ingress: port 8099, trusted HA ingress peer only. Admin panel status/setup and
  an embedded/proxied SABnzbd interface. Public origin examples use `.example`.
- Optional direct API: port 8080, no host mapping by default. `/api/v1/status`
  requires a bearer token; SABnzbd API routes retain its own API-key validation.
- SABnzbd reverse proxy: `/sabnzbd/…` to loopback port 8081, preserving query,
  body, content type, cookies and redirects. Request and response bodies are bounded to 16 MiB.
- HTTP CONNECT proxy: Gluetun port 8888, internal apps only by default. Media
  manager external requests can use this tunnel; local HA API calls bypass it.
- Health: `/healthz`, sanitized, no controls or credential information.

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
