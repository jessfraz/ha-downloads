# Downloads

SABnzbd downloads through a Gluetun VPN, with connection status and the SABnzbd
interface in Home Assistant's sidebar. The downloader waits for the VPN,
firewall, and DNS checks to pass. The queue starts paused on each app start.

The app uses NET_ADMIN, NET_RAW and `/dev/net/tun`. It does not use host networking,
the Docker socket, Home Assistant's API, or Supervisor management APIs. Media
is available under `/media`; private options and SABnzbd state live in `/data`.

## Install

Add `https://github.com/jessfraz/ha-downloads` as a Home Assistant app repository,
install **Downloads**, then configure the VPN in the app settings. Set the
provider and protocol, plus the provider's service username and password for
OpenVPN or its private key and addresses for WireGuard. Provider identifiers
are listed in [Gluetun’s provider guide](https://github.com/qdm12/gluetun-wiki/tree/main/setup/providers).
Open the sidebar panel
to configure SABnzbd and resume the queue when ready.

The app is experimental. Missing credentials leave the setup page available
and the downloader stopped. Verify the chosen provider, media paths, and tunnel
failure behavior before relying on a deployment.

See [app documentation](downloads/DOCS.md) for options, storage, and connecting
media managers. No indexer, Usenet account, or VPN subscription is included.

## Develop

```sh
nix develop
just check
just security
just openapi
just container
just network
```

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
