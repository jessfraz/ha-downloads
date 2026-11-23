Run `nix develop --command just network` after building `ha-downloads:smoke`.
Docker must expose `/dev/net/tun` to disposable Linux containers.

The suite runs the production runtime, Gluetun, SABnzbd and Transmission.
Generated test certificates trust a local TLS DNS server and origin; no VPN
subscription, account credentials or external DNS service is used. A separate
unprotected control proves the origin, IPv6 and plaintext DNS destinations
remain reachable when the OpenVPN server alone is stopped. The protected app
must lose all those paths, including uncached system DNS, while its queue stays
paused. The suite also checks startup gating, reconnect, and abrupt
Gluetun-child cleanup. The watchdog health endpoint remains successful during
startup and recoverable DNS/tunnel outages, then returns 503 after terminal
child-exit cleanup.
SABnzbd's native startup preference is exercised both ways: the initial default
queue is unpaused, and restarting the same app with `start_paused` enabled keeps
the queue paused. Tunnel reconnection preserves both an explicit operator pause
and a running queue without replacing SABnzbd.

Transmission downloads a generated single-piece torrent from a second native
Transmission daemon in the local fixture. The tracker observes the app's VPN
address; native peer readback verifies the payload source is the fixture's VPN
address, and the real consumer verifies the piece hash. While the tunnel
is stopped, new native IPv4 and IPv6 torrents cannot reach those same endpoints,
even though an unprotected control can. The IPv4 transfer completes after VPN
recovery without replacing Transmission. Native RPC readback and completed
transfer state verify the no-seeding policy, bounded upload limit,
authentication, restart persistence and owned-child cleanup. No public torrent
or swarm is used.

Only a uniquely named Docker network, containers, unpublished test image and
private `work/network-*` directory are created. Their IDs are tracked and their
absence is checked during cleanup. A successful receipt is written to
`work/network-receipt.json`. Failed runs do not produce a success receipt.
The generated CA is added only to the disposable test image, never production.
