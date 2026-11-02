# Contributing

Use `nix develop`, then `just check` and `just security`. Run `just container`
for image changes and `just network` for process, DNS, firewall, proxy, or VPN
changes. Container/network tests need a Linux Docker daemon with `/dev/net/tun`
and permission to add NET_ADMIN to disposable test containers.
Run `just web` for the browser suite. Its Playwright module and Chromium are
provided by the locked Nix shell on Linux; see `tests_web/README.md` for other
platforms.

Test observable behavior with the real binary and upstream processes. Preserve
termination/reaping, fail-closed tunnel behavior, queue pause intent, ingress
authentication, API limits, and credential redaction. Do not replace Gluetun's
firewall or implement another VPN protocol.

Never commit real options, credentials, keys, provider accounts, household
addresses, hostnames, or deployment history. Use generated credentials and
reserved `.example` names in fixtures. Keep local output under ignored `work/`.

SABnzbd dependencies are pinned from its upstream requirements in
`pyproject.toml` and resolved by `uv lock`. When updating, review upstream
requirements and security releases, refresh the source archive SHA256 and image
digests, and rerun both architecture builds. `NOTICE` describes bundled code.

Release versions must match Cargo, `pyproject.toml`, and `downloads/config.yaml`.
The release check also requires the tag to be `v` followed by that version.
The image workflow tags a multiarch image only after both native images pass
container and network checks. Run `just openapi` for the CLI-produced schema.
