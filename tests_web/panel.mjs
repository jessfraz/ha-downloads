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
let server;
let browser;
let base;

const readyStatus = () => ({
  version: "0.1.0",
  state: "ready",
  message: "The VPN, firewall, and DNS checks have passed.",
  vpn: { state: "connected", provider: "Example VPN", public_ip: "192.0.2.42" },
  downloader: { state: "running", url: "sabnzbd/", paused: false },
  protection: { kill_switch: "verified", dns: "verified" },
});

const setupStatus = () => ({
  version: "0.1.0",
  state: "needs_configuration",
  message: "Configure the VPN in the app settings to get started.",
  vpn: { state: "disconnected", provider: "", public_ip: null },
  downloader: { state: "stopped", url: "sabnzbd/", paused: true },
  protection: { kill_switch: "unverified", dns: "unverified" },
});

before(async () => {
  server = spawn(process.env.PYTHON || "python3", [resolve(directory, "fixture.py")], { stdio: ["ignore", "pipe", "inherit"] });
  const lines = createInterface({ input: server.stdout });
  const port = await Promise.race([
    once(lines, "line").then(([line]) => Number(line)),
    once(server, "exit").then(() => { throw new Error("Browser fixture exited before startup"); }),
    new Promise((_, reject) => {
      const timer = setTimeout(() => reject(new Error("Browser fixture startup timed out")), 5000);
      timer.unref();
    }),
  ]);
  assert.ok(Number.isInteger(port) && port > 0);
  base = `http://127.0.0.1:${port}/api/hassio_ingress/browser-test/`;
  browser = await chromium.launch({
    headless: true,
    ...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE } : {}),
  });
});

after(async () => {
  await browser?.close();
  if (server && server.exitCode === null) {
    server.kill("SIGTERM");
    await once(server, "exit");
  }
});

async function panel(t, initialStatus = readyStatus(), options = {}) {
  const context = await browser.newContext(options);
  t.after(() => context.close());
  const page = await context.newPage();
  await page.clock.install();
  const fixture = { status: initialStatus, httpStatus: 200, requests: 0, handle: null, paths: [], errors: [] };
  page.on("pageerror", (error) => fixture.errors.push(error.message));
  page.on("request", (request) => fixture.paths.push(new URL(request.url()).pathname));
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.split("api/v1/")[1];
    const now = Date.now();
    const coverage = [{ source: "radarr", state: "available", observed_at_unix_ms: now }, { source: "sonarr", state: "available", observed_at_unix_ms: now }];
    const responses = {
      system: { version: "0.1.5", capabilities: { read: true, request: true, admin: true }, transport: "ingress", credential_role: "administrator" },
      profiles: { items: [] },
      "health/stack": { generated_at_unix_ms: now, components: [], readiness: [{ operation: "transfer", profile_id: null, state: "ready", reasons: [], expires_at_unix_ms: now + 60000 }] },
      "managers/radarr/options": { manager: "radarr", quality_profiles: [], root_folders: [], tags: [], series_types: [] },
      "managers/sonarr/options": { manager: "sonarr", quality_profiles: [], root_folders: [], tags: [], series_types: [] },
      movies: { items: [], next_page: null, coverage }, series: { items: [], next_page: null, coverage },
      queue: { items: [], next_page: null, coverage }, requests: { items: [], next_page: null, coverage },
    };
    if (!(path in responses)) return route.fallback();
    await route.fulfill({ json: responses[path] });
  });
  await page.route("**/api/v1/status", async (route) => {
    fixture.requests++;
    assert.equal(route.request().url(), `${base}api/v1/status`);
    assert.equal(route.request().method(), "GET");
    if (fixture.handle) return fixture.handle(route);
    await route.fulfill({ status: fixture.httpStatus, json: fixture.status });
  });
  await page.goto(base);
  await page.waitForFunction(() => document.getElementById("connection-title").textContent !== "Checking your connection");
  if (!await page.locator("#stack-health-panel").isVisible()) await page.getByRole("button", { name: /Download stack/ }).click();
  if (!await page.locator(".protection-details").evaluate((element) => element.open)) await page.getByText("VPN and connection details", { exact: true }).click();
  await page.locator("#advanced-downloads").waitFor({ state: "visible" });
  await page.locator("#advanced-downloads > summary").click();
  return { page, fixture };
}

async function heading(page, title) {
  await page.getByRole("heading", { name: title, exact: true }).waitFor({ state: "visible" });
}

async function refresh(page) {
  const button = page.getByRole("button", { name: "Refresh status" });
  await button.waitFor({ state: "visible" });
  await button.click();
}

test("first run explains setup and keeps the downloader closed", async (t) => {
  const { page, fixture } = await panel(t, setupStatus());
  await heading(page, "Set up your VPN");
  await heading(page, "A private connection comes first");
  assert.match(await page.locator("#setup").innerText(), /Configuration/);
  assert.match(await page.locator("#setup").innerText(), /Save, then restart/);
  assert.equal(await page.locator("iframe").count(), 0);
  assert.equal(await page.getByRole("link", { name: /Open downloads/ }).count(), 0);
  assert.equal(await page.locator("#downloader-state").innerText(), "Stopped");
  assert.match(await page.locator("footer").innerText(), /Other Home Assistant apps keep their network settings/);
  assert.deepEqual(fixture.errors, []);
});

test("native module failure keeps a connected VPN distinct from downloader readiness", async (t) => {
  const status = readyStatus();
  status.state = "degraded";
  status.message = "The downloader's required native modules are unavailable. Update the app.";
  status.health = { ready: false, reasons: ["not_ready", "downloader_modules_unavailable"], checked_at_unix_ms: Date.now(), age_ms: 0, max_age_ms: 30000 };
  const { page } = await panel(t, status);
  await heading(page, "Downloader needs attention");
  await heading(page, "Update the app");
  assert.equal(await page.locator("#vpn-state").innerText(), "Connected");
  assert.equal(await page.locator("#downloader-state").innerText(), "Needs attention");
  assert.equal(await page.locator("iframe").count(), 0);
});

test("verified readiness opens the nested ingress queue and preserves a paused queue", async (t) => {
  const status = readyStatus();
  status.downloader.paused = true;
  const { page, fixture } = await panel(t, status);
  await heading(page, "Connected and protected");
  await heading(page, "Download queue");
  assert.equal(await page.locator("#paused-badge").innerText(), "Queue paused");
  assert.equal(await page.getByRole("link", { name: /Open downloads/ }).getAttribute("href"), `${base}sabnzbd/`);
  const queue = page.frameLocator("iframe[title='SABnzbd download queue']");
  await queue.getByRole("heading", { name: "SABnzbd queue fixture" }).waitFor({ state: "visible" });
  await queue.getByRole("searchbox", { name: "Search queue" }).fill("Keep this search");
  const updated = page.waitForResponse((response) => response.url() === `${base}api/v1/status`);
  await page.clock.runFor(5001);
  await updated;
  await page.getByRole("button", { name: "Refresh status" }).waitFor({ state: "visible" });
  assert.equal(await queue.getByRole("searchbox", { name: "Search queue" }).inputValue(), "Keep this search");
  assert.equal(fixture.paths.filter((path) => path.endsWith("/sabnzbd/")).length, 1);
  assert.deepEqual(fixture.errors, []);
});

test("a running process never overrides a missing tunnel or protection proof", async (t) => {
  const { page, fixture } = await panel(t);
  await heading(page, "Connected and protected");
  fixture.status = { ...readyStatus(), state: "degraded", message: "The VPN connection was interrupted.", vpn: { state: "disconnected", provider: "Example VPN", public_ip: null } };
  await refresh(page);
  await heading(page, "VPN connection interrupted");
  assert.equal(await page.locator("iframe").count(), 0);
  assert.equal(await page.locator("#public-ip").isVisible(), false);
  assert.equal(await page.locator("#vpn-state").innerText(), "Disconnected");
  assert.equal(await page.locator("#downloader-state").innerText(), "Running");
  assert.equal(await page.locator("#connection-badge").innerText(), "Needs attention");

  for (const check of ["kill_switch", "dns"]) {
    fixture.status = readyStatus();
    fixture.status.protection[check] = "unverified";
    await refresh(page);
    await heading(page, "Downloads unavailable");
    assert.equal(await page.locator("iframe").count(), 0);
    fixture.status.protection[check] = "failed";
    await refresh(page);
    await heading(page, "Protection check failed");
    assert.equal(await page.locator("iframe").count(), 0);
  }
  fixture.status = readyStatus();
  await refresh(page);
  await heading(page, "Download queue");
  assert.equal(await page.locator("iframe").count(), 1);
});

test("failed health and unknown status stay distinct from a stopped VPN and preserve diagnostic history", async (t) => {
  const status = readyStatus();
  status.state = "degraded";
  status.vpn.state = "unknown";
  status.diagnostics = {
    checks: { vpn_service: { outcome: "passed", http_status: 200, elapsed_ms: 1 }, vpn_health: { outcome: "timeout", elapsed_ms: 2000 } },
    last_incident: { duration_ms: 3000, recovered_at_unix_ms: null, initial_checks: { vpn_health: { outcome: "timeout", elapsed_ms: 2000 } } },
  };
  const { page, fixture } = await panel(t, status);
  await heading(page, "VPN status unavailable");
  assert.equal(await page.locator("#vpn-state").innerText(), "Status unavailable");
  await page.locator("#diagnostics-panel summary").click();
  assert.match(await page.locator("#diagnostic-checks").innerText(), /VPN health: Request timed out.*2000 ms/);
  assert.match(await page.locator("#diagnostic-history").innerText(), /Current failed-check period: 3.0 seconds/);
  assert.equal(await page.locator("iframe").count(), 0);
  fixture.status.vpn.state = "unhealthy";
  await refresh(page);
  await heading(page, "VPN health check failed");
  assert.equal(await page.locator("#vpn-state").innerText(), "Health check failed");
  const diagnostics = fixture.status.diagnostics;
  diagnostics.last_incident.recovered_at_unix_ms = 10000;
  fixture.status = { ...readyStatus(), diagnostics };
  await refresh(page);
  await heading(page, "Connected and protected");
  assert.match(await page.locator("#diagnostic-history").innerText(), /Last failed-check period: 3.0 seconds/);
  assert.match(await page.locator("#diagnostic-history").innerText(), /VPN health: Request timed out/);
  assert.equal(await page.locator("iframe").count(), 1);
  assert.deepEqual(fixture.errors, []);
});

test("unknown queue state stays neutral instead of claiming the operator paused or resumed it", async (t) => {
  const status = readyStatus();
  status.downloader.paused = null;
  const { page, fixture } = await panel(t, status);
  await heading(page, "Connected and protected");
  await heading(page, "Download queue");
  assert.equal(await page.locator("#downloader-state").innerText(), "Running");
  assert.equal(await page.locator("#downloader-detail").innerText(), "Queue managed in SABnzbd.");
  assert.equal(await page.locator("#downloader-card").getAttribute("data-tone"), "neutral");
  assert.equal(await page.locator("#paused-badge").isVisible(), false);
  fixture.status.downloader.paused = true;
  await refresh(page);
  await page.getByText("Queue paused", { exact: true }).first().waitFor({ state: "visible" });
  assert.equal(await page.locator("#paused-badge").isVisible(), true);
  fixture.status.downloader.paused = null;
  await refresh(page);
  await page.getByText("Queue managed in SABnzbd.", { exact: true }).waitFor({ state: "visible" });
  assert.equal(await page.locator("#paused-badge").isVisible(), false);
  assert.deepEqual(fixture.errors, []);
});

test("HTTP failure revokes stale protection and automatic retry restores the queue", async (t) => {
  const { page, fixture } = await panel(t);
  await heading(page, "Connected and protected");
  fixture.httpStatus = 503;
  await refresh(page);
  await heading(page, "Connection status unavailable");
  assert.equal(await page.locator("iframe").count(), 0);
  assert.equal(await page.locator("#firewall-state").innerText(), "Unavailable");
  assert.equal(await page.locator("#vpn-state").innerText(), "Unavailable");
  fixture.httpStatus = 200;
  await page.clock.runFor(5001);
  await heading(page, "Connected and protected");
});

test("unknown responses fail closed and text and upstream URLs cannot inject navigation", async (t) => {
  const { page, fixture } = await panel(t);
  await heading(page, "Connected and protected");
  fixture.status = { ...readyStatus(), protection: { kill_switch: "unknown", dns: "verified" } };
  await refresh(page);
  await heading(page, "Connection status unavailable");
  assert.equal(await page.locator("iframe").count(), 0);
  fixture.status = readyStatus();
  fixture.status.message = "<img src=x onerror=alert(1)>";
  fixture.status.vpn.provider = "<script>bad()</script>";
  fixture.status.downloader.url = "https://untrusted.example/";
  await refresh(page);
  await heading(page, "Connected and protected");
  assert.equal(await page.locator("#connection-message").innerText(), fixture.status.message);
  assert.equal(await page.locator("#vpn-detail").innerText(), fixture.status.vpn.provider);
  assert.equal(await page.locator("#connection-message img, #vpn-detail script").count(), 0);
  assert.equal(await page.locator("iframe").getAttribute("src"), `${base}sabnzbd/`);
  assert.deepEqual(fixture.errors, []);
});

test("slow requests do not overlap, time out, and retry without late stale responses", async (t) => {
  const { page, fixture } = await panel(t);
  await heading(page, "Connected and protected");
  let release;
  let started;
  const requested = new Promise((resolveRequest) => { started = resolveRequest; });
  fixture.handle = async (route) => {
    started();
    await new Promise((resolveResponse) => { release = resolveResponse; });
    await route.fulfill({ json: readyStatus() }).catch(() => {});
  };
  const before = fixture.requests;
  await refresh(page);
  await requested;
  await page.clock.runFor(6000);
  assert.equal(fixture.requests, before + 1);
  assert.equal(await page.getByRole("button", { name: "Refresh status" }).isDisabled(), true);
  await page.clock.runFor(2100);
  await heading(page, "Connection status unavailable");
  release();
  fixture.handle = null;
  fixture.status = setupStatus();
  await page.clock.runFor(5001);
  await heading(page, "Set up your VPN");
  assert.equal(await page.locator("iframe").count(), 0);
  assert.deepEqual(fixture.errors, []);
});

test("page lifecycle cancels polling and rechecks before restoring the existing queue", async (t) => {
  const { page, fixture } = await panel(t);
  await heading(page, "Connected and protected");
  const queue = page.frameLocator("iframe");
  await queue.getByRole("searchbox", { name: "Search queue" }).fill("Survives page restore");
  // Browser lifecycle events exercise the shipped listeners, with no test globals.
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent("pagehide", { persisted: true })));
  const before = fixture.requests;
  await page.clock.runFor(20000);
  assert.equal(fixture.requests, before);
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent("pageshow", { persisted: true })));
  await heading(page, "Connected and protected");
  assert.equal(await queue.getByRole("searchbox", { name: "Search queue" }).inputValue(), "Survives page restore");
  assert.equal(fixture.requests, before + 1);
  assert.deepEqual(fixture.errors, []);
});

test("starting and stopping remain understandable without exposing a queue", async (t) => {
  const status = setupStatus();
  status.state = "starting";
  status.message = "";
  status.vpn.state = "connecting";
  status.health = { ready: false, reasons: ["not_ready", "checks_missing"], checked_at_unix_ms: null, age_ms: null, max_age_ms: 30000 };
  const { page, fixture } = await panel(t, status);
  await heading(page, "Connecting securely");
  assert.equal(await page.locator("#vpn-state").innerText(), "Connecting");
  fixture.status = { ...status, state: "stopping" };
  await refresh(page);
  await heading(page, "Downloads is stopping");
  assert.equal(await page.locator("iframe").count(), 0);
});

test("mobile, dark mode, reduced motion, and keyboard controls remain usable", async (t) => {
  const { page, fixture } = await panel(t, setupStatus(), { viewport: { width: 360, height: 800 }, colorScheme: "dark", reducedMotion: "reduce" });
  await heading(page, "Set up your VPN");
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth), false);
  await page.getByRole("link", { name: "Downloads home" }).focus();
  await page.keyboard.press("Tab");
  assert.equal(await page.getByRole("button", { name: "Refresh status" }).evaluate((element) => element === document.activeElement), true);
  const before = fixture.requests;
  const updated = page.waitForResponse((response) => response.url() === `${base}api/v1/status`);
  await page.keyboard.press("Enter");
  await updated;
  assert.equal(fixture.requests, before + 1);
  await page.setViewportSize({ width: 320, height: 800 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth), false);
  if (process.env.SCREENSHOT_DIR) {
    await mkdir(process.env.SCREENSHOT_DIR, { recursive: true });
    await page.screenshot({ path: resolve(process.env.SCREENSHOT_DIR, "downloads-setup-mobile-dark.png"), fullPage: true });
    await page.setViewportSize({ width: 1200, height: 900 });
    await page.emulateMedia({ colorScheme: "light" });
    await page.screenshot({ path: resolve(process.env.SCREENSHOT_DIR, "downloads-setup-desktop-light.png"), fullPage: true });
    fixture.status = readyStatus();
    await refresh(page);
    await heading(page, "Connected and protected");
    await page.frameLocator("iframe").getByRole("heading", { name: "SABnzbd queue fixture" }).waitFor({ state: "visible" });
    await page.screenshot({ path: resolve(process.env.SCREENSHOT_DIR, "downloads-ready-desktop-light.png"), fullPage: true });
  }
  assert.deepEqual(fixture.errors, []);
});

test("ingress follows Home Assistant theme changes independently of the system theme", async (t) => {
  const { page } = await panel(t, setupStatus(), { colorScheme: "light" });
  await page.goto(`${base}theme-host/`);
  const ingress = page.frameLocator("iframe[title='Downloads ingress']");
  await ingress.locator("#connection-title").filter({ hasText: "Set up your VPN" }).waitFor({ state: "attached" });
  if (!await ingress.locator("#stack-health-panel").isVisible()) await ingress.getByRole("button", { name: /Download stack/ }).click();
  if (!await ingress.locator(".protection-details").evaluate((element) => element.open)) await ingress.getByText("VPN and connection details", { exact: true }).click();
  await ingress.getByRole("heading", { name: "Set up your VPN" }).waitFor({ state: "visible" });
  const root = ingress.locator("html");
  assert.equal(await root.evaluate((element) => getComputedStyle(element).backgroundColor), "rgb(18, 27, 36)");
  assert.equal(await root.evaluate((element) => getComputedStyle(element).colorScheme), "dark");
  await page.evaluate(() => {
    document.documentElement.style.colorScheme = "light";
    document.documentElement.style.setProperty("--primary-background-color", "#f5f7fa");
    document.documentElement.style.setProperty("--card-background-color", "#fff");
    document.documentElement.style.setProperty("--primary-text-color", "#1b2a35");
  });
  await root.locator("body").waitFor({ state: "visible" });
  await page.waitForFunction(() => {
    const root = document.querySelector("iframe").contentDocument.documentElement;
    return getComputedStyle(root).colorScheme === "light";
  });
  assert.equal(await root.evaluate((element) => getComputedStyle(element).backgroundColor), "rgb(245, 247, 250)");
});


test("stale functional checks close the queue and retain fixed service failure details", async (t) => {
  const status = readyStatus();
  status.health = { ready: false, reasons: ["checks_stale"], checked_at_unix_ms: 1000, age_ms: 31000, max_age_ms: 30000 };
  status.diagnostics = {
    checks: {}, last_incident: null,
    last_failure: { reason: "downloader_exited", exit_code: 23, signal: null, cleanup_failed: false },
  };
  const { page, fixture } = await panel(t, status);
  await heading(page, "Connection checks unavailable");
  assert.equal(await page.locator("iframe").count(), 0);
  await page.locator("#diagnostics-panel summary").click();
  assert.match(await page.locator("#diagnostic-history").innerText(), /Downloader exited.*exit 23/);
  assert.equal(await page.getByRole("link", { name: "Source code" }).getAttribute("href"), "https://github.com/jessfraz/ha-downloads");
  assert.deepEqual(fixture.errors, []);
});


test("terminal service failures preserve their specific recovery message", async (t) => {
  const status = setupStatus();
  status.state = "degraded";
  status.message = "Protected services could not be fully stopped. Restart the app.";
  status.health = { ready: false, reasons: ["not_ready", "checks_missing"], checked_at_unix_ms: null, age_ms: null, max_age_ms: 30000 };
  const { page } = await panel(t, status);
  await heading(page, "Downloads stopped");
  assert.equal(await page.locator("#connection-message").innerText(), status.message);
  assert.equal(await page.locator("iframe").count(), 0);
});

test("collapsed connection details do not silence announcements and setup guidance opens itself", async (t) => {
  const { page, fixture } = await panel(t);
  await page.reload();
  await page.getByRole("button", { name: "Download stack healthy", exact: true }).waitFor();
  await page.locator("#connection-title").filter({ hasText: "Connected and protected" }).waitFor({ state: "attached" });
  assert.equal(await page.locator("#stack-health-panel").isVisible(), false);
  assert.equal(await page.getByRole("status").filter({ hasText: /Connected and protected/ }).count(), 1);
  fixture.status = setupStatus();
  await refresh(page);
  await heading(page, "Set up your VPN");
  assert.equal(await page.locator("#stack-health-toggle").getAttribute("aria-expanded"), "true");
  assert.equal(await page.locator(".protection-details").evaluate((element) => element.open), true);
});
