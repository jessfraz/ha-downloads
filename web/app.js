import { shell } from "./shell.js";

(() => {
  "use strict";

  const POLL_INTERVAL = 5000;
  const REQUEST_TIMEOUT = 8000;
  const statusUrl = new URL("api/v1/status", document.baseURI);
  const byId = (id) => document.getElementById(id);
  const refreshButton = byId("refresh");
  let pollingTimer = null;
  let activeRequest = null;
  let active = true;
  let themeObserver = null;
  let lastGuidanceState = null;

  function followHomeAssistantTheme() {
    if (window.parent === window) return;
    try {
      const parentRoot = window.parent.document.documentElement;
      const copyTheme = () => {
        const theme = window.parent.getComputedStyle(parentRoot);
        for (const name of ["--primary-text-color", "--secondary-text-color", "--primary-background-color",
          "--secondary-background-color", "--card-background-color", "--divider-color", "--primary-color"]) {
          const value = theme.getPropertyValue(name).trim();
          if (value) document.documentElement.style.setProperty(name, value);
          else document.documentElement.style.removeProperty(name);
        }
        // HA chooses a theme independently of the operating system theme.
        const scheme = theme.colorScheme;
        if (scheme === "light" || scheme === "dark") document.documentElement.dataset.theme = scheme;
        else delete document.documentElement.dataset.theme;
      };
      copyTheme();
      themeObserver = new MutationObserver(copyTheme);
      themeObserver.observe(parentRoot, { attributes: true, attributeFilter: ["style", "class"] });
    } catch {
      // A cross-origin embedding still gets the system's light or dark palette.
    }
  }

  const states = {
    app: ["needs_configuration", "starting", "ready", "degraded", "stopping"],
    vpn: ["disconnected", "connecting", "connected", "unknown", "unhealthy"],
    downloader: ["stopped", "starting", "running"],
    protection: ["unverified", "verified", "failed"],
  };

  function readStatus(value) {
    if (
      !value || !states.app.includes(value.state) ||
      !states.vpn.includes(value.vpn?.state) ||
      !states.downloader.includes(value.downloader?.state) ||
      !states.protection.includes(value.protection?.kill_switch) ||
      !states.protection.includes(value.protection?.dns) ||
      !(value.downloader.paused === null || typeof value.downloader.paused === "boolean") ||
      typeof value.message !== "string" ||
      typeof value.version !== "string" ||
      typeof value.vpn.provider !== "string" ||
      !(value.vpn.public_ip === null || typeof value.vpn.public_ip === "string")
      || (value.health !== undefined && (
        typeof value.health?.ready !== "boolean" ||
        !Array.isArray(value.health.reasons) ||
        !value.health.reasons.every((reason) => typeof reason === "string")
      ))
    ) {
      throw new Error("Status response is not recognized");
    }
    return value;
  }

  function announce(message) {
    const announcement = byId("announcement");
    if (announcement.textContent !== message) announcement.textContent = message;
  }

  function setBanner(title, message, badge, tone = "neutral") {
    byId("connection-panel").dataset.tone = tone;
    byId("connection-title").textContent = title;
    byId("connection-message").textContent = message;
    byId("connection-badge").textContent = badge;
    byId("connection-symbol").setAttribute("href", tone === "failure" ? "#icon-alert" : "#icon-shield");
    announce(`${title}. ${message}`);
  }

  function setCheck(name, label, detail, tone = "neutral") {
    byId(`${name}-state`).textContent = label;
    byId(`${name}-detail`).textContent = detail;
    byId(`${name}-card`).dataset.tone = tone;
  }

  function renderDiagnostics(status) {
    const panel = byId("diagnostics-panel");
    const monitoring = status.diagnostics;
    panel.hidden = !monitoring?.checks;
    if (panel.hidden) return;
    const names = [["vpn_service", "VPN service"], ["vpn_health", "VPN health"], ["dns_service", "DNS service"], ["dns_lookup", "DNS lookup"], ["resolver", "Resolver configuration"], ["firewall", "Firewall"], ["downloader", "Downloader API"]];
    const outcomes = { passed: "Passed", not_checked: "Not checked", stopped: "Stopped", timeout: "Request timed out", transport_error: "Request failed", http_error: "HTTP request rejected", invalid_response: "Invalid response", modules_unavailable: "Required native modules unavailable", health_failed: "Health check failed", unsafe_resolver: "Resolver protection failed", dns_lookup_failed: "DNS lookup failed", firewall_failed: "Firewall check failed" };
    const details = { not_ready: "not ready", icmp: "ICMP check", dns: "DNS check", tls: "TLS check", timeout: "timeout", connection: "connection check", connect: "connection failed", connection_refused: "connection refused", connection_reset: "connection reset", broken_pipe: "broken pipe", unexpected_eof: "connection closed", body: "response interrupted", other: "unspecified cause" };
    const describe = (check) => {
      if (!check || !Object.hasOwn(outcomes, check.outcome)) return "Unavailable";
      const parts = [outcomes[check.outcome]];
      if (Number.isInteger(check.http_status) && check.http_status >= 100 && check.http_status <= 599) parts.push(`HTTP ${check.http_status}`);
      for (const code of [check.health_reason, check.transport_reason]) if (Object.hasOwn(details, code)) parts.push(details[code]);
      if (Number.isFinite(check.elapsed_ms) && check.elapsed_ms >= 0) parts.push(`${Math.round(check.elapsed_ms)} ms`);
      return parts.join(" · ");
    };
    const list = byId("diagnostic-checks");
    list.replaceChildren(...names.map(([name, label]) => { const row = document.createElement("li"); row.textContent = `${label}: ${describe(name === "downloader" ? monitoring.downloader_check : monitoring.checks[name])}`; return row; }));
    const incident = monitoring.last_incident;
    const history = byId("diagnostic-history");
    if (incident && Number.isFinite(incident.duration_ms)) {
      const failures = names.filter(([name]) => name !== "downloader" && incident.initial_checks?.[name]?.outcome !== "passed").map(([name, label]) => `${label}: ${describe(incident.initial_checks?.[name])}`).join("; ");
      history.textContent = `${incident.recovered_at_unix_ms === null ? "Current failed-check period" : "Last failed-check period"}: ${(incident.duration_ms / 1000).toFixed(1)} seconds. ${failures}`;
    } else history.textContent = "No failed-check period recorded since startup.";
    const failure = monitoring.last_failure;
    if (failure) {
      const reasons = { vpn_exited: "VPN service exited", downloader_exited: "Downloader exited", vpn_not_ready: "VPN startup timed out", downloader_not_ready: "Downloader startup timed out", protection_checks_failed: "Protection checks failed", vpn_start_failed: "VPN service could not start", downloader_start_failed: "Downloader could not start", downloader_configuration_failed: "Downloader configuration failed", runtime_failed: "Protection runtime failed", cleanup_failed: "Services could not fully stop" };
      const details = [reasons[failure.reason] || "Protected service failed"];
      if (Number.isInteger(failure.exit_code)) details.push(`exit ${failure.exit_code}`);
      if (Number.isInteger(failure.signal)) details.push(`signal ${failure.signal}`);
      if (failure.cleanup_failed) details.push("cleanup incomplete");
      history.textContent += ` Last service failure: ${details.join(" · ")}.`;
    }
  }

  function showHelp(title, message) {
    byId("help-title").textContent = title;
    byId("help-message").textContent = message;
    byId("connection-help").hidden = false;
  }

  function render(status) {
    const vpnConnected = status.vpn.state === "connected";
    const firewallVerified = status.protection.kill_switch === "verified";
    const dnsVerified = status.protection.dns === "verified";
    const ready = status.state === "ready" && vpnConnected && firewallVerified &&
      dnsVerified && status.downloader.state === "running" && status.health?.ready !== false;
    const setup = status.state === "needs_configuration";
    const stopping = status.state === "stopping";
    const failed = status.protection.kill_switch === "failed" || status.protection.dns === "failed";
    const queuePaused = status.downloader.paused === true;
    const modulesUnavailable = status.health?.reasons.includes("downloader_modules_unavailable") === true;
    const message = status.message.slice(0, 1000);
    const guidanceState = `${status.state}:${status.vpn.state}:${failed}`;
    if ((setup || stopping || failed || status.state === "degraded") && guidanceState !== lastGuidanceState) {
      byId("stack-health-panel").hidden = false;
      byId("stack-health-toggle").setAttribute("aria-expanded", "true");
      document.querySelector(".protection-details").open = true;
    }
    lastGuidanceState = guidanceState;

    byId("setup").hidden = !setup;
    byId("connection-help").hidden = true;
    byId("app-version").textContent = status.version.slice(0, 40);
    byId("app-version").hidden = false;
    shell.setStatus(ready ? "Download stack ready" : setup ? "Setup needed" : "Download stack not ready", ready ? "ok" : setup ? "neutral" : "stale");
    const checkedAt = status.health?.checked_at_unix_ms;
    byId("last-checked").textContent = status.health !== undefined && !Number.isFinite(checkedAt)
      ? "Checks unavailable"
      : `Checked ${new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" }).format(new Date(checkedAt ?? Date.now()))}`;
    renderDiagnostics(status);

    const vpnLabels = { connected: "Connected", connecting: "Connecting", disconnected: "Disconnected", unknown: "Status unavailable", unhealthy: "Health check failed" };
    setCheck("vpn", vpnLabels[status.vpn.state], vpnConnected
      ? status.vpn.provider.slice(0, 80) || "VPN connection established."
      : status.vpn.state === "connecting" ? "Establishing a VPN connection."
      : status.vpn.state === "unknown" ? "The connection could not be verified. Check the diagnostic details."
      : status.vpn.state === "unhealthy" ? "The VPN service is running, but its health check failed." : "The VPN service is stopped.",
    vpnConnected ? "success" : !setup ? "warning" : "neutral");
    const ip = byId("public-ip");
    ip.hidden = !vpnConnected || !status.vpn.public_ip;
    ip.textContent = ip.hidden ? "" : `Public IP: ${status.vpn.public_ip.slice(0, 128)}`;

    for (const [name, state, verifiedDetail] of [
      ["firewall", status.protection.kill_switch, "Kill switch checks passed."],
      ["dns", status.protection.dns, "VPN DNS checks passed."],
    ]) {
      setCheck(name, state === "verified" ? "Verified" : state === "failed" ? "Check failed" : "Unverified",
        state === "verified" ? verifiedDetail : state === "failed" ? "Connection needs attention." : "Waiting for connection checks.",
        state === "verified" ? "success" : state === "failed" ? "failure" : "neutral");
    }
    const downloaderLabels = { running: queuePaused ? "Queue paused" : "Running", starting: "Starting", stopped: "Stopped" };
    setCheck("downloader", modulesUnavailable ? "Needs attention" : downloaderLabels[status.downloader.state], modulesUnavailable
      ? "Required native modules are unavailable. Update the app."
      : status.downloader.state === "running"
      ? queuePaused ? "The queue is paused in SABnzbd." : status.downloader.paused === null ? "Queue managed in SABnzbd." : "SABnzbd is running."
      : status.downloader.state === "starting" ? "Starting SABnzbd." : "Waiting for a verified connection.",
    ready ? queuePaused ? "warning" : status.downloader.paused === false ? "success" : "neutral" : "neutral");

    if (ready) {
      setBanner("Connected and protected", message || "The VPN, firewall, and DNS checks have passed. Downloads are ready.", "Protected", "success");
      return;
    }
    if (status.state === "ready" && status.health?.reasons.some((reason) => reason === "checks_stale" || reason === "checks_missing")) {
      for (const name of ["vpn", "firewall", "dns", "downloader"]) {
        setCheck(name, "Unverified", "Recent check results are unavailable.");
      }
      ip.hidden = true;
      setBanner("Connection checks unavailable", "Current VPN and downloader readiness could not be verified. Check the app diagnostics.", "Needs attention", "warning");
      showHelp("Check the app logs", "The last status cannot establish that downloads are ready.");
      return;
    }
    if (setup) {
      setBanner("Set up your VPN", message || "Configure the VPN in the app settings to get started.", "Setup needed");
    } else if (stopping) {
      setBanner("Downloads is stopping", message || "The app is shutting down its connection and downloader.", "Stopping");
      showHelp("Stopping safely", "Wait for the app to finish stopping before starting it again.");
    } else if (status.state === "degraded" && status.downloader.state === "stopped") {
      setBanner("Downloads stopped", message || "A protected service stopped. Review the app logs.", "Needs attention", "failure");
      showHelp("Check the app logs", "Review the reported failure in Home Assistant before restarting the app.");
    } else if (failed) {
      setBanner("Protection check failed", message || "The connection checks need attention before requests can start.", "Needs attention", "failure");
      showHelp("Check the app logs", "Review the VPN settings and app logs in Home Assistant. Requests stay disabled until the connection checks pass.");
    } else if (modulesUnavailable) {
      setBanner("Downloader needs attention", message, "Needs attention", "failure");
      showHelp("Update the app", "The downloader's required native modules are unavailable. Install the corrected app version in Home Assistant.");
    } else if (status.state === "degraded" || status.state === "ready") {
      setBanner(vpnConnected ? "Downloads unavailable" : status.vpn.state === "unknown" ? "VPN status unavailable" : status.vpn.state === "unhealthy" ? "VPN health check failed" : "VPN connection interrupted",
        message || "The connection is not ready. Requests become available once the checks pass.", "Needs attention", "warning");
      showHelp("Waiting for a secure connection", "Requests stay disabled until the VPN, firewall, and DNS checks pass. If this continues, check the app settings and logs in Home Assistant.");
    } else {
      setBanner("Connecting securely", message || "Waiting for the VPN, firewall, and DNS checks before starting downloads.", "Starting");
      showHelp("Getting things ready", "Requests become available when the connection is ready. First startup can take a little longer.");
    }
  }

  function renderUnavailable() {
    if (lastGuidanceState !== "unavailable") {
      byId("stack-health-panel").hidden = false;
      byId("stack-health-toggle").setAttribute("aria-expanded", "true");
      document.querySelector(".protection-details").open = true;
    }
    lastGuidanceState = "unavailable";
    byId("setup").hidden = true;
    byId("public-ip").hidden = true;
    byId("public-ip").textContent = "";
    byId("last-checked").textContent = "Reconnecting…";
    shell.setStatus("App connection unavailable", "error");
    for (const name of ["vpn", "firewall", "dns", "downloader"]) {
      setCheck(name, "Unavailable", "Unable to check current status.");
    }
    setBanner("Connection status unavailable", "The app could not be reached. Current VPN and protection status cannot be confirmed.", "Reconnecting", "warning");
    showHelp("Trying again automatically", "Check that the app is running in Home Assistant, or use Refresh status to try again now.");
  }

  function setBusy(busy) {
    refreshButton.disabled = busy;
    refreshButton.setAttribute("aria-busy", String(busy));
  }

  function cancelPolling() {
    clearTimeout(pollingTimer);
    pollingTimer = null;
    activeRequest?.abort();
    activeRequest = null;
    setBusy(false);
  }

  async function refresh() {
    if (!active || document.hidden || activeRequest) return;
    clearTimeout(pollingTimer);
    const request = new AbortController();
    activeRequest = request;
    setBusy(true);
    const timeout = setTimeout(() => request.abort(), REQUEST_TIMEOUT);
    try {
      const response = await fetch(statusUrl, {
        signal: request.signal,
        cache: "no-store",
        credentials: "same-origin",
        headers: { Accept: "application/json" },
        redirect: "error",
      });
      if (!response.ok) throw new Error("Status request failed");
      const status = readStatus(await response.json());
      if (activeRequest === request && active && !document.hidden) render(status);
    } catch {
      if (activeRequest === request && active && !document.hidden) renderUnavailable();
    } finally {
      clearTimeout(timeout);
      if (activeRequest === request) {
        activeRequest = null;
        setBusy(false);
        if (active && !document.hidden) pollingTimer = setTimeout(refresh, POLL_INTERVAL);
      }
    }
  }

  function resume() {
    if (document.hidden) return;
    // Refresh connection evidence after a backgrounded ingress tab resumes.
    byId("public-ip").hidden = true;
    for (const name of ["vpn", "firewall", "dns", "downloader"]) {
      setCheck(name, "Checking", "Refreshing current status.");
    }
    setBanner("Checking your connection", "Checking the VPN and protection before reporting readiness.", "Checking");
    refresh();
  }

  refreshButton.addEventListener("click", refresh);
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) cancelPolling();
    else if (active) resume();
  });
  window.addEventListener("pagehide", () => {
    active = false;
    cancelPolling();
    themeObserver?.disconnect();
    themeObserver = null;
  });
  window.addEventListener("pageshow", () => {
    if (active) return;
    active = true;
    followHomeAssistantTheme();
    resume();
  });
  followHomeAssistantTheme();
  refresh();
})();

// Media requests use parent-app settings; this sidebar does not administer them.
(() => {
  "use strict";
  const by = (id) => document.getElementById(id);
  const labels = { radarr: "Radarr", sonarr: "Sonarr", prowlarr: "Prowlarr", sabnzbd: "SABnzbd", vpn: "VPN tunnel", firewall: "Firewall", dns: "DNS", storage: "Storage", plex: "Plex" };
  const errorMessages = {
    authentication_required: "Sign in to Home Assistant again to continue.",
    permission_denied: "This Home Assistant account cannot perform that action.",
    not_configured: "Finish quality and library setup in Radarr or Sonarr, then refresh Downloads.",
    upstream_unavailable: "The media manager cannot be reached. Check its status and try again when it is available.",
    invalid_upstream_response: "The media manager returned an unrecognized response.",
    invalid_arguments: "Check the selected title and seasons.",
    invalid_scope: "The selected seasons cannot be requested. Refresh the title and choose again.",
    existing_settings_conflict: "This title uses settings that cannot be changed here. Review it in Radarr or Sonarr.",
    idempotency_conflict: "This request key is already bound to a different selection. No new request was created.",
    not_found: "This title is no longer available.",
  };
  class ApiError extends Error {
    constructor(status, code) { super(errorMessages[code] || "Downloads could not complete this action."); this.status = status; this.code = code; }
  }
  const state = {
    view: "search", kind: "all", query: "", search: [], searchNext: null, searchAbort: null, searchVersion: 0,
    selected: null, selectedVersion: 0, seasons: new Set(), mode: "search_now", detailBusy: false, detailVerified: false,
    system: null, health: null, scopedHealth: null, healthFailed: false,
    library: new Map(), libraryPages: { movie: undefined, series: undefined }, queue: [], requests: [], imports: [],
    queueNext: null, requestsNext: null, importsNext: null, importsLoaded: false, importsDepth: 1, importsLoading: false, libraryLoaded: false, activityLoaded: false, acceptedSelections: new Map(),
    feedback: null, pending: null, active: true, controllers: new Set(),
    detailOrigin: { view: "search", focusId: "search-tab" }, detailSignature: null, activitySignature: null,
    libraryStale: new Map(), queueStale: new Map(), importsStale: new Map(),
    route: null, searchScope: null, download: null, detailRequest: null, detailError: null,
    downloadIssue: null, titleIssue: null, modalAbort: null, detailLoadingVersion: null,
  };
  const responseTimings = new WeakMap();
  const evidenceDeadlines = new WeakMap();
  const latestEvidence = new Map();
  const pendingStorage = `downloads:pending:${new URL(document.baseURI).pathname}`;
  let searchTimer;
  let refreshTimer;
  let expiryTimer;
  let scopedExpiryTimer;
  let refreshing = false;
  let closingModal = false;

  const parentHashPrefix = "#downloads/";
  let navigationParent = null;
  try { if (window.parent !== window && window.parent.location.origin === location.origin) navigationParent = window.parent; } catch { /* Cross-origin embedding keeps its own route. */ }
  function navigationLocation() {
    const parentHash = navigationParent?.location.hash;
    return navigationParent && (!parentHash || parentHash.startsWith(parentHashPrefix)) ? { host: navigationParent, embedded: true } : { host: window, embedded: false };
  }
  function navigationHash(navigation = navigationLocation()) {
    const hash = navigation.host.location.hash;
    return navigation.embedded ? hash ? `#/${hash.slice(parentHashPrefix.length)}` : location.hash : hash;
  }
  function navigationEntry(navigation = navigationLocation()) { return navigation.embedded ? navigation.host.history.state?.downloadsNavigation : navigation.host.history.state; }
  function writeRoute(route, replace, navigation = navigationLocation()) {
    const ownState = routeState(route), previous = navigation.host.history.state;
    const entry = navigation.embedded ? { ...(previous && typeof previous === "object" && !Array.isArray(previous) ? previous : {}), downloadsNavigation: ownState } : ownState;
    const hash = navigation.embedded ? parentHashPrefix + ownState.hash.slice(2) : ownState.hash;
    // A fragment-only change leaves HA's route, query and unrelated state intact.
    navigation.host.history[replace ? "replaceState" : "pushState"](entry, "", hash);
  }
  const managerResourceId = /^(radarr|sonarr):([1-9][0-9]*)$/;
  const requestId = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  function canonicalNumber(value) { return /^[1-9][0-9]*$/.test(value) && Number.isSafeInteger(Number(value)) && Number(value) <= 2147483647; }
  function validManagerResourceId(value) { const match = typeof value === "string" && managerResourceId.exec(value); return Boolean(match && canonicalNumber(match[2])); }
  function queueActionId(id) { return `activity-queue-${id.replace(":", "-")}-details`; }
  function targetKey(target) { return target ? JSON.stringify(target) : ""; }
  function parseRoute(hash) {
    const fallback = { view: "search", query: "", kind: "all", target: null };
    if (!hash || hash === "#") return { route: fallback, invalid: false };
    try {
      if (hash.length > 4096 || /%(?![0-9a-f]{2})/i.test(hash)) throw new Error();
      const [path, query = "", extra] = hash.slice(1).split("?");
      if (extra !== undefined) throw new Error();
      const parts = path.split("/");
      if (parts.shift() !== "" || !["search", "library", "activity"].includes(parts[0])) throw new Error();
      const view = parts.shift(), params = new URLSearchParams(query);
      if ([...params.keys()].some((key) => !["q", "kind"].includes(key)) || new Set(params.keys()).size !== [...params.keys()].length) throw new Error();
      const text = (params.get("q") || "").trim(), kind = params.get("kind") || "all";
      if (text.length > 200 || /[\u0000-\u001f\u007f]/.test(text) || !["all", "movie", "series"].includes(kind)) throw new Error();
      let target = null;
      if (parts.length === 3 && parts[0] === "title" && ["movie", "series"].includes(parts[1]) && canonicalNumber(parts[2])) target = { kind: "title", mediaKind: parts[1], id: Number(parts[2]) };
      else if (view === "activity" && parts.length === 2 && parts[0] === "download" && validManagerResourceId(decodeURIComponent(parts[1]))) target = { kind: "download", id: decodeURIComponent(parts[1]) };
      else if (view === "activity" && parts.length === 2 && parts[0] === "request" && requestId.test(parts[1])) target = { kind: "request", id: parts[1].toLowerCase() };
      else if (parts.length) throw new Error();
      return { route: { view, query: text, kind, target }, invalid: false };
    } catch { return { route: fallback, invalid: true }; }
  }
  function routeHash(route) {
    let path = `#/${route.view}`;
    const target = route.target;
    if (target?.kind === "title") path += `/title/${target.mediaKind}/${target.id}`;
    else if (target) path += `/${target.kind}/${encodeURIComponent(target.id)}`;
    const params = new URLSearchParams();
    if (route.query) params.set("q", route.query);
    if (route.kind !== "all") params.set("kind", route.kind);
    return path + (params.size ? `?${params}` : "");
  }
  function routeState(route) { return { downloadsRoute: 1, hash: routeHash(route), modal: route.target !== null }; }
  function defaultOrigin(route) {
    if (route.target?.kind === "download") return queueActionId(route.target.id);
    if (route.target?.kind === "request") return `activity-request-${route.target.id}-details`;
    if (route.target?.kind === "title") return route.view === "search" ? `media-card-${route.target.mediaKind}-${route.target.id}` : route.view === "library" ? `library-${route.target.mediaKind}-${route.target.id}-details` : "activity-tab";
    return `${route.view}-tab`;
  }
  function navigate(route, replace = false, focusId = null) {
    const hash = routeHash(route);
    if (hash !== navigationHash()) writeRoute(route, replace);
    applyRoute(route, focusId);
  }
  function readLocation() {
    const navigation = navigationLocation();
    const { route, invalid } = parseRoute(navigationHash(navigation)), hash = routeHash(route);
    const entry = navigationEntry(navigation);
    const ownEntry = entry?.downloadsRoute === 1 && entry.hash === hash;
    // A direct/reloaded external modal link needs its own underlying tab entry,
    // so Back closes the modal instead of unexpectedly leaving Downloads.
    if (route.target && !ownEntry) {
      const base = { ...route, target: null };
      writeRoute(base, true, navigation);
      writeRoute(route, false, navigation);
    } else if (!ownEntry || navigationHash(navigation) !== hash) writeRoute(route, true, navigation);
    notice("navigation-notice", invalid ? "This Downloads link is not valid. Showing Search." : "");
    applyRoute(route);
  }
  function applyRoute(route, focusId = null) {
    const previous = state.route;
    if (previous && routeHash(previous) === routeHash(route)) return;
    // A typed query may still be waiting for its debounce when an existing
    // card or another tab is opened. It must not navigate over that selection.
    if (route.target || previous && previous.view !== route.view) clearTimeout(searchTimer);
    const targetChanged = targetKey(previous?.target) !== targetKey(route.target);
    const closing = previous?.target && !route.target;
    state.route = route; state.query = route.query; state.kind = route.kind;
    by("media-query").value = route.query;
    for (const filter of document.querySelectorAll(".media-filter")) filter.setAttribute("aria-pressed", String(filter.dataset.kind === route.kind));
    showView(route.view);
    if (route.view === "search" && state.searchScope !== `${route.kind}:${route.query}`) search();
    if (route.target && (targetChanged || !by("media-dialog").open)) {
      state.modalAbort?.abort(); ++state.selectedVersion;
      state.selected = null; state.download = null; state.detailRequest = null; state.detailError = null;
      state.downloadIssue = null; state.titleIssue = null; state.feedback = null; state.scopedHealth = null;
      state.seasons = new Set(); state.mode = "search_now"; state.detailBusy = true; state.detailVerified = false; state.detailSignature = null;
      state.detailOrigin = { view: route.view, focusId: focusId || defaultOrigin(route) };
      renderDetail();
      if (!by("media-dialog").open) by("media-dialog").showModal();
      by("media-detail-title").focus(); refreshOpenDetail();
    } else if (!route.target) {
      state.modalAbort?.abort(); ++state.selectedVersion; clearTimeout(scopedExpiryTimer);
      state.selected = null; state.download = null; state.detailRequest = null; state.detailSignature = null;
      if (by("media-dialog").open) by("media-dialog").close();
      if (closing) {
        const origin = by(state.detailOrigin.focusId);
        (origin && !origin.disabled && origin.getClientRects().length ? origin : by(`${route.view}-tab`)).focus();
      }
    }
    updateCardSelection();
    closingModal = false;
  }
  function closeDetails() {
    if (!state.route?.target || closingModal) return;
    closingModal = true;
    const navigation = navigationLocation(), entry = navigationEntry(navigation);
    if (entry?.downloadsRoute === 1 && entry.modal) navigation.host.history.back();
    else navigate({ ...state.route, target: null }, true);
  }
  function selectMedia(media) {
    if (validIdentity(media)) navigate({ ...state.route, target: { kind: "title", mediaKind: media.kind, id: idOf(media) } }, false, document.activeElement?.id);
  }
  function openDownload(id) { if (validManagerResourceId(id)) navigate({ ...state.route, target: { kind: "download", id } }, false, document.activeElement?.id); }
  function openRequest(id) { if (requestId.test(id)) navigate({ ...state.route, target: { kind: "request", id: id.toLowerCase() } }, false, document.activeElement?.id); }

  function requestKey() {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    bytes[6] = (bytes[6] & 15) | 64; bytes[8] = (bytes[8] & 63) | 128;
    const hex = [...bytes].map((value) => value.toString(16).padStart(2, "0")).join("");
    return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
  }
  function node(tag, className, text) {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text !== undefined) element.textContent = text;
    return element;
  }
  function button(text, className, action) {
    const element = node("button", className, text); element.type = "button";
    element.addEventListener("click", action); return element;
  }
  function announce(text) { by("media-announcement").textContent = text; }
  function notice(id, text) { const element = by(id); element.textContent = text || ""; element.hidden = !text; }
  function identity(media) { return `${media.kind}:${media.kind === "movie" ? media.tmdb_id : media.tvdb_id}`; }
  function idOf(media) { return media.kind === "movie" ? media.tmdb_id : media.tvdb_id; }
  function mediaRoute(media) { return `${media.kind === "movie" ? "movies" : "series"}/${idOf(media)}`; }
  function validIdentity(media) { return media && ["movie", "series"].includes(media.kind) && Number.isSafeInteger(idOf(media)) && idOf(media) > 0 && idOf(media) <= 2147483647; }
  function validRequestAvailability(media) {
    const request = media.request;
    return request && typeof request === "object" && Object.keys(request).length === 1 && ["ready", "parent_setup_required"].includes(request.state);
  }
  function validQuality(value) {
    const text = (item) => item === null || typeof item === "string" && item.length <= 300;
    return value === null || value && typeof value.name === "string" && (value.resolution === null || Number.isSafeInteger(value.resolution) && value.resolution >= 0) && text(value.source) && text(value.modifier) && (value.revision === null || Number.isSafeInteger(value.revision?.version) && value.revision.version >= 0 && Number.isSafeInteger(value.revision.real) && value.revision.real >= 0 && typeof value.revision.is_repack === "boolean");
  }
  function validMetadata(media) {
    const value = media.metadata;
    if (value === null || value === undefined) return true;
    const text = (item) => item === null || typeof item === "string" && item.length <= 300;
    if (!(value.runtime_minutes === null || Number.isSafeInteger(value.runtime_minutes) && value.runtime_minutes >= 0) || !Array.isArray(value.genres) || value.genres.length > 100 || value.genres.some((item) => typeof item !== "string" || item.length > 300) || !text(value.certification)) return false;
    if (media.kind === "series") return text(value.network) && (value.next_airing === null || typeof value.next_airing === "string" && /^\d{4}-\d\d-\d\dT.*(?:Z|\+00:00)$/.test(value.next_airing) && Number.isFinite(Date.parse(value.next_airing)));
    return text(value.studio) && (value.file === null || value.file && validQuality(value.file.quality) && (value.file.size_bytes === null || Number.isSafeInteger(value.file.size_bytes) && value.file.size_bytes >= 0) && text(value.file.video_codec) && text(value.file.audio_codec) && text(value.file.audio_channels));
  }
  function validateMedia(media) {
    if (!validIdentity(media) || typeof media.title !== "string" || typeof media.overview !== "string" ||
        !["not_added", "managed"].includes(media.library?.state) || !validRequestAvailability(media) || !validMetadata(media) || !validAcquisition(media.acquisition) ||
        (media.kind === "series" && (!Array.isArray(media.seasons) || media.seasons.some((s) => !Number.isInteger(s.number) || s.number < 0 || !["unknown", "missing", "partial", "in_progress", "imported"].includes(s.state))))) {
      throw new Error("Unrecognized media response");
    }
    return media;
  }
  function validAcquisition(value) {
    if (!value || !["unknown", "not_requested", "waiting", "downloading", "importing", "imported", "blocked", "unavailable"].includes(value.state)) return false;
    if (value.state === "downloading") return Number.isSafeInteger(value.total_bytes) && value.total_bytes >= 0 && Number.isSafeInteger(value.completed_bytes) && value.completed_bytes >= 0 && value.completed_bytes <= value.total_bytes;
    return true;
  }
  function validatePage(value, validateItem) {
    if (!value || !Array.isArray(value.items) || !Array.isArray(value.coverage) || !(value.next_page === null || typeof value.next_page === "string") ||
        value.coverage.some((c) => !Object.hasOwn(labels, c.source) || !["available", "unavailable", "unsupported", "partial"].includes(c.state) || !Number.isFinite(c.observed_at_unix_ms))) throw new Error("Unrecognized page response");
    value.items.forEach(validateItem); return value;
  }
  function validateRequest(value) {
    if (!value || typeof value.id !== "string" || !validIdentity(value.input) || !["pending", "applying", "applied", "reconciling", "failed"].includes(value.submission?.state)) throw new Error("Unrecognized request response");
    return value;
  }
  function validateQueue(value) {
    if (!value || !validManagerResourceId(value.id) || !validIdentity(value.media) || typeof value.title !== "string" || !validAcquisition(value.acquisition)) throw new Error("Unrecognized queue item");
    return value;
  }
  function validateDownload(value) {
    const nullableText = (item) => item === null || typeof item === "string" && item.length <= 300;
    const nullableNumber = (item) => item === null || Number.isSafeInteger(item) && item >= 0;
    const timestamp = (item) => item === null || typeof item === "string" && /^\d{4}-\d\d-\d\dT.*(?:Z|\+00:00)$/.test(item) && Number.isFinite(Date.parse(item));
    const strings = (items) => Array.isArray(items) && items.length <= 100 && items.every((item) => typeof item === "string" && item.length <= 300);
    if (!value || !validManagerResourceId(value.id) || !["radarr", "sonarr"].includes(value.manager) || !value.id.startsWith(`${value.manager}:`) || !validIdentity(value.media) || (value.manager === "radarr") !== (value.media.kind === "movie") ||
        typeof value.title !== "string" || !validAcquisition(value.acquisition) || !["queued", "paused", "downloading", "completed", "failed", "unknown"].includes(value.status) ||
        !(value.episode === null || Number.isInteger(value.episode?.season) && value.episode.season >= 0 && Number.isInteger(value.episode.number) && value.episode.number >= 0) ||
        !validQuality(value.quality) ||
        !(value.progress === null || [value.progress?.completed_bytes, value.progress?.total_bytes, value.progress?.remaining_bytes].every((item) => Number.isSafeInteger(item) && item >= 0) && value.progress.completed_bytes + value.progress.remaining_bytes === value.progress.total_bytes) ||
        !nullableNumber(value.remaining_seconds) || !nullableText(value.download_client) || !nullableText(value.indexer) || ![null, "usenet", "torrent", "unknown"].includes(value.protocol) ||
        !strings(value.languages) || !strings(value.custom_formats) || !(value.custom_format_score === null || Number.isSafeInteger(value.custom_format_score)) || !timestamp(value.added_at) || !timestamp(value.estimated_completion_at) ||
        !["not_started", "pending", "importing", "imported", "failed", "unknown"].includes(value.import_status) || !Number.isFinite(value.observed_at_unix_ms) ||
        !Array.isArray(value.warnings) || value.warnings.some((item) => !["download_failed", "import_failed", "import_blocked", "provider_warning", "provider_error"].includes(item.code) || !["warning", "error"].includes(item.severity) || typeof item.message !== "string")) throw new Error("Unrecognized download details");
    return value;
  }
  function validateImport(value) {
    const timestamp = (item) => typeof item === "string" && /^\d{4}-\d\d-\d\dT.*(?:Z|\+00:00)$/.test(item) && Number.isFinite(Date.parse(item));
    const observed = (item) => Number.isSafeInteger(item) && item >= 0;
    const plex = value?.plex, subtitles = value?.subtitles;
    const validPlex = plex && (plex.state === "not_configured" || plex.state === "unavailable" && ["timeout", "connection", "authentication", "invalid_response", "not_running"].includes(plex.reason) || plex.state === "unknown" && ["identity_unavailable", "file_evidence_missing", "not_matched", "ambiguous_match", "older_version"].includes(plex.reason) || plex.state === "verified" && observed(plex.observed_at_unix_ms));
    const validSubtitles = subtitles && (subtitles.state === "unknown" && ["plex_unverified", "streams_unavailable"].includes(subtitles.reason) || subtitles.state === "missing" && observed(subtitles.observed_at_unix_ms) || subtitles.state === "verified" && observed(subtitles.observed_at_unix_ms) && Array.isArray(subtitles.languages) && subtitles.languages.length <= 100 && subtitles.languages.every((language) => typeof language === "string" && language.length <= 300));
    if (!value || !validManagerResourceId(value.id) || !["radarr", "sonarr"].includes(value.manager) || !value.id.startsWith(`${value.manager}:`) || !validIdentity(value.media) || (value.manager === "radarr") !== (value.media.kind === "movie") || typeof value.title !== "string" ||
        !(value.episode === null || Number.isInteger(value.episode?.season) && value.episode.season >= 0 && Number.isInteger(value.episode.number) && value.episode.number >= 0) || !timestamp(value.imported_at) || !validQuality(value.quality) || !validPlex || !validSubtitles || !observed(value.observed_at_unix_ms)) throw new Error("Unrecognized completed import");
    return value;
  }
  async function api(path, options = {}) {
    const controller = options.controller || new AbortController();
    const startedAt = performance.now();
    state.controllers.add(controller);
    const timer = setTimeout(() => controller.abort(), 12000);
    try {
      const response = await fetch(new URL(`api/v1/${path}`, document.baseURI), {
        method: options.method || "GET", credentials: "same-origin", cache: "no-store", redirect: "error", signal: controller.signal,
        headers: { Accept: "application/json", ...(options.body ? { "Content-Type": "application/json" } : {}), ...options.headers },
        ...(options.body ? { body: JSON.stringify(options.body) } : {}),
      });
      if (!response.ok) {
        let code;
        try { code = (await response.json()).error_code; } catch { /* No upstream body is shown. */ }
        throw new ApiError(response.status, code);
      }
      const body = await response.json();
      if (body && typeof body === "object") responseTimings.set(body, { startedAt, receivedAt: performance.now() });
      return body;
    } finally { clearTimeout(timer); state.controllers.delete(controller); }
  }
  function coverageText(coverage) {
    return coverage.filter((c) => c.state !== "available").map((c) => `${labels[c.source]} ${c.state === "partial" ? "results are partial" : c.state === "unsupported" ? "is not supported" : "is unavailable"}`).join(". ");
  }
  function acquisitionLabel(acquisition) {
    switch (acquisition.state) {
      case "waiting": return "Waiting for a matching release";
      case "not_requested": return "Not requested";
      case "unknown": return "Status not verified";
      case "downloading": return "Downloading";
      case "importing": return "Importing into library";
      case "imported": return "Imported into library";
      case "unavailable": return `${labels[acquisition.component] || "Source"} unavailable`;
      case "blocked": return ({ import_failed: "Import needs attention", download_failed: "Download failed", release_rejected: "Release rejected", unknown: "Needs attention" })[acquisition.reason] || "Needs attention";
      default: return "Status not verified";
    }
  }
  function poster(media) {
    const element = node("span", "media-poster");
    const placeholder = node("span", "media-poster-placeholder"); placeholder.setAttribute("aria-hidden", "true");
    const glyph = document.createElementNS("http://www.w3.org/2000/svg", "svg"), use = document.createElementNS("http://www.w3.org/2000/svg", "use");
    glyph.setAttribute("class", "icon"); use.setAttribute("href", media.kind === "movie" ? "#icon-movie" : "#icon-tv"); glyph.append(use); placeholder.append(glyph);
    if (media.poster !== null && media.poster !== undefined) {
      // Identity-bound local artwork only. Never trust an upstream poster URL.
      const image = node("img"); image.alt = ""; image.loading = "lazy";
      image.src = new URL(`api/v1/artwork/${media.kind}/${idOf(media)}/poster`, document.baseURI).href;
      image.addEventListener("error", () => image.replaceWith(placeholder), { once: true });
      element.append(image);
    } else element.append(placeholder);
    return element;
  }
  function renderCards() {
    by("search-results").replaceChildren(...state.search.map((media) => {
      const element = button("", "media-card", () => selectMedia(media));
      element.id = `media-card-${media.kind}-${idOf(media)}`; element.dataset.mediaIdentity = identity(media);
      element.setAttribute("aria-label", `${media.title}, ${media.year || "year unknown"}, ${media.kind === "movie" ? "movie" : "TV show"}`);
      element.setAttribute("aria-pressed", String(state.selected !== null && identity(state.selected) === identity(media)));
      element.append(poster(media), node("span", "media-card-title", media.title), node("span", "media-card-meta", `${media.year || "Year unknown"} · ${media.kind === "movie" ? "Movie" : "TV show"}`));
      if (media.library.state === "managed") element.append(node("span", "media-card-state", acquisitionLabel(media.acquisition)));
      return element;
    }));
    by("search-results-count").textContent = state.search.length ? `${state.search.length} ${state.search.length === 1 ? "title" : "titles"}${state.searchNext ? "+" : ""}` : "";
    by("search-empty").hidden = state.search.length > 0;
    by("search-more").hidden = !state.searchNext;
  }
  async function search(append = false) {
    clearTimeout(searchTimer); state.searchAbort?.abort();
    const version = ++state.searchVersion;
    const query = by("media-query").value.trim(); state.query = query;
    if (!query) { state.search = []; state.searchNext = null; state.searchScope = `${state.kind}:${query}`; notice("search-notice", ""); by("search-results-label").textContent = "Search movies and TV"; by("search-empty").textContent = "Find something worth staying in for."; renderCards(); return; }
    const controller = new AbortController(); state.searchAbort = controller;
    by("search-results-label").textContent = "Searching…"; by("search-empty").textContent = "Searching your media managers…";
    if (!append) { state.search = []; state.searchNext = null; renderCards(); }
    const params = new URLSearchParams({ query, kind: state.kind, limit: "20" });
    if (append && state.searchNext) params.set("page_token", state.searchNext);
    try {
      const result = validatePage(await api(`search?${params}`, { controller }), validateMedia);
      if (version !== state.searchVersion || !state.active) return;
      state.search = append ? [...state.search, ...result.items] : result.items;
      state.searchScope = `${state.kind}:${query}`;
      state.searchNext = result.next_page;
      const incomplete = coverageText(result.coverage);
      notice("search-notice", incomplete ? `${incomplete}. Results may be incomplete.` : "");
      by("search-results-label").textContent = "Search results";
      by("search-empty").textContent = incomplete ? "No results from the available sources." : "No matching titles. Try another title or year.";
      renderCards();
    } catch (error) {
      if (version !== state.searchVersion || controller.signal.aborted || !state.active) return;
      by("search-results-label").textContent = "Search unavailable";
      by("search-empty").textContent = "No search results could be verified.";
      notice("search-notice", error instanceof ApiError ? error.message : "Search could not be reached. Try again shortly.");
    }
  }
  function updateCardSelection() {
    for (const card of by("search-results").children) card.setAttribute("aria-pressed", String(state.selected !== null && card.dataset.mediaIdentity === identity(state.selected)));
  }
  function latestReceipt(media) {
    return [...new Map([...state.requests, ...state.acceptedSelections.values()].map((record) => [record.id, record])).values()]
      .filter((record) => identity(record.input) === identity(media)).sort((a, b) => b.created_at_unix_ms - a.created_at_unix_ms)[0];
  }
  function detailStatus(media) {
    if (state.pending && identity(state.pending.input) === identity(media)) return state.pending.posting ? "Submitting request" : "Request outcome unverified";
    if (!["unknown", "not_requested"].includes(media.acquisition.state)) return acquisitionLabel(media.acquisition);
    const record = latestReceipt(media);
    if (!record) return acquisitionLabel(media.acquisition);
    if (record.submission.state === "applied") return record.submission.result?.effect === "already_satisfied" ? "Selection already satisfied in the manager" : record.submission.result?.search?.state === "queued" ? "Request accepted · search queued" : record.input.intent.mode === "monitor_only" ? "Monitoring configured" : "Request applied";
    return ({ pending: "Request accepted", applying: "Applying request", reconciling: "Checking request outcome", failed: "Request needs attention" })[record.submission.state] || acquisitionLabel(media.acquisition);
  }
  function staleAcquisitionLabel(acquisition, kind, staleSources) {
    const source = kind === "movie" ? "radarr" : "sonarr";
    const reason = staleSources.get(source);
    return reason ? `Last seen: ${acquisitionLabel(acquisition)} · ${labels[source]} ${reason === "partial" ? "results partial" : reason === "unsupported" ? "unsupported" : "unavailable"}` : acquisitionLabel(acquisition);
  }
  function updateUnknownNotice() {
    const container = by("unknown-request-notice"); if (!container) return;
    if (!state.pending) {
      if (container.contains(document.activeElement)) by("activity-tab").focus();
      container.hidden = true; container.dataset.signature = ""; return;
    }
    const pending = state.pending;
    const signature = `${pending.key}:${pending.title || ""}:${Boolean(pending.posting)}`;
    container.hidden = false;
    if (container.dataset.signature === signature) return;
    const restoreFocus = container.contains(document.activeElement);
    container.dataset.signature = signature;
    container.replaceChildren(node("p", "", pending.posting ? `Submitting “${pending.title || "the selected title"}”. Other new requests are paused until its receipt is known.` : `The submission for “${pending.title || "an earlier title"}” is unverified. Other new requests are paused to avoid duplicates. Check the original operation in Activity.`));
    const action = button("Open Activity", "media-link", () => { changeView("activity"); (by("activity-check-request") || by("activity-tab")).focus(); }); action.id = "unknown-request-activity";
    container.append(action); if (restoreFocus) action.focus();
  }
  function updatePrimaryContrast(element) {
    if (element.disabled) { element.style.removeProperty("color"); return; }
    const color = getComputedStyle(element).backgroundColor;
    if (!color.startsWith("rgb(" ) && !color.startsWith("rgba(")) return;
    const rgb = color.match(/[\d.]+/g)?.slice(0, 3).map(Number);
    if (!rgb || rgb.length !== 3) return;
    const linear = rgb.map((channel) => { const value = channel / 255; return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4; });
    const luminance = linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722;
    element.style.color = luminance > 0.179 ? "#000" : "#fff";
  }
  const reasonLabels = {
    upstream_health: "The media manager needs attention", indexer_unavailable: "No usable indexer is available",
    download_client_unavailable: "The downloader is unavailable", storage_unavailable: "The selected library storage is unavailable",
    storage_low: "The selected storage is low on space", protection_unverified: "VPN protection is not verified",
    checks_missing: "Required checks have not completed", checks_stale: "Required checks are stale",
    not_configured: "Finish quality and library setup in Radarr or Sonarr", upstream_unavailable: "A required service cannot be reached",
  };
  function targetReadiness() {
    if (!state.scopedHealth || !state.selected) return null;
    const operation = state.selected.kind === "movie" ? "request_movie" : "request_series";
    return state.scopedHealth.readiness.find((r) => r.operation === operation) || null;
  }
  function evidenceFresh(value) { return (evidenceDeadlines.get(value) ?? -Infinity) > performance.now(); }
  function requestReady() {
    const readiness = targetReadiness();
    return readiness !== null && readiness.state === "ready" && evidenceFresh(readiness);
  }
  function setEvidenceDeadline(value, subject, stamp, expiresAt, snapshot, timing) {
    const duration = Math.max(0, timing.receivedAt - timing.startedAt);
    const candidate = timing.receivedAt + Math.max(0, expiresAt - snapshot.generated_at_unix_ms - duration);
    const previous = latestEvidence.get(subject);
    let deadline = candidate;
    if (previous && stamp < previous.stamp) deadline = -Infinity;
    else if (previous && stamp === previous.stamp) deadline = Math.min(previous.deadline, candidate);
    if (!previous || stamp >= previous.stamp) latestEvidence.set(subject, { stamp, deadline });
    evidenceDeadlines.set(value, deadline);
  }
  function updateRequestButton() {
    const element = by("request-media"); if (!element) return;
    const media = state.selected;
    const eligible = media?.request?.state === "ready";
    const needsParentSetup = media?.request?.state === "parent_setup_required";
    const complete = media?.acquisition.state === "imported" && state.mode === "search_now";
    const activeMovie = media?.kind === "movie" && ["downloading", "importing", "waiting"].includes(media.acquisition.state);
    const noScope = media?.kind === "series" && state.mode === "search_now" && state.seasons.size === 0;
    const accepted = [...state.acceptedSelections.values()].some((record) => {
      const input = record.input;
      if (!media || identity(input) !== identity(media) || input.intent.mode !== state.mode || record.submission.state === "failed") return false;
      if (media.kind === "movie" || state.mode === "monitor_only") return true;
      return input.intent.scope?.kind === "seasons" && input.intent.scope.numbers.length === state.seasons.size && input.intent.scope.numbers.every((number) => state.seasons.has(number));
    });
    const unknown = state.pending !== null;
    const eligibilityNote = by("request-eligibility-note");
    if (eligibilityNote) {
      eligibilityNote.hidden = !state.detailVerified || state.detailBusy || !needsParentSetup;
      eligibilityNote.textContent = needsParentSetup ? `Add or configure this title in ${media.kind === "movie" ? "Radarr" : "Sonarr"} first, then reopen it here. Settings stay in that app.` : "";
    }
    const readinessNote = by("request-readiness-note");
    if (readinessNote) {
      readinessNote.hidden = !state.system?.capabilities.request || requestReady();
      const readiness = targetReadiness();
      readinessNote.textContent = readiness && evidenceFresh(readiness) && readiness.state === "blocked" ? `Requests are blocked: ${readiness.reasons.map((reason) => reasonLabels[reason] || "A required component needs attention").join(". ")}.` : "Request readiness is not verified. Fresh checks are needed before requesting.";
    }
    element.disabled = !state.system?.capabilities.request || state.detailBusy || !state.detailVerified || !eligible || !requestReady() || noScope || complete || activeMovie || accepted || unknown;
    updateUnknownNotice();
    element.textContent = unknown ? state.pending.posting ? "Submitting request…" : "Outcome unverified" : state.detailBusy ? "Loading title…" : complete ? "Already imported" : activeMovie ? "Already in progress" : accepted ? state.mode === "monitor_only" ? "Monitoring configured" : "Request accepted" : state.mode === "monitor_only" ? "Monitor in media manager" : media?.kind === "series" ? "Get selected seasons" : "Request movie";
    updatePrimaryContrast(element);
  }
  function bytes(value) {
    if (value === null) return "Not reported";
    const units = ["B", "KiB", "MiB", "GiB", "TiB"]; let amount = value, unit = 0;
    while (amount >= 1024 && unit < units.length - 1) { amount /= 1024; unit++; }
    return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: unit ? 1 : 0 }).format(amount)} ${units[unit]}`;
  }
  function duration(seconds) {
    if (seconds === null) return "Not reported";
    if (seconds >= 3600) return `${Math.floor(seconds / 3600)}h ${Math.floor(seconds % 3600 / 60)}m`;
    if (seconds >= 60) return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
    return `${seconds}s`;
  }
  function dateLabel(value) { return value === null ? "Not reported" : new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(value)); }
  function detailWarning(text) { const element = node("p", "media-notice", text); element.setAttribute("role", "status"); return element; }
  function qualityLabel(quality) { return quality ? [quality.name, quality.resolution === null ? null : `${quality.resolution}p`, quality.source, quality.modifier, quality.revision?.is_repack ? "Repack" : null, quality.revision && quality.revision.version > 1 ? `Revision ${quality.revision.version}` : null].filter(Boolean).join(" · ") : "Not reported"; }
  function metadataChildren(media) {
    const metadata = media.metadata;
    if (!metadata) return [];
    const facts = node("dl", "download-facts title-facts");
    const rows = [["Runtime", metadata.runtime_minutes === null ? "Not reported" : `${metadata.runtime_minutes} min`], ["Genres", metadata.genres.length ? metadata.genres.join(" · ") : "Not reported"], [media.kind === "movie" ? "Studio" : "Network", (media.kind === "movie" ? metadata.studio : metadata.network) || "Not reported"], ["Certification", metadata.certification || "Not reported"]];
    if (media.kind === "series") rows.push(["Next airing", dateLabel(metadata.next_airing)]);
    for (const [label, value] of rows) facts.append(node("dt", "", label), node("dd", "", value));
    const children = [facts];
    if (media.kind === "movie" && metadata.file) {
      const file = metadata.file, details = node("dl", "download-facts");
      children.push(node("h4", "detail-subheading", "Imported file"));
      for (const [label, value] of [["Quality", qualityLabel(file.quality)], ["Size", bytes(file.size_bytes)], ["Video", file.video_codec || "Not reported"], ["Audio", file.audio_codec || "Not reported"], ["Audio channels", file.audio_channels || "Not reported"]]) details.append(node("dt", "", label), node("dd", "", value));
      children.push(details);
    }
    return children;
  }
  function downloadChildren() {
    const download = state.download;
    if (!download) return [];
    const children = [node("h4", "detail-subheading", "Download and import")];
    if (state.downloadIssue) children.push(detailWarning(state.downloadIssue === "not_found" ? "This item is no longer in the active queue. The details below are last seen, not confirmation of completion or import." : "Download details are temporarily unavailable. The details below are last seen and may be stale."));
    children.push(node("p", "download-release", download.title));
    const status = ({ queued: "Queued", paused: "Paused", downloading: "Downloading", completed: "Transfer complete", failed: "Download failed", unknown: "Status not reported" })[download.status];
    children.push(node("p", "download-status", `${state.downloadIssue ? "Last seen: " : ""}${status}`));
    if (download.episode) children.push(node("p", "media-detail-meta", `Season ${download.episode.season} · Episode ${download.episode.number}`));
    if (download.progress && download.progress.total_bytes > 0) {
      const percent = Math.round(download.progress.completed_bytes / download.progress.total_bytes * 100);
      const progress = node("div", "media-progress"); progress.setAttribute("role", "progressbar"); progress.setAttribute("aria-label", state.downloadIssue ? "Last seen download progress" : "Download progress");
      progress.setAttribute("aria-valuemin", "0"); progress.setAttribute("aria-valuemax", "100"); progress.setAttribute("aria-valuenow", String(percent));
      const fill = node("span"); fill.style.width = `${percent}%`; progress.append(fill); children.push(progress);
    }
    const metrics = node("div", "download-metrics");
    for (const [label, value] of [["Transferred", download.progress ? `${bytes(download.progress.completed_bytes)} of ${bytes(download.progress.total_bytes)}` : "Not reported"], ["Remaining", download.progress ? bytes(download.progress.remaining_bytes) : "Not reported"], ["Time remaining", duration(download.remaining_seconds)]]) {
      const metric = node("div"); metric.append(node("span", "", label), node("strong", "", value)); metrics.append(metric);
    }
    children.push(metrics);
    const facts = node("dl", "download-facts");
    const qualityText = qualityLabel(download.quality);
    const importStatus = ({ not_started: "Not started", pending: "Waiting to import", importing: "Importing", imported: "Imported into library", failed: "Import failed", unknown: "Not reported" })[download.import_status];
    const rows = [["Quality", qualityText], ["Import", importStatus], ["Download client", download.download_client || "Not reported"], ["Indexer", download.indexer || "Not reported"], ["Protocol", download.protocol === "usenet" ? "Usenet" : download.protocol === "torrent" ? "Torrent" : "Not reported"], ["Languages", download.languages.length ? download.languages.join(", ") : "Not reported"], ["Custom formats", download.custom_formats.length ? download.custom_formats.join(", ") : "Not reported"], ["Format score", download.custom_format_score === null ? "Not reported" : String(download.custom_format_score)], ["Added", dateLabel(download.added_at)], ["Estimated completion", dateLabel(download.estimated_completion_at)]];
    for (const [label, value] of rows) facts.append(node("dt", "", label), node("dd", "", value));
    children.push(facts);
    if (download.warnings.length) {
      const warnings = node("ul", "download-warnings"); warnings.setAttribute("role", download.warnings.some((warning) => warning.severity === "error") ? "alert" : "status");
      for (const warning of download.warnings) warnings.append(node("li", "", warning.message)); children.push(warnings);
    }
    children.push(node("p", "media-disclosure", "A completed transfer is not proof of import or playback. Radarr and Sonarr report import separately."));
    return children;
  }
  function downloadSignature(value) { return value && { ...value, observed_at_unix_ms: undefined }; }
  function renderReadOnlyDetail() {
    const container = by("media-detail"), target = state.route.target, media = state.selected;
    const signature = JSON.stringify([target, media, downloadSignature(state.download), state.detailRequest, state.detailError, state.downloadIssue, state.titleIssue, state.detailBusy]);
    if (state.detailSignature === signature) return;
    state.detailSignature = signature;
    const previousFocus = container.contains(document.activeElement) ? document.activeElement.id : null, scroll = by("media-dialog").scrollTop;
    const title = node("h3", "", media?.title || state.download?.title || (state.detailRequest ? `${state.detailRequest.input.kind === "movie" ? "Movie" : "TV"} request` : state.detailError ? target.kind === "download" ? "Download details" : "Title details" : "Loading details…")); title.id = "media-detail-title"; title.tabIndex = -1;
    const children = [title];
    if (state.detailBusy && !media && !state.download && !state.detailRequest) children.push(node("p", "media-detail-meta", "Checking the native media manager…"));
    if (state.detailError && !state.download) children.push(detailWarning(state.detailError === "not_found" ? target.kind === "download" ? "This download is no longer in the active queue. It may have finished or been removed; completion has not been verified." : "This item is no longer available. The link has been kept so it can be checked again." : "Details could not be reached. This does not confirm that the download finished. Refresh details to try again."));
    if (state.detailRequest) children.push(node("p", "media-feedback", recordFeedback(state.detailRequest)));
    if (media) {
      children.push(node("p", "media-detail-meta", `${media.year || "Year unknown"} · ${detailStatus(media)}`));
      const hero = node("div", "detail-hero"); hero.append(poster(media)); if (media.overview) hero.append(node("p", "media-overview", media.overview)); children.push(hero);
      children.push(...metadataChildren(media));
      if (media.kind === "series") {
        const seasons = node("ul", "detail-season-states");
        for (const season of media.seasons) seasons.append(node("li", "", `${season.number === 0 ? "Specials" : `Season ${season.number}`} · ${({ imported: "Imported", in_progress: "In progress", missing: "Missing", partial: "Partially imported", unknown: "Not verified" })[season.state]}${season.episode_count === null ? "" : ` · ${season.imported_count ?? "?"}/${season.episode_count} imported`}`));
        if (seasons.children.length) children.push(node("h4", "detail-subheading", "Seasons"), seasons);
      }
    }
    if (state.titleIssue) children.push(detailWarning("Title information is unavailable. Download information is shown separately when it can be verified."));
    if (state.download) children.push(...downloadChildren());
    else if (state.downloadIssue && !state.detailError) children.push(detailWarning("Download activity could not be fully verified. The title status above is separate from queue completion."));
    container.replaceChildren(...children);
    if (previousFocus) (by(previousFocus) || by("media-close")).focus({ preventScroll: true });
    by("media-dialog").scrollTop = scroll;
  }
  function renderDetail() {
    const container = by("media-detail"); const media = state.selected;
    if (!state.route?.target) { state.detailSignature = null; return; }
    by("media-dialog-context").textContent = `${({ search: "Search", library: "Library", activity: "Activity" })[state.route.view]} · ${state.route.target.kind === "download" ? "Download details" : state.route.target.kind === "request" ? "Request details" : "Title details"}`;
    if (!media || state.route.target.kind !== "title" || state.route.view === "activity") { renderReadOnlyDetail(); return; }
    const signature = JSON.stringify([media, state.mode, [...state.seasons], state.feedback, state.pending, state.system?.capabilities, state.detailBusy, state.detailVerified, state.detailOrigin, latestReceipt(media), downloadSignature(state.download), state.downloadIssue, state.detailError]);
    if (state.detailSignature === signature) { updateRequestButton(); return; }
    state.detailSignature = signature;
    const previousFocus = container.contains(document.activeElement) ? document.activeElement.id : null, scroll = by("media-dialog").scrollTop;
    const children = [];
    const title = node("h3", "", media.title); title.id = "media-detail-title"; title.tabIndex = -1;
    children.push(node("p", "media-overline", media.kind === "movie" ? "Movie" : "TV show"), title, node("p", "media-detail-meta", `${media.year || "Year unknown"} · ${detailStatus(media)}`));
    const hero = node("div", "detail-hero"); hero.append(poster(media)); if (media.overview) hero.append(node("p", "media-overview", media.overview)); children.push(hero, ...metadataChildren(media));
    if (state.detailError) children.push(detailWarning("Title details are unavailable. Previously shown information may be stale; requesting is disabled until it can be checked."));
    if (state.download) children.push(...downloadChildren());
    else if (state.downloadIssue) children.push(detailWarning("Download activity could not be fully verified. Previously loaded title information is not proof of a finished download."));
    if (media.kind === "movie" && media.acquisition.state === "imported") {
      children.push(node("p", "media-disclosure", "Imported into library. Playback has not been verified."));
      container.replaceChildren(...children);
      if (previousFocus) (by(previousFocus) || by("media-detail-title")).focus({ preventScroll: true });
      by("media-dialog").scrollTop = scroll;
      return;
    }
    children.push(node("hr"));
    children.push(node("p", "media-preference-summary", `Uses ${media.kind === "movie" ? "Radarr" : "Sonarr"} settings.`));
    const modeLabel = node("label", "media-field"); modeLabel.append(node("span", "", "Request behavior"));
    const mode = node("select"); mode.id = "request-mode";
    for (const [value, text] of [["search_now", "Search now"], ["monitor_only", "Monitor only"]]) { const option = node("option", "", text); option.value = value; option.selected = state.mode === value; mode.append(option); }
    mode.disabled = state.pending !== null;
    mode.addEventListener("change", () => { state.mode = mode.value; state.feedback = null; renderDetail(); }); modeLabel.append(mode); children.push(modeLabel);
    if (media.kind === "series" && state.mode === "search_now") {
      const seasons = node("fieldset", "media-seasons"); seasons.append(node("legend", "", "Choose seasons"));
      for (const season of media.seasons) {
        const label = node("label", "media-season"); const checkbox = node("input"); checkbox.id = `request-season-${season.number}`; checkbox.type = "checkbox"; checkbox.value = String(season.number);
        checkbox.checked = state.seasons.has(season.number); checkbox.disabled = state.detailBusy || ["imported", "in_progress"].includes(season.state) || state.pending !== null;
        checkbox.addEventListener("change", () => { checkbox.checked ? state.seasons.add(season.number) : state.seasons.delete(season.number); updateRequestButton(); });
        label.append(checkbox, node("span", "", season.number === 0 ? "Specials" : `Season ${season.number}`), node("span", "media-season-note", season.state === "imported" ? "Imported" : season.state === "in_progress" ? "In progress" : season.episode_count === null ? "Episode count unknown" : `${season.imported_count ?? "?"}/${season.episode_count} imported`)); seasons.append(label);
      }
      if (!media.seasons.length) seasons.append(node("p", "media-preference-summary", "Season information is not available yet.")); children.push(seasons);
    }
    if (state.system === null) children.push(node("p", "media-preference-summary", "Request access could not be verified."));
    else if (!state.system.capabilities.request) children.push(node("p", "media-preference-summary", "This account has read-only access."));
    const eligibilityNote = node("p", "media-preference-summary"); eligibilityNote.id = "request-eligibility-note"; children.push(eligibilityNote);
    const readinessNote = node("p", "media-preference-summary"); readinessNote.id = "request-readiness-note"; children.push(readinessNote);
    const requestButton = button("Request", "media-primary", submitRequest); requestButton.id = "request-media"; children.push(requestButton);
    children.push(node("p", "media-disclosure", "Radarr and Sonarr choose releases using your saved quality rules. Imported does not mean Plex playback has been verified."));
    if (state.feedback && (!state.feedback.identity || state.feedback.identity === identity(media))) { const feedback = node("div", "media-feedback", state.feedback.text); feedback.setAttribute("role", "status"); feedback.dataset.state = state.feedback.error ? "error" : "info";
      if (state.pending) { const check = button("Check request status", "media-link", recoverPending); check.id = "detail-check-request"; feedback.append(check); if (state.pending.allowRetry && identity(state.pending.input) === identity(media)) feedback.append(retryOriginalButton("detail-retry-request")); }
      children.push(feedback); }
    container.replaceChildren(...children); updateRequestButton();
    if (previousFocus) {
      const target = by(previousFocus);
      if (target && !target.disabled) target.focus({ preventScroll: true });
      else (by("detail-check-request") || by("media-detail-title")).focus({ preventScroll: true });
    }
    by("media-dialog").scrollTop = scroll;
  }
  async function mediaDetail(media, controller) {
    const result = validateMedia(await api(mediaRoute(media), { controller }));
    if (identity(result) !== identity(media)) throw new Error("Mismatched title");
    return result;
  }
  async function downloadDetail(id, controller) {
    const result = validateDownload(await api(`queue/${encodeURIComponent(id)}`, { controller }));
    if (result.id !== id) throw new Error("Mismatched download");
    return result;
  }
  async function titleDownload(media, controller) {
    const result = validatePage(await api("queue?limit=100", { controller }), validateQueue);
    const source = media.kind === "movie" ? "radarr" : "sonarr";
    const found = result.items.find((item) => identity(item.media) === identity(media));
    if (found) return { download: await downloadDetail(found.id, controller), issue: null };
    if (result.next_page || !result.coverage.some((item) => item.source === source && item.state === "available")) return { download: null, issue: "unavailable" };
    return { download: null, issue: null };
  }
  async function refreshOpenDetail() {
    const target = state.route?.target, version = state.selectedVersion;
    if (!target || !state.active || state.detailLoadingVersion === version) return;
    const controller = new AbortController(); state.modalAbort = controller; state.detailLoadingVersion = version;
    const current = () => state.active && version === state.selectedVersion && !controller.signal.aborted;
    try {
      if (target.kind === "title") {
        const media = { kind: target.mediaKind, ...(target.mediaKind === "movie" ? { tmdb_id: target.id } : { tvdb_id: target.id }) };
        const detail = await mediaDetail(media, controller); if (!current()) return;
        state.selected = detail; state.detailVerified = true; state.detailError = null;
        state.seasons = new Set([...state.seasons].filter((number) => detail.kind === "series" && detail.seasons.some((season) => season.number === number && !["imported", "in_progress"].includes(season.state))));
        if (detail.library.state === "managed") {
          try {
            const activity = await titleDownload(detail, controller); if (!current()) return;
            if (activity.download) state.download = activity.download;
            state.downloadIssue = activity.issue || (!activity.download && state.download ? "not_found" : null);
          } catch (error) { if (current()) state.downloadIssue = error instanceof ApiError && error.status === 404 ? "not_found" : "unavailable"; }
        }
      } else if (target.kind === "download") {
        try {
          const download = await downloadDetail(target.id, controller); if (!current()) return;
          state.download = download; state.downloadIssue = null; state.detailError = null; state.detailVerified = true;
        } catch (error) {
          if (!current()) return;
          state.detailError = error instanceof ApiError && error.status === 404 ? "not_found" : "unavailable"; state.downloadIssue = state.detailError;
        }
        if (state.download) {
          try { const media = await mediaDetail(state.download.media, controller); if (!current()) return; state.selected = media; state.titleIssue = null; }
          catch { if (current()) state.titleIssue = "unavailable"; }
        }
      } else {
        const record = validateRequest(await api(`requests/${encodeURIComponent(target.id)}`, { controller })); if (!current()) return;
        if (record.id !== target.id) throw new Error("Mismatched request");
        state.detailRequest = record; state.detailError = null; state.detailVerified = true;
        try { const media = await mediaDetail(record.input, controller); if (!current()) return; state.selected = media; state.titleIssue = null; }
        catch { if (current()) state.titleIssue = "unavailable"; }
        try {
          const activity = await titleDownload(record.input, controller); if (!current()) return;
          if (activity.download) state.download = activity.download;
          state.downloadIssue = activity.issue || (!activity.download && state.download ? "not_found" : null);
        } catch { if (current()) state.downloadIssue = "unavailable"; }
      }
    } catch (error) {
      if (!current()) return;
      state.detailError = error instanceof ApiError && error.status === 404 ? "not_found" : "unavailable";
      state.detailVerified = false;
    } finally {
      if (version === state.selectedVersion) state.detailLoadingVersion = null;
      if (current()) {
        state.detailBusy = false; renderDetail(); updateCardSelection();
        if (target.kind === "title" && state.detailVerified) await refreshScopedHealth();
      }
    }
  }
  async function refreshScopedHealth() {
    clearTimeout(scopedExpiryTimer);
    const version = state.selectedVersion;
    if (!state.selected || state.route?.target?.kind !== "title" || state.route.view === "activity") { updateRequestButton(); return; }
    try {
      const result = await api(`health/stack?${new URLSearchParams({ operation: state.selected.kind === "movie" ? "request_movie" : "request_series" })}`);
      if (version !== state.selectedVersion || !state.active) return;
      state.scopedHealth = validateHealth(result); updateRequestButton();
      const readiness = targetReadiness();
      const deadline = readiness && evidenceDeadlines.get(readiness);
      if (Number.isFinite(deadline) && deadline > performance.now()) scopedExpiryTimer = setTimeout(updateRequestButton, Math.ceil(deadline - performance.now()));
    } catch { if (version === state.selectedVersion) { state.scopedHealth = null; updateRequestButton(); } }
  }
  function storePending() {
    try { state.pending ? sessionStorage.setItem(pendingStorage, JSON.stringify(state.pending)) : sessionStorage.removeItem(pendingStorage); } catch { /* Requests still stay bound in the backend if session storage is disabled. */ }
  }
  function recordFeedback(record) {
    const submission = record.submission;
    if (submission.state === "applied") return submission.result?.effect === "already_satisfied" ? "The manager reports this selection is already satisfied. Playback has not been verified." : submission.result?.search?.state === "queued" ? "Request accepted and search queued. This is not downloaded yet." : "Monitoring settings applied. No initial download search was requested.";
    if (submission.state === "failed") return errorMessages[submission.error?.code] || "The request needs attention. No automatic retry was made.";
    if (submission.state === "reconciling") return "Checking the upstream outcome. No duplicate request will be submitted.";
    return "Request accepted. Waiting for the media manager; this is not downloaded yet.";
  }
  function applyReceipt(record) {
    validateRequest(record);
    state.requests = state.requests.filter((r) => r.id !== record.id).concat(record);
    state.acceptedSelections.set(record.id, record);
    if (state.pending) { state.pending = null; storePending(); }
    state.feedback = { text: recordFeedback(record), error: record.submission.state === "failed", identity: identity(record.input) };
    renderDetail(); renderActivity(); updateUnknownNotice(); announce(state.feedback.text);
  }
  async function recoverPending() {
    const pending = state.pending; if (!pending) return;
    try {
      const record = await api(pending.id ? `requests/${encodeURIComponent(pending.id)}` : `request-keys/${encodeURIComponent(pending.key)}`);
      if (state.pending !== pending || !state.active) return; applyReceipt(record);
    } catch (error) {
      if (state.pending !== pending || !state.active) return;
      if (error instanceof ApiError && error.status === 404) { pending.uncertain = true; pending.allowRetry = !pending.retiredFormat; storePending(); state.feedback = { text: pending.retiredFormat ? "This earlier submission uses a retired format. Keep checking its original receipt; it will not be resubmitted or replaced automatically." : "No receipt is available yet. The original submission may still be running. Check again, or explicitly retry the exact original request with its original key.", identity: identity(pending.input) }; }
      else { pending.uncertain = true; pending.allowRetry = !pending.retiredFormat; storePending(); state.feedback = { text: "The request outcome is still unknown. Check status again when Downloads is reachable. No new request was submitted.", error: true, identity: identity(pending.input) }; }
      renderDetail(); renderActivity(); updateUnknownNotice(); announce(state.feedback.text);
    }
  }
  function retryOriginalButton(id) {
    const element = button("Retry original request", "media-link", retryOriginalRequest);
    element.id = id;
    element.disabled = !state.system?.capabilities.request || state.pending?.posting === true || state.pending?.retiredFormat === true;
    return element;
  }
  async function retryOriginalRequest() {
    const pending = state.pending;
    if (!pending || !pending.allowRetry || pending.posting || pending.retiredFormat || !state.system?.capabilities.request) return;
    pending.uncertain = true; pending.allowRetry = false; pending.posting = true; storePending();
    state.feedback = { text: "Retrying only the original selection with its original request key.", identity: identity(pending.input) };
    renderDetail(); renderActivity();
    await postPending(pending);
  }
  async function submitRequest() {
    const media = state.selected; if (!media || media.request?.state !== "ready" || by("request-media")?.disabled || state.pending) return;
    if (!requestReady()) { updateRequestButton(); return; }
    const input = media.kind === "movie" ? { kind: "movie", tmdb_id: media.tmdb_id, intent: { mode: state.mode } } : { kind: "series", tvdb_id: media.tvdb_id, intent: state.mode === "monitor_only" ? { mode: "monitor_only" } : { mode: "search_now", scope: { kind: "seasons", numbers: [...state.seasons].sort((a, b) => a - b) } } };
    const pending = { key: requestKey(), input, title: media.title, posting: true, allowRetry: false, uncertain: true }; state.pending = pending; storePending(); state.feedback = { text: "Submitting this exact selection…", identity: identity(input) }; renderDetail();
    await postPending(pending, true);
  }
  async function postPending(pending, originalAttempt = false) {
    const priorUncertainty = !originalAttempt && pending.uncertain === true;
    try { const record = await api("requests", { method: "POST", body: pending.input, headers: { "Idempotency-Key": pending.key } }); if (state.pending === pending) applyReceipt(record); }
    catch (error) {
      if (state.pending !== pending || !state.active) return;
      if (error instanceof ApiError && error.status < 500 && priorUncertainty) {
        pending.posting = false; pending.allowRetry = !pending.retiredFormat; storePending();
        state.feedback = { text: "This retry was rejected, but the original submission still has an unverified outcome. Its original key and selection were kept. Check its receipt again; no new request was created.", error: true, identity: identity(pending.input) };
        renderDetail(); renderActivity();
      } else if (error instanceof ApiError && error.status < 500) {
        state.pending = null; storePending();
        state.feedback = { text: error.message, error: true };
        renderDetail();
      } else { pending.uncertain = true; pending.posting = false; pending.allowRetry = !pending.retiredFormat; storePending(); state.feedback = { text: "The connection ended before the request outcome was known. Checking the original request, without submitting it again.", identity: identity(pending.input) }; renderDetail(); await recoverPending(); }
    } finally { if (state.pending === pending) { pending.posting = false; storePending(); } }
  }
  function progressElement(acquisition, lastSeen = false) {
    if (acquisition.state !== "downloading" || acquisition.total_bytes === 0) return null;
    const percent = Math.min(100, Math.max(0, Math.round(acquisition.completed_bytes / acquisition.total_bytes * 100)));
    const progress = node("div", "media-progress"); progress.setAttribute("role", "progressbar"); progress.setAttribute("aria-label", lastSeen ? "Last seen download progress" : "Download progress"); progress.setAttribute("aria-valuenow", String(percent)); progress.setAttribute("aria-valuemin", "0"); progress.setAttribute("aria-valuemax", "100");
    const fill = node("span"); fill.style.width = `${percent}%`; progress.append(fill); return progress;
  }
  function mediaRow(media, description) {
    const row = node("article", "media-row"); row.append(poster(media));
    const copy = node("div", "media-row-copy"); copy.append(node("h3", "", media.title), node("p", "", description));
    const progress = progressElement(media.acquisition, state.libraryStale.has(media.kind === "movie" ? "radarr" : "sonarr")); if (progress) copy.append(progress);
    const action = button("View details", "media-link", () => selectMedia(media)); action.id = `library-${media.kind}-${idOf(media)}-details`;
    copy.append(action); row.append(copy, node("span", "media-row-state", staleAcquisitionLabel(media.acquisition, media.kind, state.libraryStale))); return row;
  }
  function renderLibrary() {
    const items = [...state.library.values()].sort((a, b) => a.title.localeCompare(b.title));
    by("library-list").replaceChildren(...items.map((media) => mediaRow(media, `${media.year || "Year unknown"} · ${media.kind === "movie" ? "Movie" : "TV show"}`)));
    by("library-empty").hidden = items.length > 0;
    by("library-empty").textContent = state.libraryLoaded ? "No titles were returned by the available media managers." : "Loading your library…";
    by("library-more").hidden = !Object.values(state.libraryPages).some((token) => typeof token === "string");
  }
  async function loadLibrary(append = false) {
    const kinds = ["movie", "series"].filter((kind) => !append || typeof state.libraryPages[kind] === "string");
    const results = await Promise.allSettled(kinds.map(async (kind) => {
      const params = new URLSearchParams({ limit: "20" }); if (append) params.set("page_token", state.libraryPages[kind]);
      const result = validatePage(await api(`${kind === "movie" ? "movies" : "series"}?${params}`), validateMedia);
      return { kind, result };
    }));
    if (!state.active) return;
    const messages = [];
    for (let i = 0; i < results.length; i++) {
      const item = results[i];
      if (item.status === "rejected") { state.libraryStale.set(kinds[i] === "movie" ? "radarr" : "sonarr", "unavailable"); messages.push(`${kinds[i] === "movie" ? "Movie" : "TV"} library unavailable; previously loaded titles may be stale`); continue; }
      const { kind, result } = item.value; const incomplete = coverageText(result.coverage); if (incomplete) messages.push(incomplete);
      const source = kind === "movie" ? "radarr" : "sonarr";
      const sourceState = result.coverage.find((c) => c.source === source)?.state || "unavailable";
      sourceState === "available" ? state.libraryStale.delete(source) : state.libraryStale.set(source, sourceState);
      if (!append && result.coverage.some((c) => c.source === source && c.state === "available")) {
        for (const [key, media] of state.library) if (media.kind === kind) state.library.delete(key);
      }
      for (const media of result.items) state.library.set(identity(media), media);
      state.libraryPages[kind] = result.next_page;
    }
    state.libraryLoaded = true; notice("library-notice", messages.length ? `${messages.join(". ")}. Results may be incomplete.` : ""); renderLibrary();
  }
  function renderActivity() {
    const signature = JSON.stringify([state.queue, state.requests, state.imports, state.queueNext, state.requestsNext, state.importsNext, state.pending, [...state.queueStale], [...state.importsStale], [...state.library.values()], state.search]);
    if (signature === state.activitySignature) return;
    state.activitySignature = signature;
    const previousFocus = by("activity-panel").contains(document.activeElement) ? document.activeElement.id : null;
    const queuedIdentities = new Set(state.queue.map((q) => identity(q.media)));
    const queueRows = state.queue.map((item) => {
      const media = state.library.get(identity(item.media)) || state.search.find((m) => identity(m) === identity(item.media));
      const row = node("article", "media-row");
      if (media) row.append(poster(media));
      const copy = node("div", "media-row-copy"); copy.append(node("h3", "", item.title));
      if (item.episode) copy.append(node("p", "", `Season ${item.episode.season}, episode ${item.episode.number}`));
      const progress = progressElement(item.acquisition, state.queueStale.has(item.media.kind === "movie" ? "radarr" : "sonarr")); if (progress) copy.append(progress);
      const action = button("View details", "media-link", () => openDownload(item.id)); action.id = queueActionId(item.id); copy.append(action);
      row.append(copy, node("span", "media-row-state", staleAcquisitionLabel(item.acquisition, item.media.kind, state.queueStale))); return row;
    });
    const requestRows = [...state.requests].sort((a, b) => b.created_at_unix_ms - a.created_at_unix_ms).filter((record) => !queuedIdentities.has(identity(record.input))).map((record) => {
      const media = state.library.get(identity(record.input)) || state.search.find((m) => identity(m) === identity(record.input)) || (state.selected && identity(state.selected) === identity(record.input) ? state.selected : null);
      const row = node("article", "media-row"); const copy = node("div", "media-row-copy");
      copy.append(node("h3", "", media?.title || `${record.input.kind === "movie" ? "Movie" : "TV"} request`), node("p", "", recordFeedback(record)));
      const action = button("View details", "media-link", () => openRequest(record.id)); action.id = `activity-request-${record.id}-details`; copy.append(action);
      row.append(copy, node("span", "media-row-state", ({ pending: "Accepted", applying: "Applying request", applied: "Request applied", reconciling: "Checking outcome", failed: "Needs attention" })[record.submission.state])); return row;
    });
    const unresolved = [];
    if (state.pending && !state.pending.id) {
      const pending = state.pending, row = node("article", "media-row"), copy = node("div", "media-row-copy");
      copy.append(node("h3", "", pending.title || "Unverified media request"), node("p", "", pending.retiredFormat ? "An earlier app version submitted this request. Only its receipt can be checked here; no replacement will be created." : "The original submission may still be running. No new request will be created automatically."));
      const check = button("Check request status", "media-link", recoverPending); check.id = "activity-check-request"; copy.append(check);
      if (pending.allowRetry) copy.append(retryOriginalButton("activity-retry-request"));
      row.append(copy, node("span", "media-row-state", "Outcome unverified")); unresolved.push(row);
    }
    const completedRows = [...state.imports].sort((a, b) => Date.parse(b.imported_at) - Date.parse(a.imported_at) || a.id.localeCompare(b.id)).map((item) => {
      const media = state.library.get(identity(item.media)) || state.search.find((title) => identity(title) === identity(item.media));
      const stale = state.importsStale.get(item.manager);
      const row = node("article", "media-row completed-row"); if (media) row.append(poster(media));
      const copy = node("div", "media-row-copy"); copy.append(node("h3", "", item.title));
      copy.append(node("p", "", `${item.episode ? `Season ${item.episode.season}, episode ${item.episode.number} · ` : ""}Imported ${dateLabel(item.imported_at)}`));
      copy.append(node("p", "", `Quality: ${qualityLabel(item.quality)}`));
      const prefix = stale ? "Last seen · " : "";
      const plexReason = ({ timeout: "timed out", connection: "connection unavailable", authentication: "authentication required", invalid_response: "unrecognized response", not_running: "not running", identity_unavailable: "title identity unavailable", file_evidence_missing: "file evidence missing", not_matched: "not matched", ambiguous_match: "ambiguous match", older_version: "only an older version found" })[item.plex.reason];
      const plex = item.plex.state === "verified" ? "Plex library: verified" : item.plex.state === "not_configured" ? "Plex library: not configured" : `Plex library: ${item.plex.state === "unavailable" ? "unavailable" : "not verified"} (${plexReason})`;
      const subtitles = item.subtitles.state === "verified" ? `Subtitles: verified${item.subtitles.languages.length ? ` (${item.subtitles.languages.join(", ")})` : ""}` : item.subtitles.state === "missing" ? "Subtitles: not found" : `Subtitles: not verified (${item.subtitles.reason === "plex_unverified" ? "Plex library unverified" : "stream information unavailable"})`;
      copy.append(node("p", "completed-evidence", prefix + plex), node("p", "completed-evidence", prefix + subtitles));
      // Import IDs identify native history rows, not active queue rows. Details
      // always open the canonical title and cannot initiate another acquisition.
      const action = button("View title", "media-link", () => selectMedia(item.media)); action.id = `completed-${item.id.replace(":", "-")}-details`; copy.append(action);
      row.append(copy, node("span", "media-row-state", stale ? `Last seen: Imported · ${labels[item.manager]} ${stale === "partial" ? "results partial" : "unavailable"}` : "Imported")); return row;
    });
    by("completed-list").replaceChildren(...completedRows);
    by("completed-empty").hidden = completedRows.length > 0;
    by("completed-empty").textContent = state.importsLoaded ? state.importsStale.size ? "No completed imports could be verified from all sources. Results may be incomplete." : "No completed imports were returned by Radarr or Sonarr." : "Loading completed imports…";
    by("completed-more").hidden = !state.importsNext;
    by("completed-more").disabled = state.importsLoading;
    by("activity-list").replaceChildren(...unresolved, ...queueRows, ...requestRows);
    by("activity-empty").hidden = unresolved.length + queueRows.length + requestRows.length > 0;
    by("activity-empty").textContent = state.activityLoaded ? "No activity was returned by the available sources." : "Loading activity…";
    by("activity-more").hidden = !state.queueNext && !state.requestsNext;
    const count = unresolved.length + state.queue.length + state.requests.filter((r) => ["pending", "applying", "reconciling"].includes(r.submission.state) && !queuedIdentities.has(identity(r.input))).length;
    by("activity-count").textContent = String(count); by("activity-count").hidden = count === 0;
    if (previousFocus) (by(previousFocus) || by("activity-tab")).focus();
  }
  async function readImports(append) {
    const pages = append ? 1 : state.importsDepth;
    const items = new Map(), coverage = new Map();
    const severity = { available: 0, partial: 1, unsupported: 2, unavailable: 3 };
    let next = append ? state.importsNext : null;
    // Refetch the depth explicitly opened by the user, using current cursors.
    // Each read stays bounded; no arbitrary total cap discards later pages.
    for (let page = 0; page < pages; page++) {
      if (!state.active) throw new DOMException("Downloads is not active", "AbortError");
      const params = new URLSearchParams({ limit: "20", ...(next ? { page_token: next } : {}) });
      const result = validatePage(await api(`imports?${params}`), validateImport);
      for (const item of result.items) items.set(item.id, item);
      for (const source of ["radarr", "sonarr"]) {
        const observation = result.coverage.find((item) => item.source === source) || { source, state: "unavailable", observed_at_unix_ms: 0 };
        const previous = coverage.get(source);
        if (!previous || severity[observation.state] > severity[previous.state]) coverage.set(source, observation);
      }
      next = result.next_page;
      if (!next) break;
    }
    return { items: [...items.values()], coverage: [...coverage.values()], next_page: next };
  }
  async function loadActivity(append = false, collection = "active") {
    const paths = [];
    if ((!append || collection === "active") && (!append || state.queueNext)) paths.push({ kind: "queue", path: `queue?${new URLSearchParams({ limit: "20", ...(append ? { page_token: state.queueNext } : {}) })}` });
    if ((!append || collection === "active") && (!append || state.requestsNext)) paths.push({ kind: "requests", path: `requests?${new URLSearchParams({ limit: "20", ...(append ? { page_token: state.requestsNext } : {}) })}` });
    const includesImports = !state.importsLoading && (!append || collection === "completed") && (!append || state.importsNext !== null);
    const restoreImportsFocus = includesImports && document.activeElement === by("completed-more");
    if (includesImports) { paths.push({ kind: "imports" }); state.importsLoading = true; by("completed-more").disabled = true; }
    try {
      const results = await Promise.allSettled(paths.map(async ({ kind, path }) => ({ kind, result: kind === "imports" ? await readImports(append) : validatePage(await api(path), kind === "requests" ? validateRequest : validateQueue) })));
      if (!state.active) return; const messages = [], completedMessages = [];
      for (let i = 0; i < results.length; i++) {
        const item = results[i], failedKind = paths[i].kind;
        if (item.status === "rejected") {
          if (failedKind === "queue" || failedKind === "imports") for (const source of ["radarr", "sonarr"]) (failedKind === "imports" ? state.importsStale : state.queueStale).set(source, "unavailable");
          if (failedKind === "imports") { state.importsLoaded = true; completedMessages.push("Completed imports unavailable; previously loaded imports and Plex/subtitle checks are last seen, not current"); }
          else messages.push(`${failedKind === "queue" ? "Download queue" : "Request history"} unavailable; previous results may be stale`);
          continue;
        }
        const { kind, result } = item.value; const incomplete = coverageText(result.coverage); if (incomplete) (kind === "imports" ? completedMessages : messages).push(incomplete);
        if (kind === "queue" || kind === "imports") {
          const stale = kind === "imports" ? state.importsStale : state.queueStale;
          for (const source of ["radarr", "sonarr"]) { const sourceState = result.coverage.find((coverage) => coverage.source === source)?.state || "unavailable"; sourceState === "available" ? stale.delete(source) : stale.set(source, sourceState); }
          const previous = kind === "imports" ? state.imports : state.queue;
          const keep = append ? previous : previous.filter((row) => stale.has(row.media.kind === "movie" ? "radarr" : "sonarr"));
          const items = [...new Map([...keep, ...result.items].map((row) => [row.id, row])).values()];
          if (kind === "imports") { state.imports = items; state.importsNext = result.next_page; state.importsLoaded = true; if (append) state.importsDepth++; }
          else { state.queue = items; state.queueNext = result.next_page; }
        } else { state.requests = [...new Map([...(append ? state.requests : []), ...result.items].map((record) => [record.id, record])).values()]; for (const record of result.items) if (state.acceptedSelections.has(record.id)) state.acceptedSelections.set(record.id, record); state.requestsNext = result.next_page; }
      }
      state.activityLoaded = true;
      if (!append || collection === "active") notice("activity-notice", messages.length ? `${messages.join(". ")}. Results may be incomplete.` : "");
      if (includesImports) notice("completed-notice", completedMessages.length ? `${completedMessages.join(". ")}. Results may be incomplete.` : "");
      renderActivity(); renderDetail();
    } finally {
      if (includesImports) {
        const more = by("completed-more"); state.importsLoading = false; more.disabled = false;
        if (restoreImportsFocus && (document.activeElement === document.body || document.activeElement === more)) (more.hidden ? by("completed-title") : more).focus({ preventScroll: true });
      }
    }
  }
  function validateHealth(value) {
    if (!value || !Array.isArray(value.components) || !Array.isArray(value.readiness) || !Number.isFinite(value.generated_at_unix_ms) ||
        value.components.some((c) => !Object.hasOwn(labels, c.component) || !["never_checked", "observed"].includes(c.observation?.state) || c.observation.state === "observed" && (!["fresh", "stale"].includes(c.observation.freshness) || !["healthy", "degraded", "unavailable", "not_configured"].includes(c.observation.outcome?.state) || (!Number.isFinite(c.observation.observed_at_unix_ms) || !Number.isFinite(c.observation.expires_at_unix_ms)))) ||
        value.readiness.some((r) => !["request_movie", "request_series", "transfer", "import_movie", "import_series"].includes(r.operation) || !["ready", "blocked"].includes(r.state) || (!Number.isFinite(r.expires_at_unix_ms) || !Array.isArray(r.reasons) || r.reasons.some((reason) => !Object.hasOwn(reasonLabels, reason))))) throw new Error("Unrecognized health response");
    const timing = responseTimings.get(value);
    if (!timing) throw new Error("Missing health response timing");
    for (const { component, observation } of value.components) {
      if (observation.state === "observed") setEvidenceDeadline(observation, `component:${component}`, observation.observed_at_unix_ms, observation.expires_at_unix_ms, value, timing);
    }
    for (const readiness of value.readiness) setEvidenceDeadline(readiness, `operation:${readiness.operation}`, value.generated_at_unix_ms, readiness.expires_at_unix_ms, value, timing);
    return value;
  }
  function renderHealth() {
    const health = state.health;
    const rows = health ? health.components.map(({ component, observation }) => {
      const element = node("div", "stack-health-item"); element.append(node("strong", "", labels[component]));
      const stale = state.healthFailed || observation.state !== "observed" || observation.freshness === "stale" || !evidenceFresh(observation);
      const outcome = observation.state === "observed" ? observation.outcome.state : "never_checked";
      element.dataset.state = stale ? "unknown" : outcome === "healthy" ? "healthy" : "degraded";
      element.append(node("span", "", stale ? observation.state === "never_checked" ? "Not checked" : "Status stale" : ({ healthy: "Healthy", degraded: "Needs attention", unavailable: "Unavailable", not_configured: "Not configured" })[outcome]));
      if (!stale && outcome === "degraded" && Array.isArray(observation.outcome.issues)) element.append(node("span", "", observation.outcome.issues.map((issue) => typeof issue.message === "string" && issue.message.trim() ? issue.message.trim().slice(0, 240) : ({ indexer_unavailable: "Indexer unavailable", download_client_unavailable: "Downloader unavailable", storage_unavailable: "Storage unavailable", storage_low: "Storage low", protection_unverified: "Protection unverified", checks_stale: "Checks stale", upstream_health: "Upstream needs attention" })[issue.code] || "Check component settings").join(" · ")));
      return element;
    }) : [];
    by("stack-health-components").replaceChildren(...rows);
    const readiness = health?.readiness || [];
    const stale = state.healthFailed || readiness.length === 0 || readiness.some((r) => !evidenceFresh(r));
    const blocked = readiness.some((r) => r.state === "blocked");
    const tone = stale ? "unknown" : blocked ? "degraded" : "healthy";
    by("stack-health-toggle").dataset.state = tone;
    by("stack-health-label").textContent = stale ? health ? "Download stack status stale" : "Download stack unavailable" : blocked ? "Download stack needs attention" : "Download stack healthy";
    by("stack-health-freshness").textContent = health ? `Snapshot received ${Math.max(0, Math.floor((performance.now() - responseTimings.get(health).receivedAt) / 1000))} seconds ago` : "Not verified";
    by("stack-health-note").textContent = stale ? "Current readiness cannot be confirmed. Browsing remains available; requests require fresh target-specific checks." : blocked ? "Some operations need attention. Requests use the checks for their media manager; optional services do not block unrelated work." : "Requesting, downloading, and importing are available. Playback is checked separately.";
    updateRequestButton();
  }
  async function refreshHealth() {
    try { const result = validateHealth(await api("health/stack")); if (!state.active) return; state.health = result; state.healthFailed = false; }
    catch { if (!state.active) return; state.healthFailed = true; state.scopedHealth = null; }
    renderHealth(); await refreshScopedHealth();
  }
  function changeView(view) { navigate({ ...state.route, view, query: by("media-query").value.trim(), target: null }); }
  function showView(view) {
    state.view = view;
    for (const name of ["search", "library", "activity"]) { by(`${name}-tab`).setAttribute("aria-selected", String(name === view)); by(`${name}-tab`).tabIndex = name === view ? 0 : -1; by(`${name}-panel`).hidden = name !== view; }
    if (view === "library" && !state.libraryLoaded) loadLibrary();
    if (view === "activity" && !state.activityLoaded) loadActivity();
  }
  async function refreshMedia() {
    if (!state.active || document.hidden || refreshing) return;
    refreshing = true;
    try {
      await refreshHealth();
      if (state.view === "activity") await loadActivity();
      if (state.route?.target) await refreshOpenDetail();
      if (state.pending) await recoverPending();
    } finally { refreshing = false; if (state.active && !document.hidden) refreshTimer = setTimeout(refreshMedia, 5000); }
  }
  async function initialize() {
    try {
      const system = await api("system");
      if (!system || typeof system.capabilities?.read !== "boolean" || typeof system.capabilities?.request !== "boolean" || typeof system.capabilities?.admin !== "boolean") throw new Error("Unrecognized system response");
      state.system = system;
      renderDetail();
    } catch { notice("search-notice", "Downloads could not verify account access. Browsing remains available; requests stay disabled."); }
    if (state.active) refreshMedia();
  }
  const unknownNotice = node("div", "media-notice"); unknownNotice.id = "unknown-request-notice"; unknownNotice.hidden = true; unknownNotice.setAttribute("role", "status");
  by("search-panel").parentElement.prepend(unknownNotice);
  const searchMore = button("Load more results", "media-secondary", () => search(true)); searchMore.id = "search-more"; searchMore.hidden = true; by("search-results-container").append(searchMore);
  function searchRoute() { const route = { view: "search", query: by("media-query").value.trim(), kind: state.kind, target: null }; if (routeHash(route) === navigationHash()) search(); else navigate(route, true); }
  by("media-search-form").addEventListener("submit", (event) => { event.preventDefault(); clearTimeout(searchTimer); searchRoute(); });
  by("media-query").addEventListener("input", () => { clearTimeout(searchTimer); state.searchAbort?.abort(); ++state.searchVersion; searchTimer = setTimeout(searchRoute, 300); });
  for (const filter of document.querySelectorAll(".media-filter")) filter.addEventListener("click", () => navigate({ view: "search", query: by("media-query").value.trim(), kind: filter.dataset.kind, target: null }, true));
  const tabs = [...document.querySelectorAll(".media-tab")];
  for (const tab of tabs) {
    tab.addEventListener("click", () => changeView(tab.dataset.view));
    tab.addEventListener("keydown", (event) => { const index = tabs.indexOf(tab); let next;
      if (event.key === "ArrowRight") next = (index + 1) % tabs.length; else if (event.key === "ArrowLeft") next = (index + tabs.length - 1) % tabs.length; else if (event.key === "Home") next = 0; else if (event.key === "End") next = tabs.length - 1;
      if (next !== undefined) { event.preventDefault(); tabs[next].focus(); changeView(tabs[next].dataset.view); }
    });
  }
  by("stack-health-toggle").addEventListener("click", () => { const panel = by("stack-health-panel"); panel.hidden = !panel.hidden; by("stack-health-toggle").setAttribute("aria-expanded", String(!panel.hidden)); });
  by("library-more").addEventListener("click", () => loadLibrary(true)); by("activity-more").addEventListener("click", () => loadActivity(true)); by("completed-more").addEventListener("click", () => loadActivity(true, "completed"));
  by("refresh").addEventListener("click", async () => { clearTimeout(refreshTimer); await refreshMedia(); if (state.view === "library") await loadLibrary(); if (state.query) await search(); });
  function stop() { state.active = false; clearTimeout(refreshTimer); clearTimeout(searchTimer); clearInterval(expiryTimer); clearTimeout(scopedExpiryTimer); state.modalAbort?.abort(); state.controllers.forEach((controller) => controller.abort()); }
  function start() { if (document.hidden) return; state.active = true; clearInterval(expiryTimer); expiryTimer = setInterval(renderHealth, 1000); if (state.route?.target) refreshOpenDetail(); refreshMedia(); }
  document.addEventListener("visibilitychange", () => { document.hidden ? stop() : start(); });
  window.addEventListener("pagehide", stop); window.addEventListener("pageshow", () => { if (!state.active) start(); });
  try { const saved = JSON.parse(sessionStorage.getItem(pendingStorage) || "null"); if (saved && typeof saved.key === "string" && /^[a-f0-9-]{36}$/.test(saved.key) && validIdentity(saved.input)) { saved.posting = false; saved.uncertain = true; saved.retiredFormat = Object.hasOwn(saved.input, "profile"); if (saved.retiredFormat) saved.allowRetry = false; state.pending = saved; notice("activity-notice", "An earlier request has an unverified outcome. Its original receipt will be checked without submitting it again."); } } catch { /* Ignore malformed local state. */ }
  by("media-close").addEventListener("click", closeDetails);
  by("media-dialog").addEventListener("cancel", (event) => { event.preventDefault(); closeDetails(); });
  by("media-dialog").addEventListener("keydown", (event) => {
    if (event.key !== "Tab") return;
    // Native modal inertness protects the page underneath. Keep keyboard
    // traversal inside the dialog too, including from its focused heading.
    const controls = [...by("media-dialog").querySelectorAll("button:not([disabled]), input:not([disabled]), select:not([disabled]), a[href], [tabindex]:not([tabindex='-1'])")].filter((element) => element.getClientRects().length > 0);
    if (!controls.length) { event.preventDefault(); by("media-detail-title").focus(); return; }
    const index = controls.indexOf(document.activeElement);
    if (index === -1 || event.shiftKey && index === 0 || !event.shiftKey && index === controls.length - 1) {
      event.preventDefault(); controls[event.shiftKey ? controls.length - 1 : 0].focus();
    }
  });
  by("media-dialog").addEventListener("click", (event) => {
    if (event.target !== by("media-dialog")) return;
    const bounds = by("media-dialog").getBoundingClientRect();
    if (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom) closeDetails();
  });
  by("media-detail-refresh").addEventListener("click", async () => { await refreshHealth(); await refreshOpenDetail(); });
  function attachNavigation() {
    for (const host of navigationParent ? [window, navigationParent] : [window]) { host.addEventListener("popstate", readLocation); host.addEventListener("hashchange", readLocation); }
  }
  function detachNavigation() {
    for (const host of navigationParent ? [window, navigationParent] : [window]) { host.removeEventListener("popstate", readLocation); host.removeEventListener("hashchange", readLocation); }
  }
  // The parent outlives an ingress frame. Release its listeners on navigation,
  // and reattach only if this document is restored from the back-forward cache.
  window.addEventListener("pagehide", detachNavigation); window.addEventListener("pageshow", attachNavigation);
  attachNavigation();
  updateUnknownNotice(); readLocation(); expiryTimer = setInterval(renderHealth, 1000); initialize();
})();
