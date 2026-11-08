# Security boundaries

Downloads runs Gluetun and SABnzbd in one private container network namespace.
NET_ADMIN, NET_RAW and `/dev/net/tun` are requested. The app does not use host
networking/PID/IPC, the Docker socket, Home Assistant API access, Supervisor
management privileges, or arbitrary user-supplied environment overrides.
The default Supervisor role uses only self-info and scoped discovery for the
optional companion; no broad HA or Supervisor API privilege is requested.

Gluetun is the VPN, firewall kill switch, and DNS authority. SABnzbd must not
start until the tunnel and resolver are ready. Tunnel failure does not authorize
direct WAN access. No supported option disables the VPN or kill switch. Local
subnet exceptions must be narrow and validated; IPv6 fallback must be tested.

HA ingress peer and user validation are required for the UI. Authenticated
users may read; allowlisted users may request; only administrators may edit
profiles or open the SABnzbd browser. Direct status uses a read-only bearer
token. Separate requester, administrator and integration credentials must be
distinct. Only the integration credential may delegate authenticated HA actors,
never administration. The HA bridge accepts no forwarded actor assertions.
Proxied SABnzbd API requests retain SABnzbd's API-key checks. The CONNECT
proxy is for trusted internal clients and has no default host port mapping.
Mapping a port does not make it safe for an untrusted network.

Supervisor discovery uses its documented private `http://supervisor` API.
The client fixes the two endpoint paths and disables proxies and redirects.
This platform transport is separate from configurable manager connections;
the Supervisor token and private app network remain trusted HA infrastructure.

Secrets belong in masked HA options, private `/data` state, and private runtime
files. Do not log option values, raw upstream errors, credentials, API keys, or
proxy URLs containing credentials. Cold backups contain secrets and need
encryption and access controls. The app has no telemetry or credential uploads.

Upstream diagnostics are allowlisted fixed messages, not redacted raw logs.
Probe diagnostics contain only typed outcomes, timing, and HTTP status codes.
Unknown response bodies and upstream strings never cross the logging boundary.

Image builds pin upstream sources and digests. Native amd64 and arm64 tests,
including local tunnel-outage/DNS/IPv6 fixtures, gate publication. Those tests
do not replace live provider, ingress, NAS, and controlled-outage verification.

Report a vulnerability through GitHub's private reporting feature if available.
Otherwise request a private reporting channel without posting the details.
Never include credentials, real deployment identifiers, or personal network
details in public issues or pull requests.
