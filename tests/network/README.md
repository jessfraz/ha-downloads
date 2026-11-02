Run `nix develop --command just network` after building `ha-downloads:smoke`.
Docker must expose `/dev/net/tun` to disposable Linux containers.

The suite runs the production runtime, Gluetun and SABnzbd. Generated test
certificates trust a local TLS DNS server and origin; no VPN subscription,
account credentials or external DNS service is used. A separate unprotected
control proves the origin, IPv6 and plaintext DNS destinations remain reachable
when the OpenVPN server alone is stopped. The protected app must lose all those
paths, including uncached system DNS, while its queue stays paused. The suite
also checks startup gating, reconnect, and abrupt Gluetun-child cleanup.

Only a uniquely named Docker network, containers, unpublished test image and
private `work/network-*` directory are created. Their IDs are tracked and their
absence is checked during cleanup. A successful receipt is written to
`work/network-receipt.json`. Failed runs do not produce a success receipt.
The generated CA is added only to the disposable test image, never production.
