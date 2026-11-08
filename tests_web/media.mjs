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
  const fixture = { posts: [], readKeys: [], errors: [], apiCalls: [], searchCoverage: coverage(), queueCoverage: coverage(), queue: [], requests: [], onPost: null, onKey: null, onHealth: null, systemFailure: options.systemFailure ?? false, health: null, search: [movie(), series()], requestPermission: options.requestPermission ?? true, adminPermission: options.adminPermission ?? true, artworkPaths: [] };
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
    else if (path === "requests") value = pageOf(fixture.requests);
    else if (path.startsWith("requests/")) value = fixture.requests.find((r) => r.id === path.split("/")[1]);
    if (value === undefined) return route.fulfill({ status: 404, json: { error_code: "not_found", message: "Not found" } });
    return route.fulfill({ json: value });
  });
  await page.goto(base); await page.getByRole("button", { name: "Download stack healthy" }).waitFor(); return { page, fixture };
}
async function find(page, query, title) {
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
  fixture.queue = [{ id: "radarr-1", media: { kind: "movie", tmdb_id: 14337 }, title: "Primer", episode: null, acquisition: { state: "downloading", completed_bytes: 40, total_bytes: 100 } }];
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
  await page.getByRole("tab", { name: /Activity/ }).click();
  await page.getByRole("button", { name: "Retry original request", exact: true }).waitFor();
  fixture.onPost = null;
  await page.getByRole("button", { name: "Retry original request", exact: true }).click();
  await page.locator("#activity-list").getByText(/Request accepted and search queued/).waitFor();
  assert.equal(fixture.posts.length, 2); assert.equal(fixture.posts[1].key, original.key); assert.deepEqual(fixture.posts[1].input, original.input);
});
test("mobile detail drill-in, theme, keyboard tabs, and true imported labels remain usable", async (t) => {
  const { page, fixture } = await panel(t, { viewport: { width: 390, height: 850 } });
  await find(page, "Primer", "Primer, 2004, movie");
  assert.equal(await page.getByRole("searchbox", { name: "Search movies and TV" }).isVisible(), false);
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await capture(page, "downloads-media-mobile-detail.png");
  await page.getByRole("button", { name: "← Back to results" }).click();
  await page.getByRole("tab", { name: "Search", exact: true }).focus(); await page.keyboard.press("ArrowRight");
  await page.getByRole("heading", { name: "Your library" }).waitFor();
  await page.setViewportSize({ width: 320, height: 850 }); assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await page.setViewportSize({ width: 1100, height: 900 }); await page.getByRole("tab", { name: "Search", exact: true }).click();
  await page.getByRole("button", { name: "Primer, 2004, movie" }).click(); await capture(page, "downloads-media-desktop-dark.png");
  await page.emulateMedia({ colorScheme: "light" }); await capture(page, "downloads-media-desktop-light.png");
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
  await page.getByRole("button", { name: "Refresh status", exact: true }).click();
  await page.clock.runFor(1100);
  await page.getByRole("button", { name: "Download stack status stale", exact: true }).waitFor();
});

test("target readiness copy and action expire together without rebuilding the detail", async (t) => {
  const { page, fixture } = await panel(t);
  await find(page, "Primer", "Primer, 2004, movie");
  await page.locator("#request-readiness-note").waitFor({ state: "hidden" });
  await page.waitForFunction(() => !document.getElementById("request-media").disabled);
  const now = await page.evaluate(() => Date.now());
  fixture.health = healthy("request_movie");
  fixture.health.readiness[0].expires_at_unix_ms = now + 500;
  const scoped = page.waitForResponse((response) => response.url().includes("health/stack?") && response.url().includes("operation="));
  await page.getByRole("button", { name: "Refresh status", exact: true }).click(); await scoped;
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
  fixture.queue = [{ id: "radarr-1", media: { kind: "movie", tmdb_id: 14337 }, title: "Primer", episode: null, acquisition: { state: "downloading", completed_bytes: 40, total_bytes: 100 } }];
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
  await page.waitForFunction(() => !document.getElementById("request-media").disabled);
  const blocked = structuredClone(initial);
  blocked.generated_at_unix_ms += 100;
  blocked.readiness[0].state = "blocked";
  blocked.readiness[0].reasons = ["storage_unavailable"];
  blocked.readiness[0].expires_at_unix_ms -= 30000;
  fixture.onHealth = async (route, url) => route.fulfill({ json: url.searchParams.has("operation") ? blocked : healthy() });
  await page.getByRole("button", { name: "Refresh status", exact: true }).click();
  await page.locator("#request-readiness-note").getByText(/selected library storage is unavailable/).waitFor();
  assert.equal(await page.getByRole("button", { name: "Request movie", exact: true }).isDisabled(), true);
});

test("wall-clock skew and repeated identical evidence cannot extend a readiness deadline", async (t) => {
  const { page, fixture } = await panel(t);
  const frozen = healthy("request_movie"); frozen.readiness[0].expires_at_unix_ms = frozen.generated_at_unix_ms + 1500;
  fixture.health = frozen;
  await find(page, "Primer", "Primer, 2004, movie");
  await page.waitForFunction(() => !document.getElementById("request-media").disabled);
  const wall = await page.evaluate(() => Date.now());
  await page.clock.setSystemTime(new Date(wall + 86400000));
  assert.equal(await page.getByRole("button", { name: "Request movie", exact: true }).isEnabled(), true);
  await page.clock.setSystemTime(new Date(wall - 86400000));
  await page.clock.runFor(600);
  const refreshed = page.waitForResponse((response) => response.url().includes("health/stack?") && response.url().includes("operation="));
  await page.getByRole("button", { name: "Refresh status", exact: true }).click(); await refreshed;
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
  await page.waitForFunction(() => !document.getElementById("request-media").disabled);
  await page.clock.runFor(900);
  assert.equal(await page.getByRole("button", { name: "Request movie", exact: true }).isDisabled(), true);
});

test("mobile details return to their Library or Activity origin and restore the originating action", async (t) => {
  const { page, fixture } = await panel(t, { viewport: { width: 390, height: 850 } });
  await page.getByRole("tab", { name: "Library", exact: true }).click();
  const libraryAction = page.locator("#library-movie-14337-details"); await libraryAction.click();
  await page.getByRole("button", { name: "← Back to library", exact: true }).click();
  assert.equal(await page.getByRole("tab", { name: "Library", exact: true }).getAttribute("aria-selected"), "true");
  assert.equal(await libraryAction.evaluate((element) => element === document.activeElement), true);
  fixture.queue = [{ id: "radarr-1", media: { kind: "movie", tmdb_id: 14337 }, title: "Primer", episode: null, acquisition: { state: "waiting" } }];
  await page.getByRole("tab", { name: /Activity/ }).click();
  const activityAction = page.locator("#activity-queue-radarr-1-details"); await activityAction.click();
  await page.getByRole("button", { name: "← Back to activity", exact: true }).click();
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
  await page.getByRole("button", { name: "Refresh status", exact: true }).click();
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
    await page.getByRole('button', { name: accessibleTitle, exact: true }).click();
    if (kind === 'series') await page.getByRole('checkbox', { name: /Season 2/ }).check();
    await page.waitForFunction(() => !document.getElementById('request-media').disabled);
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
  await page.locator('#media-detail').getByText('Title details could not be verified. Refresh before requesting.', { exact: true }).waitFor();
  assert.equal(await page.locator('#request-media').isDisabled(), true);
  assert.equal(fixture.posts.length, 0);
});
