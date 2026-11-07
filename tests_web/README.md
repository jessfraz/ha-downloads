# Sidebar browser checks

The panel ships plain HTML, CSS, and JavaScript without runtime dependencies.
The tests run the shipped assets in Chromium at a nested Home Assistant ingress
path. A local Python fixture serves the assets and a native downloader page;
bounded HTTP fixtures exercise the owned media contract without VPN credentials,
provider accounts, or live downloads. Synthetic titles exist only in tests.

```sh
node --test tests_web/panel.mjs tests_web/media.mjs
```

`PLAYWRIGHT_MODULE` can select an existing Playwright installation.
`PLAYWRIGHT_CHROMIUM_EXECUTABLE` can select an existing Chromium binary.
`SCREENSHOT_DIR` writes synthetic browser screenshots outside the source tree.
Tests never launch the user's browser or modify Home Assistant settings.

`panel.mjs` covers existing connection setup, separate protection proofs, paused
queues, loss/recovery, malformed responses, ingress URLs, request timeouts,
polling lifecycle, keyboard access, responsive layouts, and native HA themes.

`media.mjs` covers movie requests using saved profiles, explicit TV season scope,
partial search coverage, unavailable queue sources, profile-conflict review,
uncertain writes and late acceptance, same-key retry across reload, local artwork,
mobile detail navigation, origin/focus restoration, source-row freshness, blocked
readiness explanations, monotonic expiry under wall-clock skew and cached
evidence, lifecycle-interrupted writes, and administrator profile setup.

A request-key lookup returning 404 after a lost submission response does not
prove the original handler cannot accept later. The UI retains the original
key and exact input; only an explicit retry can submit that same operation.
