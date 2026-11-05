# 0.1.3

Distinguish a stopped VPN service from failed health checks and unavailable
status requests. Show per-check results and the most recent failed-check period
in Diagnostic details. Log timestamped check changes, recovery durations and
fixed-message VPN diagnostics without forwarding raw upstream output.

# 0.1.2

Include the dependency updates merged before deployment.

# 0.1.1

Preserve trailing slashes in SABnzbd routes, including setup and login pages.
Request NET_RAW for Gluetun's ping health check in Home Assistant's restricted
runtime. Report handled service failures in the app log.

# 0.1.0

Initial experimental release: a Rust supervisor, Gluetun, SABnzbd, authenticated
Home Assistant ingress, private persistent configuration, and isolated media
storage. Requires deployment validation with the selected VPN provider.
