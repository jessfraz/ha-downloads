# Panel browser checks

The panel ships plain HTML, CSS, and JavaScript with no runtime dependencies.
These tests run the actual assets in Chromium at a nested Home Assistant
ingress path. A local Python fixture serves assets and a small queue page;
Playwright supplies status responses without requiring a VPN or credentials.

With Node.js, Python 3, and Playwright available as a development tool:

```sh
node --test tests_web/panel.mjs
```

`PLAYWRIGHT_MODULE` can point to an existing Playwright module installation.
`PLAYWRIGHT_CHROMIUM_EXECUTABLE` can select an existing Chromium binary.
Set `SCREENSHOT_DIR` to a directory outside the source tree for visual review.
No tests launch the user's browser or change Home Assistant settings.

Coverage includes setup, independent connection proofs, paused queues,
connection loss, recovery, malformed status, safe text and ingress URLs,
request timeouts, polling lifecycle, responsive layout, keyboard access, and
Home Assistant theme changes independent of the system theme.
