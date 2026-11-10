import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir } from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
const { chromium } = createRequire(import.meta.url)(process.env.PLAYWRIGHT_MODULE || "playwright");
const directory = dirname(fileURLToPath(import.meta.url));
let server, browser, base;
const movie = () => ({ kind: "movie", tmdb_id: 14337, title: "Primer", year: 2004, overview: "Two engineers discover that their invention has unexpected consequences.", poster: "api/v1/artwork/movie/14337/poster", library: { state: "not_added" }, request: { state: "ready" }, acquisition: { state: "not_requested" } });
const series = () => ({ kind: "series", tvdb_id: 123456, title: "Severance", year: 2022, overview: "A team discovers the consequences of separating work from the rest of life.", poster: null, library: { state: "managed", upstream_id: 8 }, request: { state: "ready" }, acquisition: { state: "unknown" }, seasons: [{ number: 1, episode_count: 9, imported_count: 9, active_count: 0, monitored: true, state: "imported" }, { number: 2, episode_count: 10, imported_count: 0, active_count: 0, monitored: false, state: "missing" }] });
const coverage = () => ["radarr", "sonarr"].map((source) => ({ source, state: "available", observed_at_unix_ms: Date.now() }));
const pageOf = (items, sources = coverage()) => ({ items, next_page: null, coverage: sources });
const healthy = (operation = "transfer") => ({ generated_at_unix_ms: Date.now(), components: ["radarr", "sonarr", "sabnzbd", "vpn", "firewall", "dns", "storage", "plex"].map((component) => ({ component, observation: { state: "observed", freshness: "fresh", observed_at_unix_ms: Date.now(), expires_at_unix_ms: Date.now() + 60000, outcome: component === "plex" ? { state: "not_configured" } : { state: "healthy" } } })), readiness: [{ operation, state: "ready", reasons: [], expires_at_unix_ms: Date.now() + 60000 }] });
const download = (item, overrides = {}) => ({
  id: item.id, manager: item.media.kind === "movie" ? "radarr" : "sonarr", media: item.media,
  title: `${item.title}.2160p.REMUX`, episode: item.episode || null, acquisition: item.acquisition,
  status: item.acquisition.state === "downloading" ? "downloading" : "queued",
  quality: { name: "Bluray-2160p", resolution: 2160, source: "bluray", modifier: "remux", revision: { version: 1, real: 0, is_repack: false } },
  progress: item.acquisition.state === "downloading" ? { completed_bytes: item.acquisition.completed_bytes, total_bytes: item.acquisition.total_bytes, remaining_bytes: item.acquisition.total_bytes - item.acquisition.completed_bytes } : null,
  remaining_seconds: 120, download_client: "SABnzbd", indexer: "Example indexer", protocol: "usenet",
  languages: ["English"], custom_formats: ["Original language"], custom_format_score: 20,
  added_at: "2026-11-07T12:00:00Z", estimated_completion_at: "2026-11-07T12:02:00Z",
  import_status: "not_started", warnings: [], observed_at_unix_ms: Date.now(), ...overrides,
});
const imported = (overrides = {}) => ({ id: "radarr:31", manager: "radarr", media: { kind: "movie", tmdb_id: 14337 }, title: "Primer", episode: null, imported_at: "2026-11-07T12:03:00Z", quality: { name: "Bluray-2160p", resolution: 2160, source: "bluray", modifier: "remux", revision: null }, plex: { state: "unavailable", reason: "authentication" }, subtitles: { state: "unknown", reason: "plex_unverified" }, observed_at_unix_ms: Date.now(), ...overrides });
const receipt = (input, submission = { state: "applied", result: { effect: "added", media: { kind: input.kind, ...(input.kind === "movie" ? { tmdb_id: input.tmdb_id } : { tvdb_id: input.tvdb_id }) }, manager: input.kind === "movie" ? "radarr" : "sonarr", upstream_id: 9, search: { state: "queued", command_id: 21 } } }) => ({ id: "159b2a58-2261-45a2-82ed-1ff4f6cc0fc1", created_at_unix_ms: Date.now(), input, submission });
before(async () => {
  server = spawn(process.env.PYTHON || "python3", [resolve(directory, "fixture.py")], { stdio: ["ignore", "pipe", "inherit"] });
  const lines = createInterface({ input: server.stdout });
  const [port] = await once(lines, "line"); base = `http://127.0.0.1:${Number(port)}/api/hassio_ingress/browser-test/`;
  browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE } : {}) });
});
after(async () => { await browser?.close(); if (server && server.exitCode === null) { server.kill("SIGTERM"); await once(server, "exit"); } });
async function panel(t, options = {}) {
  const context = await browser.newContext({ viewport: options.viewport || { width: 1100, height: 900 }, colorScheme: options.colorScheme || "dark" }); t.after(() => context.close());
  const page = await context.newPage(); page.setDefaultTimeout(8000); await page.clock.install();
  const fixture = { posts: [], readKeys: [], errors: [], apiCalls: [], downloads: new Map(), downloadFailure: null, onDownload: null, searchCoverage: coverage(), queueCoverage: coverage(), importsCoverage: coverage(), imports: options.imports || [], importsNext: null, importsFailure: null, onImports: null, queue: options.queue || [], requests: options.requests || [], onPost: null, onKey: null, onHealth: null, systemFailure: options.systemFailure ?? false, health: null, search: [movie(), series()], requestPermission: options.requestPermission ?? true, adminPermission: options.adminPermission ?? true, artworkPaths: [] };
  page.on("pageerror", (error) => fixture.errors.push(error.message));
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url()), path = url.pathname.split("api/v1/")[1], method = route.request().method();
    fixture.apiCalls.push({ path, method });
    if (path.startsWith("artwork/")) { fixture.artworkPaths.push(url.pathname); return route.fulfill({ status: 404, body: "" }); }
    if (path === "requests" && method === "POST") {
      const input = route.request().postDataJSON(), key = route.request().headers()["idempotency-key"];
      fixture.posts.push({ input, key }); if (fixture.onPost) return fixture.onPost(route, input, key);
      const result = receipt(input); fixture.requests.push(result); return route.fulfill({ status: 202, json: result });
    }
    if (path.startsWith("request-keys/")) { fixture.readKeys.push(path.split("/")[1]); if (fixture.onKey) return fixture.onKey(route); return route.fulfill({ status: 404, json: { error_code: "not_found", message: "Not found" } }); }
    if (path.startsWith("queue/")) {
      const id = decodeURIComponent(path.slice("queue/".length));
      if (fixture.onDownload) return fixture.onDownload(route, id);
      if (fixture.downloadFailure) return route.fulfill({ status: fixture.downloadFailure, json: { error_code: "upstream_unavailable", message: "Unavailable" } });
      const item = fixture.queue.find((queue) => queue.id === id);
      const value = fixture.downloads.get(id) || (item && download(item));
      return value ? route.fulfill({ json: value }) : route.fulfill({ status: 404, json: { error_code: "not_found", message: "Not found" } });
    }
    if (path === "imports" && fixture.onImports) return fixture.onImports(route, url);
    if (path === "imports" && fixture.importsFailure) return route.fulfill({ status: fixture.importsFailure, json: { error_code: "upstream_unavailable", message: "Unavailable" } });
    if (path === "health/stack" && fixture.onHealth) return fixture.onHealth(route, url);
    if (path === "system" && fixture.systemFailure) return route.fulfill({ status: 503, json: { error_code: "upstream_unavailable", message: "Unavailable" } });
    let value;
    if (path === "system") value = { version: "0.1.5", capabilities: { read: true, request: fixture.requestPermission, admin: fixture.adminPermission }, transport: "ingress", credential_role: fixture.adminPermission ? "administrator" : "requester" };
    else if (path === "status") value = { version: "0.1.5", state: "ready", message: "Protection checks passed.", vpn: { state: "connected", provider: "Fixture VPN", public_ip: null }, downloader: { state: "running", url: "sabnzbd/", paused: false }, protection: { kill_switch: "verified", dns: "verified" } };
    else if (path === "health/stack") value = fixture.health || healthy(url.searchParams.get("operation") || "transfer");
    else if (path === "search") value = pageOf(fixture.search.filter((m) => (url.searchParams.get("kind") === "all" || m.kind === url.searchParams.get("kind")) && m.title.toLowerCase().includes(url.searchParams.get("query").toLowerCase())), fixture.searchCoverage);
    else if (path === "movies") value = pageOf([movie()]);
    else if (path === "series") value = pageOf([series()]);
    else if (path === "movies/14337") value = fixture.movieDetail || movie();
    else if (path === "series/123456") value = fixture.seriesDetail || series();
    else if (path === "queue") value = pageOf(fixture.queue, fixture.queueCoverage);
    else if (path === "imports") value = { ...pageOf(fixture.imports, fixture.importsCoverage), next_page: fixture.importsNext };
    else if (path === "requests") value = pageOf(fixture.requests);
    else if (path.startsWith("requests/")) value = fixture.requests.find((r) => r.id === path.split("/")[1]);
    if (value === undefined) return route.fulfill({ status: 404, json: { error_code: "not_found", message: "Not found" } });
    return route.fulfill({ json: value });
  });
  await page.goto(base + (options.embedded ? "theme-host/?panel=downloads" : "") + (options.hash || ""));
  const app = options.embedded ? page.frameLocator("iframe") : page;
  await app.locator("#stack-health-label").filter({ hasText: "Download stack healthy" }).waitFor({ state: "attached" }); return { page, fixture, app };
}
async function closeDetails(page) {
  if (await page.locator("#media-dialog").count() && await page.locator("#media-dialog").evaluate((element) => element.open)) {
    await page.getByRole("button", { name: "Close details", exact: true }).click();
    await page.locator("#media-dialog").waitFor({ state: "hidden" });
  }
}
async function refreshDetails(page) {
  if (await page.locator("#media-dialog").evaluate((element) => element.open)) await page.getByRole("button", { name: "Refresh details", exact: true }).click();
  else await page.getByRole("button", { name: "Refresh status", exact: true }).click();
}
async function find(page, query, title) {
  await closeDetails(page);
  await page.getByRole("searchbox", { name: "Search movies and TV" }).fill(query); await page.clock.runFor(301);
  await page.getByRole("button", { name: title }).click();
}
async function capture(page, name) {
  if (!process.env.SCREENSHOT_DIR) return;
  await mkdir(process.env.SCREENSHOT_DIR, { recursive: true }); await page.screenshot({ path: resolve(process.env.SCREENSHOT_DIR, name), fullPage: true });
}
test("search requests a movie through the typed API using parent-app settings and local artwork", async (t) => {
  const { page, fixture } = await panel(t);
  await find(page, "Primer", "Primer, 2004, movie");
  await page.getByRole("button", { name: "Request movie", exact: true }).waitFor();
  assert.match(await page.locator("#media-detail").innerText(), /Uses Radarr settings/);
  assert.equal(await page.locator("#request-readiness-note").isVisible(), false);
  await page.getByRole("button", { name: "Request movie", exact: true }).click();
  await page.locator("#media-detail").getByText(/Request accepted and search queued/).waitFor();
  assert.equal(fixture.posts.length, 1); assert.match(fixture.posts[0].key, /^[a-f0-9-]{36}$/);
  assert.deepEqual(fixture.posts[0].input, { kind: "movie", tmdb_id: 14337, intent: { mode: "search_now" } });
  assert.ok(fixture.artworkPaths.every((path) => path.startsWith("/api/hassio_ingress/browser-test/api/v1/artwork/")));
  assert.match(await page.locator("#media-detail .media-detail-meta").innerText(), /Request accepted/);
  assert.doesNotMatch(await page.locator("#media-detail .media-detail-meta").innerText(), /Not requested/);
  assert.deepEqual(fixture.errors, []);
});
test("TV requires explicit seasons, preserves imported seasons, and submits no unrelated settings", async (t) => {
  const { page, fixture } = await panel(t);
  await find(page, "Severance", "Severance, 2022, TV show");
  const request = page.getByRole("button", { name: "Get selected seasons", exact: true });
  assert.equal(await request.isDisabled(), true);
  assert.equal(await page.getByRole("checkbox", { name: /Season 1/ }).isDisabled(), true);
  assert.match(await page.locator("#media-detail").innerText(), /Uses Sonarr settings/);
  await page.getByRole("checkbox", { name: /Season 2/ }).check();
  await request.click(); await page.locator("#media-detail").getByText(/Request accepted and search queued/).waitFor();
  assert.deepEqual(fixture.posts[0].input, { kind: "series", tvdb_id: 123456, intent: { mode: "search_now", scope: { kind: "seasons", numbers: [2] } } });
  assert.deepEqual(fixture.errors, []);
});
test("partial search and stale activity remain distinct from empty results", async (t) => {
  const { page, fixture } = await panel(t);
  fixture.searchCoverage = coverage().map((c) => c.source === "sonarr" ? { ...c, state: "unavailable" } : c);
  await page.getByRole("searchbox", { name: "Search movies and TV" }).fill("Primer"); await page.clock.runFor(301);
  await page.getByText(/Sonarr is unavailable/).waitFor();
  fixture.queue = [{ id: "radarr:1", media: { kind: "movie", tmdb_id: 14337 }, title: "Primer", episode: null, acquisition: { state: "downloading", completed_bytes: 40, total_bytes: 100 } }];
  await page.getByRole("tab", { name: /Activity/ }).click(); await page.getByRole("progressbar").waitFor();
  fixture.queue = []; fixture.queueCoverage = coverage().map((c) => ({ ...c, state: "unavailable" }));
  await page.clock.runFor(5001); await page.locator("#activity-notice").getByText(/Results may be incomplete/).waitFor();
  assert.match(await page.locator("#activity-list").innerText(), /Primer/);
  assert.match(await page.locator("#activity-list").innerText(), /Last seen: Downloading · Radarr unavailable/);
  assert.equal(await page.getByRole("progressbar").getAttribute("aria-valuenow"), "40");
  assert.deepEqual(fixture.errors, []);
});
test("404 after a lost response retains the original key and recovers late acceptance without reposting", async (t) => {
  const { page, fixture } = await panel(t);
  let originalInput;
  fixture.onPost = async (route, input) => { originalInput = input; await route.abort("connectionfailed"); };
  await find(page, "Primer", "Primer, 2004, movie"); await page.getByRole("button", { name: "Request movie", exact: true }).click();
  await page.getByRole("button", { name: "Retry original request", exact: true }).waitFor();
  assert.equal(fixture.posts.length, 1); assert.equal(fixture.readKeys[0], fixture.posts[0].key);
  const originalKey = fixture.posts[0].key;
  fixture.onKey = async (route) => route.fulfill({ json: receipt(originalInput) });
  await page.getByRole("button", { name: "Check request status", exact: true }).click();
  await page.locator("#media-detail").getByText(/Request accepted and search queued/).waitFor();
  assert.equal(fixture.posts.length, 1); assert.equal(fixture.readKeys.at(-1), originalKey);
});
test("explicit retry after404 reuses the exact original key and body, including across reload", async (t) => {
  const { page, fixture } = await panel(t);
  fixture.onPost = async (route) => route.abort("connectionfailed");
  await find(page, "Primer", "Primer, 2004, movie"); await page.getByRole("button", { name: "Request movie", exact: true }).click();
  await page.getByRole("button", { name: "Retry original request", exact: true }).waitFor();
  const original = fixture.posts[0]; await page.reload();
  await closeDetails(page);
  await page.getByRole("tab", { name: /Activity/ }).click();
  await page.getByRole("button", { name: "Retry original request", exact: true }).waitFor();
  fixture.onPost = null;
  await page.getByRole("button", { name: "Retry original request", exact: true }).click();
  await page.locator("#activity-list").getByText(/Request accepted and search queued/).waitFor();
  assert.equal(fixture.posts.length, 2); assert.equal(fixture.posts[1].key, original.key); assert.deepEqual(fixture.posts[1].input, original.input);
});
test("mobile detail modal, theme, keyboard tabs, and true imported labels remain usable", async (t) => {
  const { page, fixture } = await panel(t, { viewport: { width: 390, height: 850 } });
  await find(page, "Primer", "Primer, 2004, movie");
  assert.equal(await page.getByRole("dialog").isVisible(), true);
  assert.equal(await page.locator("#search-tab").getAttribute("aria-selected"), "true");
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await capture(page, "downloads-media-mobile-detail.png");
  await closeDetails(page);
  await page.getByRole("tab", { name: "Search", exact: true }).focus(); await page.keyboard.press("ArrowRight");
  await page.getByRole("heading", { name: "Your library" }).waitFor();
  await page.setViewportSize({ width: 320, height: 850 }); assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await page.setViewportSize({ width: 1100, height: 900 }); await page.getByRole("tab", { name: "Search", exact: true }).click();
  await page.getByRole("button", { name: "Primer, 2004, movie" }).click();
  await page.getByRole("button", { name: "Request movie", exact: true }).waitFor();
  await capture(page, "downloads-media-desktop-dark.png");
  await page.emulateMedia({ colorScheme: "light" });
  await page.waitForFunction(() => getComputedStyle(document.getElementById("request-media")).color === "rgb(0, 0, 0)");
  await capture(page, "downloads-media-desktop-light.png");
  assert.deepEqual(fixture.errors, []);
});
test("requester and read-only accounts have no parent-app settings or embedded downloader", async (t) => {
  const { page, fixture } = await panel(t, { adminPermission: false, requestPermission: false });
  assert.equal(await page.locator("iframe").count(), 0);
  await page.getByRole("button", { name: "Download stack healthy" }).click();
  assert.equal(await page.getByText("Advanced downloader settings", { exact: true }).count(), 0);
  assert.equal(await page.getByText("Saved download profiles", { exact: true }).count(), 0);
  await find(page, "Primer", "Primer, 2004, movie");
  await page.getByText("This account has read-only access.", { exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Request movie", exact: true }).isDisabled(), true);
  assert.equal(fixture.posts.length, 0);
  assert.equal(await page.locator("iframe").count(), 0);
});

test("cached readiness expires locally even while successful responses remain cached", async (t) => {
  const { page, fixture } = await panel(t);
  const snapshot = healthy(); snapshot.readiness[0].expires_at_unix_ms = Date.now() + 100;
  fixture.health = snapshot;
  await refreshDetails(page);
  await page.clock.runFor(1100);
  await page.getByRole("button", { name: "Download stack status stale", exact: true }).waitFor();
});

test("target readiness copy and action expire together without rebuilding the detail", async (t) => {
  const { page, fixture } = await panel(t);
  await find(page, "Primer", "Primer, 2004, movie");
  await page.locator("#request-readiness-note").waitFor({ state: "hidden" });
  await page.waitForFunction(() => document.getElementById("request-media")?.disabled === false);
  const now = await page.evaluate(() => Date.now());
  fixture.health = healthy("request_movie");
  fixture.health.readiness[0].expires_at_unix_ms = now + 500;
  const scoped = page.waitForResponse((response) => response.url().includes("health/stack?") && response.url().includes("operation="));
  await refreshDetails(page); await scoped;
  await page.locator("#request-mode").focus();
  await page.clock.runFor(1100);
  assert.equal(await page.getByRole("button", { name: "Request movie", exact: true }).isDisabled(), true);
  await page.locator("#request-readiness-note").waitFor({ state: "visible" });
  assert.match(await page.locator("#request-readiness-note").innerText(), /not verified/);
  assert.equal(await page.locator("#request-mode").evaluate((element) => element === document.activeElement), true);
});

test("a rejected retry cannot erase uncertainty about the original detached submission", async (t) => {
  const { page, fixture } = await panel(t);
  fixture.onPost = async (route) => route.abort("connectionfailed");
  await find(page, "Primer", "Primer, 2004, movie"); await page.getByRole("button", { name: "Request movie", exact: true }).click();
  await page.getByRole("button", { name: "Retry original request", exact: true }).waitFor();
  const original = fixture.posts[0];
  fixture.onPost = async (route) => route.fulfill({ status: 401, json: { error_code: "authentication_required", message: "Authentication required" } });
  await page.getByRole("button", { name: "Retry original request", exact: true }).click();
  await page.locator("#media-detail").getByText(/original key and selection were kept/).waitFor();
  assert.equal(await page.getByRole("button", { name: "Outcome unverified", exact: true }).isDisabled(), true);
  assert.equal(fixture.posts[1].key, original.key);
  fixture.onKey = async (route) => route.fulfill({ json: receipt(original.input) });
  await page.getByRole("button", { name: "Check request status", exact: true }).click();
  await page.locator("#media-detail").getByText(/Request accepted and search queued/).waitFor();
  assert.equal(fixture.posts.length, 2);
});

test("pagehide during an original POST retains uncertainty through404 and a rejected retry", async (t) => {
  const { page, fixture } = await panel(t);
  let releaseOriginal;
  let postStarted;
  const started = new Promise((resolveStarted) => { postStarted = resolveStarted; });
  fixture.onPost = async (route) => { postStarted(); await new Promise((resolveOriginal) => { releaseOriginal = resolveOriginal; }); await route.abort("connectionfailed").catch(() => {}); };
  await find(page, "Primer", "Primer, 2004, movie");
  await page.getByRole("button", { name: "Request movie", exact: true }).click(); await started;
  const failed = page.waitForEvent("requestfailed", (request) => request.method() === "POST" && request.url().endsWith("/requests"));
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent("pagehide", { persisted: true })));
  await failed; releaseOriginal();
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent("pageshow", { persisted: true })));
  await page.getByRole("button", { name: "Retry original request", exact: true }).waitFor();
  const original = fixture.posts[0];
  fixture.onPost = async (route) => route.fulfill({ status: 401, json: { error_code: "authentication_required", message: "Sign in" } });
  await page.getByRole("button", { name: "Retry original request", exact: true }).click();
  await page.locator("#media-detail").getByText(/original key and selection were kept/).waitFor();
  assert.equal(fixture.posts[1].key, original.key);
  assert.equal(await page.getByRole("button", { name: "Outcome unverified", exact: true }).isDisabled(), true);
  fixture.onKey = async (route) => route.fulfill({ json: receipt(original.input) });
  await page.getByRole("button", { name: "Check request status", exact: true }).click();
  await page.locator("#media-detail").getByText(/Request accepted and search queued/).waitFor();
});

test("a known acceptance receipt releases other titles while retaining the same-selection guard", async (t) => {
  const { page, fixture } = await panel(t);
  fixture.onPost = async (route, input) => {
    const record = receipt(input, { state: "reconciling", reason: "dependency_unavailable" });
    if (input.kind === "series") record.id = "cb1a7813-bbf2-4b73-b657-a8202e5ae59f";
    fixture.requests.push(record); await route.fulfill({ status: 202, json: record });
  };
  await find(page, "Primer", "Primer, 2004, movie"); await page.getByRole("button", { name: "Request movie", exact: true }).click();
  await page.locator("#media-detail").getByText(/Checking the upstream outcome/).waitFor();
  assert.equal(await page.getByRole("button", { name: "Request accepted", exact: true }).isDisabled(), true);
  assert.equal(await page.locator("#unknown-request-notice").isVisible(), false);
  await find(page, "Severance", "Severance, 2022, TV show");
  await page.getByRole("checkbox", { name: /Season 2/ }).check();
  await page.getByRole("button", { name: "Get selected seasons", exact: true }).click();
  await page.locator("#media-detail").getByText(/Checking the upstream outcome/).waitFor();
  assert.equal(fixture.posts.length, 2); assert.equal(fixture.posts[1].input.kind, "series");
});

test("a truly unverified original explains its cross-title block without offering an unsafe discard", async (t) => {
  const { page, fixture } = await panel(t);
  fixture.onPost = async (route) => route.abort("connectionfailed");
  await find(page, "Primer", "Primer, 2004, movie"); await page.getByRole("button", { name: "Request movie", exact: true }).click();
  await page.getByRole("button", { name: "Retry original request", exact: true }).waitFor();
  await find(page, "Severance", "Severance, 2022, TV show");
  assert.match(await page.locator("#unknown-request-notice").innerText(), /Primer.*unverified.*paused/s);
  assert.equal(await page.getByRole("button", { name: "Outcome unverified", exact: true }).isDisabled(), true);
  assert.equal(await page.getByRole("button", { name: /Stop tracking|Discard request/i }).count(), 0);
  assert.equal(fixture.posts.length, 1);
});

test("activity polling preserves focused actions for unchanged and changed progress", async (t) => {
  const { page, fixture } = await panel(t);
  fixture.queue = [{ id: "radarr:1", media: { kind: "movie", tmdb_id: 14337 }, title: "Primer", episode: null, acquisition: { state: "downloading", completed_bytes: 40, total_bytes: 100 } }];
  await page.getByRole("tab", { name: /Activity/ }).click();
  const action = page.locator("#activity-queue-radarr-1-details"); await action.focus();
  await page.clock.runFor(5001);
  assert.equal(await action.evaluate((element) => element === document.activeElement), true);
  fixture.queue[0].acquisition.completed_bytes = 50;
  await page.clock.runFor(5001);
  await page.waitForFunction(() => document.querySelector("#activity-list [role='progressbar']")?.getAttribute("aria-valuenow") === "50");
  assert.equal(await action.evaluate((element) => element === document.activeElement), true);
});

test("recovery polling keeps its focused check action and selection moves desktop focus into detail", async (t) => {
  const { page, fixture } = await panel(t);
  await find(page, "Primer", "Primer, 2004, movie");
  // Selection renders a loading title, then replaces it when details arrive.
  // Wait for the completed selection and inspect the current DOM atomically.
  await page.getByRole("button", { name: "Request movie", exact: true }).waitFor();
  assert.equal(await page.evaluate(() => document.getElementById("media-detail-title") === document.activeElement), true);
  fixture.onPost = async (route) => route.abort("connectionfailed");
  await page.getByRole("button", { name: "Request movie", exact: true }).click();
  const check = page.locator("#detail-check-request"); await check.focus();
  await page.clock.runFor(5001);
  assert.equal(await check.evaluate((element) => element === document.activeElement), true);
});

test("blocked target readiness explains its typed reasons", async (t) => {
  const { page, fixture } = await panel(t);
  fixture.onHealth = async (route, url) => {
    const snapshot = healthy(url.searchParams.get("operation") || "transfer");
    if (url.searchParams.has("operation")) { snapshot.readiness[0].state = "blocked"; snapshot.readiness[0].reasons = ["storage_unavailable", "protection_unverified"]; }
    await route.fulfill({ json: snapshot });
  };
  await find(page, "Primer", "Primer, 2004, movie");
  await page.locator("#request-readiness-note").getByText(/selected library storage is unavailable/).waitFor();
  assert.match(await page.locator("#request-readiness-note").innerText(), /VPN protection is not verified/);
  assert.equal(await page.getByRole("button", { name: "Request movie", exact: true }).isDisabled(), true);
});

test("a newer blocked health snapshot can have an earlier readiness expiry", async (t) => {
  const { page, fixture } = await panel(t);
  const initial = healthy("request_movie");
  fixture.onHealth = async (route, url) => route.fulfill({ json: url.searchParams.has("operation") ? initial : healthy() });
  await find(page, "Primer", "Primer, 2004, movie");
  await page.waitForFunction(() => document.getElementById("request-media")?.disabled === false);
  const blocked = structuredClone(initial);
  blocked.generated_at_unix_ms += 100;
  blocked.readiness[0].state = "blocked";
  blocked.readiness[0].reasons = ["storage_unavailable"];
  blocked.readiness[0].expires_at_unix_ms -= 30000;
  fixture.onHealth = async (route, url) => route.fulfill({ json: url.searchParams.has("operation") ? blocked : healthy() });
  await refreshDetails(page);
  await page.locator("#request-readiness-note").getByText(/selected library storage is unavailable/).waitFor();
  assert.equal(await page.getByRole("button", { name: "Request movie", exact: true }).isDisabled(), true);
});

test("wall-clock skew and repeated identical evidence cannot extend a readiness deadline", async (t) => {
  const { page, fixture } = await panel(t);
  const frozen = healthy("request_movie"); frozen.readiness[0].expires_at_unix_ms = frozen.generated_at_unix_ms + 1500;
  fixture.health = frozen;
  await find(page, "Primer", "Primer, 2004, movie");
  await page.waitForFunction(() => document.getElementById("request-media")?.disabled === false);
  const wall = await page.evaluate(() => Date.now());
  await page.clock.setSystemTime(new Date(wall + 86400000));
  assert.equal(await page.getByRole("button", { name: "Request movie", exact: true }).isEnabled(), true);
  await page.clock.setSystemTime(new Date(wall - 86400000));
  await page.clock.runFor(600);
  const refreshed = page.waitForResponse((response) => response.url().includes("health/stack?") && response.url().includes("operation="));
  await refreshDetails(page); await refreshed;
  await page.clock.runFor(1000);
  assert.equal(await page.getByRole("button", { name: "Request movie", exact: true }).isDisabled(), true);
  assert.equal(await page.locator("#request-readiness-note").isVisible(), true);
});

test("response latency is subtracted from the conservative readiness lifetime", async (t) => {
  const { page, fixture } = await panel(t);
  let releaseScope, scopeStarted;
  const started = new Promise((resolveStarted) => { scopeStarted = resolveStarted; });
  fixture.onHealth = async (route, url) => {
    const snapshot = healthy(url.searchParams.get("operation") || "transfer");
    if (url.searchParams.has("operation")) { snapshot.readiness[0].expires_at_unix_ms = snapshot.generated_at_unix_ms + 1500; scopeStarted(); await new Promise((release) => { releaseScope = release; }); }
    await route.fulfill({ json: snapshot });
  };
  await find(page, "Primer", "Primer, 2004, movie"); await started;
  await page.clock.runFor(700); releaseScope();
  await page.waitForFunction(() => document.getElementById("request-media")?.disabled === false);
  await page.clock.runFor(900);
  assert.equal(await page.getByRole("button", { name: "Request movie", exact: true }).isDisabled(), true);
});

test("mobile modal details stay over Library or Activity and restore the originating action", async (t) => {
  const { page, fixture } = await panel(t, { viewport: { width: 390, height: 850 } });
  await page.getByRole("tab", { name: "Library", exact: true }).click();
  const libraryAction = page.locator("#library-movie-14337-details"); await libraryAction.click();
  assert.equal(await page.locator("#library-tab").getAttribute("aria-selected"), "true");
  await closeDetails(page);
  assert.equal(await page.getByRole("tab", { name: "Library", exact: true }).getAttribute("aria-selected"), "true");
  assert.equal(await libraryAction.evaluate((element) => element === document.activeElement), true);
  fixture.queue = [{ id: "radarr:1", media: { kind: "movie", tmdb_id: 14337 }, title: "Primer", episode: null, acquisition: { state: "waiting" } }];
  await page.getByRole("tab", { name: /Activity/ }).click();
  const activityAction = page.locator("#activity-queue-radarr-1-details"); await activityAction.click();
  assert.equal(await page.locator("#activity-tab").getAttribute("aria-selected"), "true");
  await closeDetails(page);
  assert.equal(await activityAction.evaluate((element) => element === document.activeElement), true);
});

test("a failed capability read says unverified, not read-only", async (t) => {
  const { page } = await panel(t, { systemFailure: true });
  await find(page, "Primer", "Primer, 2004, movie");
  await page.getByText("Request access could not be verified.", { exact: true }).waitFor();
  assert.equal(await page.getByText("This account has read-only access.", { exact: true }).count(), 0);
  assert.equal(await page.getByRole("button", { name: "Request movie", exact: true }).isDisabled(), true);
});


test("settings stay in parent apps for administrators without catalog/options API calls", async (t) => {
  const { page, fixture } = await panel(t);
  await page.getByRole("button", { name: "Download stack healthy", exact: true }).click();
  assert.equal(await page.getByText("Saved download profiles", { exact: true }).count(), 0);
  assert.equal(await page.getByText("Advanced downloader settings", { exact: true }).count(), 0);
  assert.equal(await page.getByRole("link", { name: /Open downloads/ }).count(), 0);
  assert.equal(await page.locator("iframe").count(), 0);
  await find(page, "Primer", "Primer, 2004, movie");
  await page.getByRole("button", { name: "Request movie", exact: true }).waitFor();
  assert.equal(await page.getByRole("combobox").count(), 1);
  assert.equal(await page.getByRole("combobox", { name: "Request behavior", exact: true }).isVisible(), true);
  assert.match(await page.locator("#media-detail").innerText(), /Uses Radarr settings/);
  assert.equal(fixture.apiCalls.some(({ path }) => path.startsWith("profiles") || path.startsWith("managers/")), false);
  assert.deepEqual(fixture.errors, []);
});

test("missing native defaults explains the parent-app setup without exposing settings", async (t) => {
  const { page, fixture } = await panel(t);
  fixture.onHealth = async (route, url) => {
    const snapshot = healthy(url.searchParams.get("operation") || "transfer");
    if (url.searchParams.has("operation")) { snapshot.readiness[0].state = "blocked"; snapshot.readiness[0].reasons = ["not_configured"]; }
    await route.fulfill({ json: snapshot });
  };
  await find(page, "Primer", "Primer, 2004, movie");
  await page.locator("#request-readiness-note").getByText(/Finish quality and library setup in Radarr or Sonarr/).waitFor();
  assert.equal(await page.getByRole("button", { name: "Request movie", exact: true }).isDisabled(), true);
  assert.equal(fixture.posts.length, 0);
  assert.equal(fixture.apiCalls.some(({ path }) => path.startsWith("profiles") || path.startsWith("managers/")), false);
});

test("server-owned safe health messages explain an update warning as text", async (t) => {
  const { page, fixture } = await panel(t);
  const snapshot = healthy();
  snapshot.components.push({ component: "prowlarr", observation: { state: "observed", freshness: "fresh", observed_at_unix_ms: snapshot.generated_at_unix_ms, expires_at_unix_ms: snapshot.generated_at_unix_ms + 60000, outcome: { state: "degraded", issues: [{ code: "upstream_health", severity: "warning", message: "Prowlarr could not check for updates. <b>Indexers remain available.</b>" }] } } });
  fixture.health = snapshot;
  await refreshDetails(page);
  await page.getByRole("button", { name: "Download stack healthy", exact: true }).click();
  await page.locator("#stack-health-components").getByText("Prowlarr could not check for updates. <b>Indexers remain available.</b>", { exact: true }).waitFor();
  assert.equal(await page.locator("#stack-health-components b").count(), 0);
  assert.equal(fixture.posts.length, 0);
});

test("an unverified request from the previous app version keeps its receipt key without reposting a changed body", async (t) => {
  const { page, fixture } = await panel(t);
  const key = "ba90478e-67aa-4fbc-944a-48928e841098";
  const input = { kind: "movie", tmdb_id: 14337, profile: { id: "276319ec-c797-460c-a49e-6b90fc1f6ae8", revision: 4 }, intent: { mode: "search_now" } };
  // Persisted input from 0.2.0 is a compatibility boundary, not a new request.
  await page.evaluate(({ key, input }) => sessionStorage.setItem(`downloads:pending:${new URL(document.baseURI).pathname}`, JSON.stringify({ key, input, title: "Primer", uncertain: true, allowRetry: true })), { key, input });
  await page.reload();
  await page.getByRole("tab", { name: /Activity/ }).click();
  await page.locator("#activity-list").getByText(/Only its receipt can be checked here/).waitFor();
  assert.equal(await page.getByRole("button", { name: "Retry original request", exact: true }).count(), 0);
  assert.equal(fixture.posts.length, 0);
  fixture.onKey = async (route) => route.fulfill({ json: receipt(input) });
  await page.getByRole("button", { name: "Check request status", exact: true }).click();
  await page.locator("#activity-list").getByText(/Request accepted and search queued/).waitFor();
  assert.equal(fixture.readKeys.at(-1), key);
  assert.equal(fixture.posts.length, 0);
  assert.equal(await page.locator("#unknown-request-notice").isVisible(), false);
});

for (const kind of ['movie', 'series']) {
  test(`${kind} parent setup blocks requesting despite healthy prerequisites until the native title is configured`, async (t) => {
    const { page, fixture } = await panel(t);
    const owner = kind === 'movie' ? 'Radarr' : 'Sonarr';
    const title = kind === 'movie' ? 'Primer' : 'Severance';
    const accessibleTitle = kind === 'movie' ? 'Primer, 2004, movie' : 'Severance, 2022, TV show';
    const blocked = { ...(kind === 'movie' ? movie() : series()), library: { state: 'not_added' }, request: { state: 'parent_setup_required' } };
    if (kind === 'series') blocked.seasons = blocked.seasons.map((season) => ({ ...season, imported_count: 0, active_count: 0, monitored: false, state: 'missing' }));
    fixture.search = [blocked];
    if (kind === 'movie') fixture.movieDetail = blocked; else fixture.seriesDetail = blocked;
    await find(page, title, accessibleTitle);
    await page.locator('#request-eligibility-note').getByText(`Add or configure this title in ${owner} first, then reopen it here. Settings stay in that app.`, { exact: true }).waitFor();
    await page.locator('#request-readiness-note').waitFor({ state: 'hidden' });
    if (kind === 'series') await page.getByRole('checkbox', { name: /Season 2/ }).check();
    assert.equal(await page.locator('#request-media').isDisabled(), true);
    await page.getByRole('combobox', { name: 'Request behavior', exact: true }).selectOption('monitor_only');
    assert.equal(await page.locator('#request-media').isDisabled(), true);
    assert.equal(fixture.posts.length, 0);
    assert.equal(fixture.apiCalls.some(({ path }) => path.startsWith('profiles') || path.startsWith('managers/')), false);
    // Simulate the owner app adding/configuring the title, then reopen it.
    const managed = { ...blocked, library: { state: 'managed', upstream_id: 8 }, request: { state: 'ready' } };
    if (kind === 'movie') fixture.movieDetail = managed; else fixture.seriesDetail = managed;
    await closeDetails(page);
    await page.getByRole('button', { name: accessibleTitle, exact: true }).click();
    if (kind === 'series') await page.getByRole('checkbox', { name: /Season 2/ }).check();
    await page.waitForFunction(() => document.getElementById('request-media')?.disabled === false);
    assert.equal(await page.locator('#request-eligibility-note').isVisible(), false);
    assert.match(await page.locator('#media-detail').innerText(), new RegExp(`Uses ${owner} settings`));
    assert.equal(fixture.posts.length, 0);
    assert.deepEqual(fixture.errors, []);
  });
}

test('an extra manager override in a title response is rejected instead of enabling a request', async (t) => {
  const { page, fixture } = await panel(t);
  fixture.movieDetail = { ...movie(), request: { state: 'parent_setup_required', manager: 'radarr' } };
  await find(page, 'Primer', 'Primer, 2004, movie');
  await page.locator('#media-detail').getByText(/Details could not be reached/).waitFor();
  assert.equal(await page.locator('#request-media').count(), 0);
  assert.equal(fixture.posts.length, 0);
});

test('Library details open a real modal over Library, keep rich imported context, and restore focus on Back and Forward', async (t) => {
  const { page, fixture } = await panel(t);
  fixture.movieDetail = { ...movie(), library: { state: 'managed', upstream_id: 9 }, acquisition: { state: 'imported' }, metadata: { runtime_minutes: 77, genres: ['Drama', 'Science Fiction'], studio: 'Example studio', certification: 'PG-13', file: { quality: { name: 'Bluray-2160p', resolution: 2160, source: 'bluray', modifier: 'remux', revision: null }, size_bytes: 8589934592, video_codec: 'HEVC', audio_codec: 'TrueHD', audio_channels: '5.1' } } };
  await page.getByRole('tab', { name: 'Library', exact: true }).click();
  const origin = page.locator('#library-movie-14337-details'); await origin.click();
  assert.equal(await page.locator('#library-tab').getAttribute('aria-selected'), 'true');
  await page.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  assert.equal(new URL(page.url()).hash, '#/library/title/movie/14337');
  const detail = page.locator('#media-detail');
  assert.match(await detail.innerText(), /77 min/); assert.match(await detail.innerText(), /Science Fiction/);
  assert.match(await detail.innerText(), /Example studio/); assert.match(await detail.innerText(), /Imported file/);
  assert.match(await detail.innerText(), /Bluray-2160p/); assert.match(await detail.innerText(), /8 GiB/);
  assert.match(await detail.innerText(), /HEVC/); assert.match(await detail.innerText(), /TrueHD/); assert.match(await detail.innerText(), /5\.1/);
  assert.equal(await page.locator('#request-mode, #request-media').count(), 0);
  await page.goBack(); await page.locator('#media-dialog').waitFor({ state: 'hidden' });
  assert.equal(new URL(page.url()).hash, '#/library');
  assert.equal(await origin.evaluate((element) => element === document.activeElement), true);
  await page.goForward(); await page.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  assert.equal(await page.locator('#library-tab').getAttribute('aria-selected'), 'true');
  assert.equal(fixture.posts.length, 0);
  await capture(page, 'downloads-library-imported-modal.png');
});

test('Activity details show native release, progress, source and import facts without switching Search or claiming imported', async (t) => {
  const item = { id: 'radarr:1', media: { kind: 'movie', tmdb_id: 14337 }, title: 'Primer', episode: null, acquisition: { state: 'importing' } };
  const { page, fixture } = await panel(t, { queue: [item] });
  fixture.movieDetail = { ...movie(), library: { state: 'managed', upstream_id: 9 }, acquisition: { state: 'importing' } };
  fixture.downloads.set(item.id, download(item, { status: 'completed', progress: { completed_bytes: 8589934592, total_bytes: 8589934592, remaining_bytes: 0 }, remaining_seconds: 0, import_status: 'pending', warnings: [{ code: 'import_blocked', severity: 'warning', message: 'Import is waiting for the native media manager.' }] }));
  await page.getByRole('tab', { name: /^Activity/ }).click();
  await page.locator('#activity-queue-radarr-1-details').click();
  assert.equal(await page.locator('#activity-tab').getAttribute('aria-selected'), 'true');
  await page.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  assert.equal(new URL(page.url()).hash, '#/activity/download/radarr%3A1');
  const detail = page.locator('#media-detail');
  assert.match(await detail.innerText(), /Primer\.2160p\.REMUX/); assert.match(await detail.innerText(), /Transfer complete/);
  assert.match(await detail.innerText(), /Waiting to import/); assert.match(await detail.innerText(), /8 GiB of 8 GiB/);
  assert.match(await detail.innerText(), /Bluray-2160p/); assert.match(await detail.innerText(), /SABnzbd/);
  assert.match(await detail.innerText(), /Example indexer/); assert.match(await detail.innerText(), /Usenet/);
  assert.match(await detail.innerText(), /Original language/); assert.match(await detail.innerText(), /Import is waiting/);
  assert.doesNotMatch(await detail.innerText(), /Imported into library/);
  assert.equal(await page.locator('#request-media').count(), 0);
  assert.equal(fixture.posts.length, 0);
  await capture(page, 'downloads-activity-download-modal.png');
});

test('URL restores query, filter and open title across reload, Back and Forward without polling history entries', async (t) => {
  const { page, fixture } = await panel(t);
  await page.getByRole('button', { name: 'Movies', exact: true }).click();
  await find(page, 'Primer', 'Primer, 2004, movie');
  await page.getByRole('button', { name: 'Request movie', exact: true }).waitFor();
  const hash = '#/search/title/movie/14337?q=Primer&kind=movie';
  assert.equal(new URL(page.url()).hash, hash);
  const length = await page.evaluate(() => history.length);
  await page.clock.runFor(15001); await refreshDetails(page);
  assert.equal(await page.evaluate(() => history.length), length);
  assert.equal(new URL(page.url()).hash, hash);
  assert.equal(fixture.posts.length, 0);
  await page.reload(); await page.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  assert.equal(new URL(page.url()).hash, hash);
  await page.goBack(); await page.locator('#media-dialog').waitFor({ state: 'hidden' });
  assert.equal(new URL(page.url()).hash, '#/search?q=Primer&kind=movie');
  assert.equal(await page.getByRole('searchbox', { name: 'Search movies and TV' }).inputValue(), 'Primer');
  assert.equal(await page.getByRole('button', { name: 'Movies', exact: true }).getAttribute('aria-pressed'), 'true');
  await page.goForward(); await page.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  assert.equal(new URL(page.url()).hash, hash);
  assert.equal(fixture.posts.length, 0);
});

test('a direct Activity download link survives reload and has a real base tab for Back', async (t) => {
  const item = { id: 'sonarr:2', media: { kind: 'series', tvdb_id: 123456 }, title: 'Severance', episode: { season: 2, number: 3 }, acquisition: { state: 'downloading', completed_bytes: 40, total_bytes: 100 } };
  const hash = '#/activity/download/sonarr%3A2?q=Primer&kind=movie';
  const { page, fixture } = await panel(t, { queue: [item], hash });
  await page.getByRole('dialog', { name: 'Severance', exact: true }).waitFor();
  assert.equal(await page.locator('#activity-tab').getAttribute('aria-selected'), 'true');
  assert.match(await page.locator('#media-detail').innerText(), /Season 2 · Episode 3/);
  await page.reload(); await page.getByRole('dialog', { name: 'Severance', exact: true }).waitFor();
  assert.equal(new URL(page.url()).hash, hash);
  await page.goBack(); await page.locator('#media-dialog').waitFor({ state: 'hidden' });
  assert.equal(new URL(page.url()).hash, '#/activity?q=Primer&kind=movie');
  await page.getByRole('tab', { name: 'Search', exact: true }).click();
  assert.equal(await page.getByRole('searchbox', { name: 'Search movies and TV' }).inputValue(), 'Primer');
  assert.equal(await page.getByRole('button', { name: 'Movies', exact: true }).getAttribute('aria-pressed'), 'true');
  assert.equal(fixture.posts.length, 0);
});

test('download outage and disappearance retain a clearly last-seen modal until the user closes it', async (t) => {
  const item = { id: 'radarr:1', media: { kind: 'movie', tmdb_id: 14337 }, title: 'Primer', episode: null, acquisition: { state: 'downloading', completed_bytes: 40, total_bytes: 100 } };
  const { page, fixture } = await panel(t, { queue: [item] });
  await page.getByRole('tab', { name: /^Activity/ }).click(); await page.locator('#activity-queue-radarr-1-details').click();
  await page.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  for (const status of [503, 502]) {
    fixture.downloadFailure = status; await refreshDetails(page);
    await page.locator('#media-detail').getByText(/temporarily unavailable.*last seen/).waitFor();
    assert.match(await page.locator('#media-detail').innerText(), /Last seen: Downloading/);
    assert.equal(new URL(page.url()).hash, '#/activity/download/radarr%3A1');
  }
  fixture.downloadFailure = null; fixture.queue = [];
  await refreshDetails(page); await page.locator('#media-detail').getByText(/no longer in the active queue.*last seen/).waitFor();
  assert.equal(await page.getByRole('dialog').isVisible(), true);
  assert.doesNotMatch(await page.locator('#media-detail').innerText(), /Imported into library/);
  await page.clock.runFor(5001);
  await page.locator('#activity-queue-radarr-1-details').waitFor({ state: 'detached' });
  await page.keyboard.press('Escape'); await page.locator('#media-dialog').waitFor({ state: 'hidden' });
  assert.equal(await page.locator('#activity-tab').evaluate((element) => element === document.activeElement), true);
  assert.equal(new URL(page.url()).hash, '#/activity');
  assert.equal(fixture.posts.length, 0);
});

test('mobile modal traps keyboard focus, supports Escape, and never hides the originating Library tab', async (t) => {
  const { page, fixture } = await panel(t, { viewport: { width: 390, height: 850 } });
  await page.getByRole('tab', { name: 'Library', exact: true }).click();
  const origin = page.locator('#library-series-123456-details'); await origin.click();
  await page.getByRole('dialog', { name: 'Severance', exact: true }).waitFor();
  assert.equal(await page.locator('#library-tab').getAttribute('aria-selected'), 'true');
  assert.equal(await page.locator('#media-detail .media-poster-placeholder .icon').isVisible(), true);
  for (let index = 0; index < 9; index++) {
    await page.keyboard.press(index % 3 === 0 ? 'Shift+Tab' : 'Tab');
    assert.equal(await page.evaluate(() => document.getElementById('media-dialog').contains(document.activeElement)), true);
  }
  await page.setViewportSize({ width: 320, height: 850 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await capture(page, 'downloads-native-modal-mobile.png');
  await page.keyboard.press('Escape'); await page.locator('#media-dialog').waitFor({ state: 'hidden' });
  assert.equal(await origin.evaluate((element) => element === document.activeElement), true);
  assert.equal(new URL(page.url()).hash, '#/library');
  assert.equal(fixture.posts.length, 0);
});

test('direct missing downloads and malformed hash routes cannot fabricate completion or arbitrary API navigation', async (t) => {
  const { page, fixture } = await panel(t, { hash: '#/activity/download/radarr%3A999' });
  await page.locator('#media-detail').getByText(/no longer in the active queue.*completion has not been verified/).waitFor();
  assert.equal(await page.getByRole('dialog').isVisible(), true);
  assert.equal(await page.getByRole('progressbar').count(), 0);
  assert.equal(await page.locator('#request-media').count(), 0);
  await closeDetails(page);
  for (const hash of ['#/activity/download/radarr%3A01', '#/activity/download/radarr%3A1?token=not-a-credential', '#/search/title/movie/0', '#/library/title/movie/14337?kind=series&kind=movie', '#/search/title/movie/%3Cimg%3E']) {
    await page.evaluate((value) => { location.hash = value; }, hash);
    await page.waitForFunction(() => location.hash === '#/search');
    assert.equal(await page.locator('#media-dialog').evaluate((element) => element.open), false);
  }
  assert.equal(fixture.apiCalls.some(({ path }) => path.includes('<') || path.includes('%3C') || path.includes('token=')), false);
  assert.equal(fixture.posts.length, 0);
});

test('closing a modal aborts a delayed details read without reopening it or changing history later', async (t) => {
  const item = { id: 'radarr:1', media: { kind: 'movie', tmdb_id: 14337 }, title: 'Primer', episode: null, acquisition: { state: 'waiting' } };
  const { page, fixture } = await panel(t, { queue: [item] });
  let started, release;
  const requested = new Promise((resolve) => { started = resolve; });
  fixture.onDownload = async (route) => { started(); await new Promise((resolve) => { release = resolve; }); await route.fulfill({ json: download(item) }).catch(() => {}); };
  await page.getByRole('tab', { name: /^Activity/ }).click(); await page.locator('#activity-queue-radarr-1-details').click(); await requested;
  await closeDetails(page); const length = await page.evaluate(() => history.length); release();
  await page.clock.runFor(5001);
  assert.equal(new URL(page.url()).hash, '#/activity');
  assert.equal(await page.locator('#media-dialog').evaluate((element) => element.open), false);
  assert.equal(await page.evaluate(() => history.length), length);
  assert.equal(fixture.posts.length, 0);
  assert.deepEqual(fixture.errors, []);
});

test('unknown native download fields stay unknown rather than becoming zero progress or a fake ETA', async (t) => {
  const item = { id: 'radarr:1', media: { kind: 'movie', tmdb_id: 14337 }, title: 'Primer', episode: null, acquisition: { state: 'unknown' } };
  const { page, fixture } = await panel(t, { queue: [item] });
  fixture.downloads.set(item.id, download(item, { status: 'unknown', quality: null, progress: null, remaining_seconds: null, download_client: null, indexer: null, protocol: null, languages: [], custom_formats: [], custom_format_score: null, added_at: null, estimated_completion_at: null, import_status: 'unknown' }));
  await page.getByRole('tab', { name: /^Activity/ }).click(); await page.locator('#activity-queue-radarr-1-details').click();
  await page.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  assert.match(await page.locator('#media-detail').innerText(), /Not reported/);
  assert.equal(await page.getByRole('progressbar').count(), 0);
  assert.doesNotMatch(await page.locator('#media-detail').innerText(), /0 B|0s|0%/);
  assert.equal(fixture.posts.length, 0);
});


test('completed imports started outside Downloads show separate Plex and subtitle evidence and a read-only canonical title modal', async (t) => {
  const { page, fixture } = await panel(t, { imports: [imported(), imported({ id: "sonarr:31", manager: "sonarr", media: { kind: "series", tvdb_id: 123456 }, title: "Severance", episode: { season: 2, number: 3 }, plex: { state: "verified", observed_at_unix_ms: Date.now() }, subtitles: { state: "missing", observed_at_unix_ms: Date.now() } })] });
  fixture.movieDetail = { ...movie(), library: { state: "managed", upstream_id: 9 }, acquisition: { state: "imported" } };
  await page.getByRole('tab', { name: /^Activity/ }).click();
  const completed = page.locator('#completed-list');
  await completed.getByRole('heading', { name: 'Primer', exact: true }).waitFor();
  assert.match(await completed.innerText(), /Bluray-2160p/);
  assert.match(await completed.innerText(), /Plex library: unavailable \(authentication required\)/);
  assert.match(await completed.innerText(), /Subtitles: not verified \(Plex library unverified\)/);
  assert.match(await completed.innerText(), /Season 2, episode 3/);
  assert.match(await completed.innerText(), /Plex library: verified/);
  assert.match(await completed.innerText(), /Subtitles: not found/);
  assert.doesNotMatch(await completed.innerText(), /ready to watch|playback verified|Subtitles: verified/);
  assert.equal(fixture.requests.length, 0);
  const origin = page.locator('#completed-radarr-31-details'); await origin.click();
  await page.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  assert.equal(new URL(page.url()).hash, '#/activity/title/movie/14337');
  assert.equal(await page.locator('#activity-tab').getAttribute('aria-selected'), 'true');
  assert.equal(await page.locator('#request-mode, #request-media').count(), 0);
  assert.equal(fixture.apiCalls.some(({ path }) => path.startsWith('queue/')), false);
  await closeDetails(page);
  assert.equal(await origin.evaluate((element) => element === document.activeElement), true);
  assert.equal(fixture.posts.length, 0);
  assert.deepEqual(fixture.errors, []);
  await capture(page, 'downloads-completed-native-imports.png');
});

test('completed imports paginate through bounded native history and distinguish partial, stale and genuinely empty results', async (t) => {
  const { page, fixture } = await panel(t);
  const first = imported(), second = imported({ id: 'radarr:32', title: 'Another import' });
  fixture.onImports = async (route, url) => {
    assert.equal(url.searchParams.get('limit'), '20');
    if (url.searchParams.get('page_token') === 'history-page-2') return route.fulfill({ json: pageOf([second]) });
    return route.fulfill({ json: { ...pageOf([first], [{ source: 'radarr', state: 'partial', observed_at_unix_ms: Date.now() }, { source: 'sonarr', state: 'unavailable', observed_at_unix_ms: Date.now() }]), next_page: 'history-page-2' } });
  };
  await page.getByRole('tab', { name: /^Activity/ }).click();
  await page.locator('#completed-radarr-31-details').waitFor();
  assert.match(await page.locator('#completed-notice').innerText(), /Radarr results are partial/);
  assert.match(await page.locator('#completed-list').innerText(), /Last seen: Imported/);
  await page.getByRole('button', { name: 'Load more completed imports', exact: true }).click();
  await page.locator('#completed-radarr-32-details').waitFor();
  assert.equal(await page.locator('#completed-list .media-row').count(), 2);
  fixture.onImports = null; fixture.importsFailure = 503;
  await refreshDetails(page);
  await page.locator('#completed-notice').getByText(/Completed imports unavailable/).waitFor();
  assert.equal(await page.locator('#completed-list .media-row').count(), 2);
  assert.match(await page.locator('#completed-list').innerText(), /Last seen · Plex library/);
  assert.equal(await page.locator('#completed-empty').isVisible(), false);
  fixture.importsFailure = null; fixture.imports = [];
  await refreshDetails(page);
  await page.locator('#completed-empty').getByText('No completed imports were returned by Radarr or Sonarr.', { exact: true }).waitFor();
  assert.equal(await page.locator('#completed-list .media-row').count(), 0);
  assert.equal(fixture.posts.length, 0);
  assert.deepEqual(fixture.errors, []);
});

test('same-origin ingress keeps its route in the browser-visible owned parent hash across reload, Back and Forward', async (t) => {
  const { page, app, fixture } = await panel(t, { embedded: true });
  await page.evaluate(() => history.replaceState({ ...history.state, haFixture: 'preserved' }, '', location.href));
  await app.getByRole('tab', { name: /^Activity/ }).click();
  fixture.imports = [imported()];
  await app.getByRole('button', { name: 'Refresh status', exact: true }).click();
  const origin = app.locator('#completed-radarr-31-details'); await origin.click();
  await app.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  const hash = '#downloads/activity/title/movie/14337';
  assert.equal(new URL(page.url()).hash, hash);
  assert.equal(new URL(page.url()).search, '?panel=downloads');
  assert.equal(await page.evaluate(() => history.state.haFixture), 'preserved');
  const length = await page.evaluate(() => history.length);
  await page.clock.runFor(5001);
  assert.equal(await page.evaluate(() => history.length), length);
  await page.reload(); await app.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  assert.equal(new URL(page.url()).hash, hash);
  await page.goBack(); await app.locator('#media-dialog').waitFor({ state: 'hidden' });
  assert.equal(new URL(page.url()).hash, '#downloads/activity');
  assert.equal(await app.locator('#activity-tab').getAttribute('aria-selected'), 'true');
  await page.goForward(); await app.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  assert.equal(new URL(page.url()).hash, hash);
  assert.equal(fixture.posts.length, 0);
  assert.deepEqual(fixture.errors, []);
});

test('an unrelated Home Assistant fragment stays untouched while embedded Downloads uses its own fallback route', async (t) => {
  const { page, app, fixture } = await panel(t, { embedded: true, hash: '#ha-unrelated' });
  await app.getByRole('tab', { name: /^Activity/ }).click();
  assert.equal(new URL(page.url()).hash, '#ha-unrelated');
  assert.equal(new URL(page.url()).search, '?panel=downloads');
  assert.equal(await app.locator('html').evaluate(() => location.hash), '#/activity');
  assert.equal(await app.locator('#activity-tab').getAttribute('aria-selected'), 'true');
  assert.equal(fixture.posts.length, 0);
  assert.deepEqual(fixture.errors, []);
});


test('opening an existing result or another tab cancels a pending search debounce without closing details or destroying Forward', async (t) => {
  const { page, fixture } = await panel(t);
  const query = page.getByRole('searchbox', { name: 'Search movies and TV' });
  await query.fill('Primer'); await page.clock.runFor(301);
  const card = page.getByRole('button', { name: 'Primer, 2004, movie', exact: true }); await card.waitFor();
  await query.fill('Severance');
  await card.click(); await page.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  const hash = '#/search/title/movie/14337?q=Primer', length = await page.evaluate(() => history.length);
  await page.clock.runFor(350);
  assert.equal(await page.getByRole('dialog', { name: 'Primer', exact: true }).isVisible(), true);
  assert.equal(new URL(page.url()).hash, hash);
  assert.equal(await page.evaluate(() => history.length), length);
  await page.goBack(); await page.locator('#media-dialog').waitFor({ state: 'hidden' });
  assert.equal(new URL(page.url()).hash, '#/search?q=Primer');
  await page.goForward(); await page.getByRole('dialog', { name: 'Primer', exact: true }).waitFor();
  assert.equal(new URL(page.url()).hash, hash);
  await closeDetails(page);
  await query.fill('Severance'); await page.getByRole('tab', { name: 'Library', exact: true }).click();
  const tabLength = await page.evaluate(() => history.length);
  await page.clock.runFor(350);
  assert.equal(new URL(page.url()).hash, '#/library?q=Severance');
  assert.equal(await page.locator('#library-tab').getAttribute('aria-selected'), 'true');
  assert.equal(await page.evaluate(() => history.length), tabLength);
  await page.getByRole('tab', { name: 'Search', exact: true }).click();
  assert.equal(await query.inputValue(), 'Severance');
  await page.getByRole('button', { name: 'Severance, 2022, TV show', exact: true }).waitFor();
  assert.equal(fixture.posts.length, 0);
  assert.deepEqual(fixture.errors, []);
});


test('completed import polling refetches all user-loaded pages beyond100 with fresh arrivals, evidence and coherent cursors', async (t) => {
  const { page, fixture } = await panel(t);
  const native = Array.from({ length: 132 }, (_, index) => {
    const id = 132 - index;
    return imported({ id: `radarr:${id}`, title: `Import ${id}`, imported_at: new Date(Date.UTC(2026, 10, 7, 12, 0, id)).toISOString() });
  });
  let generation = 'before';
  fixture.onImports = async (route, url) => {
    assert.equal(url.searchParams.get('limit'), '20');
    const token = url.searchParams.get('page_token');
    if (token) assert.equal(token.split(':')[0], generation);
    const offset = token ? Number(token.split(':')[1]) : 0;
    const items = native.slice(offset, offset + 20), next = offset + items.length;
    await route.fulfill({ json: { ...pageOf(items), next_page: next < native.length ? `${generation}:${next}` : null } });
  };
  await page.getByRole('tab', { name: /^Activity/ }).click();
  const more = page.getByRole('button', { name: 'Load more completed imports', exact: true });
  await page.waitForFunction(() => document.querySelectorAll('#completed-list .media-row').length === 20);
  for (let pages = 2; pages <= 6; pages++) {
    await more.click();
    await page.waitForFunction((count) => document.querySelectorAll('#completed-list .media-row').length === count, pages * 20);
  }
  assert.equal(await page.locator('#completed-list .media-row').count(), 120);
  const length = await page.evaluate(() => history.length);
  native.unshift(imported({ id: 'radarr:133', title: 'New arrival', imported_at: '2026-11-07T12:03:00Z' }));
  const changed = native.find((item) => item.id === 'radarr:50');
  changed.plex = { state: 'verified', observed_at_unix_ms: Date.now() };
  changed.subtitles = { state: 'verified', languages: ['English'], observed_at_unix_ms: Date.now() };
  generation = 'after';
  await page.clock.runFor(5001);
  await page.locator('#completed-radarr-133-details').waitFor();
  assert.equal(await page.locator('#completed-list .media-row').count(), 120);
  await page.locator('#completed-radarr-50-details').locator('..').getByText('Subtitles: verified (English)', { exact: true }).waitFor();
  assert.equal(await more.isVisible(), true);
  assert.equal(await page.evaluate(() => history.length), length);
  await more.click();
  await page.waitForFunction(() => document.querySelectorAll('#completed-list .media-row').length === 133);
  await page.locator('#completed-radarr-1-details').waitFor();
  assert.equal(await more.isVisible(), false);
  assert.equal(fixture.posts.length, 0);
  assert.deepEqual(fixture.errors, []);
});
