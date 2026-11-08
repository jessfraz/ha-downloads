# 0.3.0

Removes Downloads profile management from the sidebar, API, CLI and Assist.
Existing native quality and library selections stay private and automatic;
Radarr/Sonarr keep ownership of their rules. The sidebar no longer embeds
advanced downloader settings. Existing titles keep native configuration, and
new titles follow native Add defaults for availability and season folders.
Accepted requests survive the journal migration. Health warnings now identify
available upstream updates clearly. The shared header/footer stays unchanged.

# 0.2.0

Search and request movies or exact TV seasons with saved Radarr/Sonarr profiles
in the Downloads sidebar. Add typed media APIs, durable request recovery,
component health, native Assist tools, and CLI media commands. Keep existing
health, status, problem sensor, and CLI contracts compatible.
Pair through Supervisor using a stable private app credential, with no manual
token provisioning. Check Plex identity health without requiring a token.

# 0.1.7

Match SABnzbd's required native decoder version and check its actual native
compatibility flag before reporting readiness. Keep module failures distinct
from VPN outages. Verify packaged module compatibility and image source labels.

# 0.1.6

Expose fresh functional readiness through the existing internal health endpoint
and a native, read-only Home Assistant integration. Track downloader API checks,
retain typed terminal failure details, and reject stale ready snapshots in the
doctor command. Add an exposure- and permission-gated Assist health query.
Align the panel header and footer with the other apps, including its source link.
Add a read-only `ha-downloads` CLI with native and authenticated Home Assistant
transports, typed health/reasons, redacted JSON errors, and honest exit codes.

# 0.1.5

Report terminal service failures as unhealthy so Home Assistant's enabled
watchdog can restart the app. Keep setup and recoverable VPN outages healthy.
Clear current protection checks after services stop, retain their history, and
log the fixed failure reason and final downloader startup probe result.
Honor SABnzbd's Start paused setting instead of forcing the queue paused after
every app restart. Start the downloader only after protection checks pass and
preserve operator pauses during tunnel recovery.

# 0.1.4

Use fresh HTTP connections for local Gluetun probes to avoid reusing connections
near the upstream server's short request deadlines. Keep the same timeouts,
authentication, and protection checks.

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
