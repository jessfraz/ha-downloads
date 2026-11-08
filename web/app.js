(() => {
  "use strict";
  // Only verified app capabilities may mount the administrator downloader UI.
  let canAdministerDownloader = false;
  let renderVerifiedDownloaderStatus = () => {};
(() => {
  "use strict";

  const POLL_INTERVAL = 5000;
  const REQUEST_TIMEOUT = 8000;
  const statusUrl = new URL("api/v1/status", document.baseURI);
  // Keep upstream navigation inside the authenticated ingress prefix.
  const downloaderUrl = new URL("sabnzbd/", document.baseURI).href;
  const byId = (id) => document.getElementById(id);
  const refreshButton = byId("refresh");
  let pollingTimer = null;
  let activeRequest = null;
  let active = true;
  let frame = null;
  let themeObserver = null;
  let latestStatus = null;
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

  function closeDownloads() {
    byId("downloads").hidden = true;
    byId("open-downloads").removeAttribute("href");
    if (frame) {
      frame.remove();
      frame = null;
    }
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

  function showDownloads(paused) {
    if (!canAdministerDownloader) { closeDownloads(); return; }
    byId("open-downloads").href = downloaderUrl;
    byId("paused-badge").hidden = paused !== true;
    if (!frame) {
      frame = document.createElement("iframe");
      frame.title = "SABnzbd download queue";
      frame.src = downloaderUrl;
      frame.referrerPolicy = "same-origin";
      byId("download-frame").append(frame);
    }
    byId("downloads").hidden = false;
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
      ? queuePaused ? "Resume the queue in SABnzbd when ready." : status.downloader.paused === null ? "Queue managed in SABnzbd." : "SABnzbd is running."
      : status.downloader.state === "starting" ? "Starting SABnzbd." : "Waiting for a verified connection.",
    ready ? queuePaused ? "warning" : status.downloader.paused === false ? "success" : "neutral" : "neutral");

    if (ready) {
      setBanner("Connected and protected", message || "The VPN, firewall, and DNS checks have passed. Downloads are ready.", "Protected", "success");
      showDownloads(queuePaused);
      return;
    }
    closeDownloads();
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
      setBanner("Protection check failed", message || "The connection checks need attention before downloads can open.", "Needs attention", "failure");
      showHelp("Check the app logs", "Review the VPN settings and app logs in Home Assistant. The downloads interface stays closed until all connection checks pass.");
    } else if (modulesUnavailable) {
      setBanner("Downloader needs attention", message, "Needs attention", "failure");
      showHelp("Update the app", "The downloader's required native modules are unavailable. Install the corrected app version in Home Assistant.");
    } else if (status.state === "degraded" || status.state === "ready") {
      setBanner(vpnConnected ? "Downloads unavailable" : status.vpn.state === "unknown" ? "VPN status unavailable" : status.vpn.state === "unhealthy" ? "VPN health check failed" : "VPN connection interrupted",
        message || "The connection is not ready. Downloads will open again once the checks pass.", "Needs attention", "warning");
      showHelp("Waiting for a secure connection", "The downloads interface stays closed until the VPN, firewall, and DNS checks pass. If this continues, check the app settings and logs in Home Assistant.");
    } else {
      setBanner("Connecting securely", message || "Waiting for the VPN, firewall, and DNS checks before starting downloads.", "Starting");
      showHelp("Getting things ready", "Downloads will open here automatically when the connection is ready. First startup can take a little longer.");
    }
  }

  function renderUnavailable() {
    if (lastGuidanceState !== "unavailable") {
      byId("stack-health-panel").hidden = false;
      byId("stack-health-toggle").setAttribute("aria-expanded", "true");
      document.querySelector(".protection-details").open = true;
    }
    lastGuidanceState = "unavailable";
    latestStatus = null;
    closeDownloads();
    byId("setup").hidden = true;
    byId("public-ip").hidden = true;
    byId("public-ip").textContent = "";
    byId("last-checked").textContent = "Reconnecting…";
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
      if (activeRequest === request && active && !document.hidden) { latestStatus = status; render(status); }
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
    // Recheck before showing a queue left open in a backgrounded ingress tab.
    // Retain its frame until the result is known so an unchanged connection
    // preserves the operator's place in SABnzbd.
    byId("downloads").hidden = true;
    byId("public-ip").hidden = true;
    for (const name of ["vpn", "firewall", "dns", "downloader"]) {
      setCheck(name, "Checking", "Refreshing current status.");
    }
    setBanner("Checking your connection", "Checking the VPN and protection before opening downloads.", "Checking");
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
  renderVerifiedDownloaderStatus = () => { if (latestStatus && active && !document.hidden) render(latestStatus); };
  followHomeAssistantTheme();
  refresh();
})();

// Media requests are separate from the VPN runtime and the administrator SAB UI.
(() => {
  "use strict";
  const by = (id) => document.getElementById(id);
  const labels = { radarr: "Radarr", sonarr: "Sonarr", prowlarr: "Prowlarr", sabnzbd: "SABnzbd", vpn: "VPN tunnel", firewall: "Firewall", dns: "DNS", storage: "Storage", plex: "Plex" };
  const errorMessages = {
    authentication_required: "Sign in to Home Assistant again to continue.",
    permission_denied: "This Home Assistant account cannot perform that action.",
    not_configured: "Connect the media manager and choose a saved profile in the app settings.",
    upstream_unavailable: "The media manager cannot be reached. Check its status and try again when it is available.",
    invalid_upstream_response: "The media manager returned an unrecognized response.",
    invalid_arguments: "Check the selected title, profile, and seasons.",
    invalid_scope: "The selected seasons cannot be requested. Refresh the title and choose again.",
    existing_settings_conflict: "This title uses different quality settings. Its existing settings were not changed. Choose a matching saved profile.",
    profile_revision_conflict: "The saved profile changed. Review the updated settings before requesting again.",
    revision_conflict: "This profile changed elsewhere. Reload it before saving.",
    idempotency_conflict: "This request key is already bound to a different selection. No new request was created.",
    not_found: "This title or profile is no longer available.",
    precondition_required: "Reload the profile before saving it.",
  };
  class ApiError extends Error {
    constructor(status, code) { super(errorMessages[code] || "Downloads could not complete this action."); this.status = status; this.code = code; }
  }
  const state = {
    view: "search", kind: "all", query: "", search: [], searchNext: null, searchAbort: null, searchVersion: 0,
    selected: null, selectedVersion: 0, selectedProfile: null, seasons: new Set(), mode: "search_now", detailBusy: false, detailVerified: false,
    profiles: [], options: new Map(), system: null, health: null, scopedHealth: null, healthFailed: false,
    library: new Map(), libraryPages: { movie: undefined, series: undefined }, queue: [], requests: [],
    queueNext: null, requestsNext: null, libraryLoaded: false, activityLoaded: false, acceptedSelections: new Map(),
    feedback: null, conflict: null, pending: null, active: true, controllers: new Set(),
    detailOrigin: { view: "search", focusId: "search-tab" }, profileSnapshot: null, detailSignature: null, activitySignature: null,
    libraryStale: new Map(), queueStale: new Map(),
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
  let profileSave = null;

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
  function validIdentity(media) { return media && ["movie", "series"].includes(media.kind) && Number.isInteger(idOf(media)) && idOf(media) > 0; }
  function validateMedia(media) {
    if (!validIdentity(media) || typeof media.title !== "string" || typeof media.overview !== "string" ||
        !["not_added", "managed"].includes(media.library?.state) || !validAcquisition(media.acquisition) ||
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
    const placeholder = node("span", "media-poster-placeholder", media.title);
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
    if (!query) { state.search = []; state.searchNext = null; notice("search-notice", ""); by("search-results-label").textContent = "Search movies and TV"; by("search-empty").textContent = "Find something worth staying in for."; renderCards(); return; }
    const controller = new AbortController(); state.searchAbort = controller;
    by("search-results-label").textContent = "Searching…"; by("search-empty").textContent = "Searching your media managers…";
    if (!append) { state.search = []; state.searchNext = null; renderCards(); }
    const params = new URLSearchParams({ query, kind: state.kind, limit: "20" });
    if (append && state.searchNext) params.set("page_token", state.searchNext);
    try {
      const result = validatePage(await api(`search?${params}`, { controller }), validateMedia);
      if (version !== state.searchVersion || !state.active) return;
      state.search = append ? [...state.search, ...result.items] : result.items;
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
  function profileForSelection() {
    return state.selectedProfile && state.profiles.find((p) => p.kind === state.selected?.kind && p.id === state.selectedProfile.id && p.revision === state.selectedProfile.revision);
  }
  function profileSummary(profile) {
    if (!profile) return "Choose a saved profile before requesting.";
    const options = state.options.get(profile.kind === "movie" ? "radarr" : "sonarr");
    const quality = options?.quality_profiles.find((p) => p.id === profile.settings.quality_profile_id)?.name || "Saved quality profile";
    const folder = options?.root_folders.find((p) => p.id === profile.settings.root_folder_id)?.name || "Saved library folder";
    return `${quality} · ${folder}${profile.kind === "series" ? profile.settings.future_monitoring === "monitor" ? " · Monitor future episodes" : " · No future episode monitoring" : ""}`;
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
  function profileConflictText(conflict) {
    const previous = conflict.previous, current = conflict.current;
    const nameChange = previous.name && previous.name !== current.name ? `“${previous.name}” is now “${current.name}”. ` : `“${current.name}” changed. `;
    const changes = [];
    if (previous.settings) {
      if (previous.settings.quality_profile_id !== current.settings.quality_profile_id) changes.push("Quality profile changed");
      if (previous.settings.root_folder_id !== current.settings.root_folder_id) changes.push("Library folder changed");
      if (JSON.stringify(previous.settings.tags) !== JSON.stringify(current.settings.tags)) changes.push("Tags changed");
      if (previous.settings.minimum_availability !== current.settings.minimum_availability) changes.push("Minimum availability changed");
      if (previous.settings.future_monitoring !== current.settings.future_monitoring) changes.push("Future episode monitoring changed");
      if (previous.settings.series_type !== current.settings.series_type) changes.push("Series type changed");
      if (previous.settings.season_folders !== current.settings.season_folders) changes.push("Season-folder preference changed");
    }
    return `${nameChange}${changes.length ? `${changes.join(". ")}. ` : ""}${profileSummary(current)}. Review the updated preferences before requesting.`;
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
    container.replaceChildren(node("p", "", pending.posting ? `Submitting “${pending.title || "the selected title"}”. Other new requests are paused until its receipt is known.` : `The submission for “${pending.title || "an earlier title"}” is unverified. Other new requests are paused to avoid duplicates. Check or retry the original operation in Activity.`));
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
    not_configured: "A required component is not configured", upstream_unavailable: "A required service cannot be reached",
  };
  function targetReadiness() {
    if (!state.scopedHealth || !state.selectedProfile || !state.selected) return null;
    const operation = state.selected.kind === "movie" ? "request_movie" : "request_series";
    return state.scopedHealth.readiness.find((r) => r.operation === operation && r.profile_id === state.selectedProfile.id) || null;
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
    const media = state.selected, profile = profileForSelection();
    const complete = media?.acquisition.state === "imported" && state.mode === "search_now";
    const activeMovie = media?.kind === "movie" && ["downloading", "importing", "waiting"].includes(media.acquisition.state);
    const noScope = media?.kind === "series" && state.mode === "search_now" && state.seasons.size === 0;
    const noFuture = media?.kind === "series" && state.mode === "monitor_only" && profile?.settings.future_monitoring !== "monitor";
    const accepted = [...state.acceptedSelections.values()].some((record) => {
      const input = record.input;
      if (!media || !profile || identity(input) !== identity(media) || input.profile.id !== profile.id || input.profile.revision !== profile.revision || input.intent.mode !== state.mode || record.submission.state === "failed") return false;
      if (media.kind === "movie" || state.mode === "monitor_only") return true;
      return input.intent.scope?.kind === "seasons" && input.intent.scope.numbers.length === state.seasons.size && input.intent.scope.numbers.every((number) => state.seasons.has(number));
    });
    const unknown = state.pending !== null;
    const readinessNote = by("request-readiness-note");
    if (readinessNote) {
      readinessNote.hidden = !state.system?.capabilities.request || !state.selectedProfile || requestReady() && !noFuture;
      const readiness = targetReadiness();
      readinessNote.textContent = noFuture ? "This profile does not monitor future episodes. Choose Search now or a profile with future monitoring enabled." : readiness && evidenceFresh(readiness) && readiness.state === "blocked" ? `Requests with this profile are blocked: ${readiness.reasons.map((reason) => reasonLabels[reason] || "A required component needs attention").join(". ")}.` : "Request readiness is not verified. Fresh checks are needed before requesting.";
    }
    element.disabled = !state.system?.capabilities.request || state.detailBusy || !state.detailVerified || !profile || !requestReady() || noScope || noFuture || complete || activeMovie || accepted || unknown || state.conflict !== null;
    updateUnknownNotice();
    element.textContent = unknown ? state.pending.posting ? "Submitting request…" : "Outcome unverified" : state.detailBusy ? "Loading title…" : complete ? "Already imported" : activeMovie ? "Already in progress" : accepted ? state.mode === "monitor_only" ? "Monitoring configured" : "Request accepted" : state.mode === "monitor_only" ? "Monitor with saved settings" : media?.kind === "series" ? "Get selected seasons" : "Request movie";
    updatePrimaryContrast(element);
  }
  function renderDetail() {
    const container = by("media-detail"); const media = state.selected;
    if (!media) { container.hidden = true; state.detailSignature = null; return; }
    const currentProfile = state.selectedProfile && state.profiles.find((profile) => profile.id === state.selectedProfile.id);
    if (currentProfile && currentProfile.revision !== state.selectedProfile.revision) {
      state.conflict = { previous: state.profileSnapshot || state.selectedProfile, current: currentProfile };
      state.feedback = null;
    }
    const signature = JSON.stringify([media, state.selectedProfile, state.profiles, state.mode, [...state.seasons], state.feedback, state.conflict, state.pending, state.system?.capabilities, state.detailBusy, state.detailVerified, state.detailOrigin, latestReceipt(media)]);
    if (state.detailSignature === signature) { updateRequestButton(); return; }
    state.detailSignature = signature;
    container.hidden = false;
    const previousFocus = container.contains(document.activeElement) ? document.activeElement.id : null;
    const backLabel = state.detailOrigin.view === "search" ? "results" : state.detailOrigin.view;
    const back = button(`← Back to ${backLabel}`, "media-link media-back", () => {
      by("search-panel").dataset.detail = "closed";
      changeView(state.detailOrigin.view);
      (by(state.detailOrigin.focusId) || by(`${state.detailOrigin.view}-tab`)).focus();
    }); back.id = "media-back";
    const children = [back];
    const title = node("h3", "", media.title); title.id = "media-detail-title"; title.tabIndex = -1;
    children.push(node("p", "media-overline", media.kind === "movie" ? "Movie" : "TV show"), title, node("p", "media-detail-meta", `${media.year || "Year unknown"} · ${detailStatus(media)}`));
    if (media.overview) children.push(node("p", "media-overview", media.overview));
    children.push(node("hr"));
    if (media.library.state === "managed") children.push(node("p", "media-preference-summary", `Existing settings preserved · ${media.library.quality_profile_name}`));
    const profileLabel = node("label", "media-field"); profileLabel.append(node("span", "", "Saved preferences"));
    const profileSelect = node("select"); profileSelect.id = "request-profile";
    const prompt = node("option", "", "Choose a saved profile"); prompt.value = ""; profileSelect.append(prompt);
    for (const profile of state.profiles.filter((p) => p.kind === media.kind)) {
      const option = node("option", "", profile.name); option.value = profile.id;
      option.selected = profile.id === state.selectedProfile?.id; profileSelect.append(option);
    }
    profileSelect.disabled = state.pending !== null;
    profileSelect.addEventListener("change", () => { const profile = state.profiles.find((p) => p.id === profileSelect.value); state.profileSnapshot = profile || null; state.selectedProfile = profile ? { id: profile.id, revision: profile.revision } : null; state.conflict = null; state.feedback = null; state.scopedHealth = null; renderDetail(); refreshScopedHealth(); });
    profileLabel.append(profileSelect); children.push(profileLabel, node("p", "media-preference-summary", profileSummary(profileForSelection())));
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
    else if (!state.profiles.some((p) => p.kind === media.kind)) children.push(node("p", "media-preference-summary", "An administrator needs to save a profile in System health → Saved download profiles."));
    const readinessNote = node("p", "media-preference-summary"); readinessNote.id = "request-readiness-note"; children.push(readinessNote);
    const requestButton = button("Request", "media-primary", submitRequest); requestButton.id = "request-media"; children.push(requestButton);
    children.push(node("p", "media-disclosure", "Radarr and Sonarr choose releases using your saved quality rules. Imported does not mean Plex playback has been verified."));
    if (state.feedback && (!state.feedback.identity || state.feedback.identity === identity(media))) { const feedback = node("div", "media-feedback", state.feedback.text); feedback.setAttribute("role", "status"); feedback.dataset.state = state.feedback.error ? "error" : "info";
      if (state.pending) { const check = button("Check request status", "media-link", recoverPending); check.id = "detail-check-request"; feedback.append(check); if (state.pending.allowRetry && identity(state.pending.input) === identity(media)) feedback.append(retryOriginalButton("detail-retry-request")); }
      children.push(feedback); }
    if (state.conflict) {
      const conflict = node("div", "media-feedback", profileConflictText(state.conflict)); conflict.dataset.state = "error"; conflict.setAttribute("role", "alert");
      const useUpdated = button("Use updated profile", "media-link", () => {
        const profile = state.conflict.current;
        state.profiles = state.profiles.filter((p) => p.id !== profile.id).concat(profile);
        state.selectedProfile = { id: profile.id, revision: profile.revision }; state.profileSnapshot = profile;
        state.conflict = null; state.feedback = { text: "Updated preferences selected. Review, then request again." };
        state.scopedHealth = null; renderDetail(); by("request-profile").focus(); refreshScopedHealth();
      }); useUpdated.id = "use-updated-profile"; conflict.append(useUpdated); children.push(conflict);
    }
    container.replaceChildren(...children); updateRequestButton();
    if (previousFocus) {
      const target = by(previousFocus);
      if (target && !target.disabled) target.focus();
      else (by("detail-check-request") || by("use-updated-profile") || by("media-detail-title")).focus();
    }
  }
  async function selectMedia(media, preserveOrigin = false) {
    if (!validIdentity(media)) return;
    if (!preserveOrigin) state.detailOrigin = { view: state.view, focusId: document.activeElement?.id || `${state.view}-tab` };
    changeView("search"); state.selected = media; state.seasons = new Set(); state.mode = "search_now"; state.conflict = null; state.feedback = null;
    state.selectedProfile = media.default_profile; state.profileSnapshot = state.profiles.find((profile) => profile.id === media.default_profile?.id && profile.revision === media.default_profile?.revision) || null; state.scopedHealth = null; state.detailBusy = true; state.detailVerified = false;
    const version = ++state.selectedVersion; by("search-panel").dataset.detail = "open"; updateCardSelection(); renderDetail();
    by(matchMedia("(max-width: 650px)").matches ? "media-back" : "media-detail-title").focus();
    try {
      const detail = validateMedia(await api(mediaRoute(media)));
      if (version !== state.selectedVersion || !state.active) return;
      state.selected = detail; state.selectedProfile = detail.default_profile;
      if (detail.default_profile && !state.profiles.some((p) => p.id === detail.default_profile.id && p.revision === detail.default_profile.revision)) {
        const current = await api(`profiles/${encodeURIComponent(detail.default_profile.id)}`);
        if (version !== state.selectedVersion || !state.active) return;
        if (current.revision !== detail.default_profile.revision) state.conflict = { previous: detail.default_profile, current };
        else state.profiles = state.profiles.filter((p) => p.id !== current.id).concat(current);
      }
      state.profileSnapshot = profileForSelection() || state.profileSnapshot;
      state.detailBusy = false; state.detailVerified = true; renderDetail(); await refreshScopedHealth();
    } catch (error) {
      if (version !== state.selectedVersion || !state.active) return;
      state.detailBusy = false; state.feedback = { text: error instanceof ApiError ? error.message : "Title details could not be verified. Refresh before requesting.", error: true }; renderDetail();
    }
  }
  async function refreshScopedHealth() {
    clearTimeout(scopedExpiryTimer);
    const version = state.selectedVersion; const reference = state.selectedProfile;
    if (!state.selected || !reference) { updateRequestButton(); return; }
    try {
      const result = await api(`health/stack?${new URLSearchParams({ operation: state.selected.kind === "movie" ? "request_movie" : "request_series", profile_id: reference.id })}`);
      if (version !== state.selectedVersion || reference !== state.selectedProfile || !state.active) return;
      state.scopedHealth = validateHealth(result); updateRequestButton();
      const readiness = targetReadiness();
      const deadline = readiness && evidenceDeadlines.get(readiness);
      if (Number.isFinite(deadline) && deadline > performance.now()) scopedExpiryTimer = setTimeout(updateRequestButton, Math.ceil(deadline - performance.now()));
    } catch { if (version === state.selectedVersion && reference === state.selectedProfile) { state.scopedHealth = null; updateRequestButton(); } }
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
      if (error instanceof ApiError && error.status === 404) { pending.uncertain = true; pending.allowRetry = true; storePending(); state.feedback = { text: "No receipt is available yet. The original submission may still be running. Check again, or explicitly retry the exact original request with its original key.", identity: identity(pending.input) }; }
      else { pending.uncertain = true; pending.allowRetry = true; storePending(); state.feedback = { text: "The request outcome is still unknown. Check status again when Downloads is reachable. No new request was submitted.", error: true, identity: identity(pending.input) }; }
      renderDetail(); renderActivity(); updateUnknownNotice(); announce(state.feedback.text);
    }
  }
  function retryOriginalButton(id) {
    const element = button("Retry original request", "media-link", retryOriginalRequest);
    element.id = id;
    element.disabled = !state.system?.capabilities.request || state.pending?.posting === true;
    return element;
  }
  async function retryOriginalRequest() {
    const pending = state.pending;
    if (!pending || !pending.allowRetry || pending.posting || !state.system?.capabilities.request) return;
    pending.uncertain = true; pending.allowRetry = false; pending.posting = true; storePending();
    state.feedback = { text: "Retrying only the original selection with its original request key.", identity: identity(pending.input) };
    renderDetail(); renderActivity();
    await postPending(pending);
  }
  async function submitRequest() {
    const media = state.selected, profile = profileForSelection(); if (!media || !profile || by("request-media")?.disabled || state.pending) return;
    if (!requestReady()) { updateRequestButton(); return; }
    const input = media.kind === "movie" ? { kind: "movie", tmdb_id: media.tmdb_id, profile: { id: profile.id, revision: profile.revision }, intent: { mode: state.mode } } : { kind: "series", tvdb_id: media.tvdb_id, profile: { id: profile.id, revision: profile.revision }, intent: state.mode === "monitor_only" ? { mode: "monitor_only" } : { mode: "search_now", scope: { kind: "seasons", numbers: [...state.seasons].sort((a, b) => a - b) } } };
    const pending = { key: requestKey(), input, title: media.title, profileSnapshot: profile, posting: true, allowRetry: false, uncertain: true }; state.pending = pending; storePending(); state.feedback = { text: "Submitting this exact selection…", identity: identity(input) }; renderDetail();
    await postPending(pending, true);
  }
  async function postPending(pending, originalAttempt = false) {
    const priorUncertainty = !originalAttempt && pending.uncertain === true;
    try { const record = await api("requests", { method: "POST", body: pending.input, headers: { "Idempotency-Key": pending.key } }); if (state.pending === pending) applyReceipt(record); }
    catch (error) {
      if (state.pending !== pending || !state.active) return;
      if (error instanceof ApiError && error.status < 500 && priorUncertainty) {
        pending.posting = false; pending.allowRetry = true; storePending();
        state.feedback = { text: "This retry was rejected, but the original submission still has an unverified outcome. Its original key and selection were kept. Check its receipt again; no new request was created.", error: true, identity: identity(pending.input) };
        renderDetail(); renderActivity();
      } else if (error instanceof ApiError && error.status < 500) {
        state.pending = null; storePending();
        if (error.code === "profile_revision_conflict") {
          state.feedback = null;
          try { const current = await api(`profiles/${encodeURIComponent(pending.input.profile.id)}`); state.conflict = { previous: pending.profileSnapshot || pending.input.profile, current }; }
          catch { state.feedback = { text: "The saved profile changed, but its new settings could not be read. Nothing was retried. Refresh profiles before continuing.", error: true }; }
        } else state.feedback = { text: error.message, error: true };
        renderDetail();
      } else { pending.uncertain = true; pending.posting = false; pending.allowRetry = true; storePending(); state.feedback = { text: "The connection ended before the request outcome was known. Checking the original request, without submitting it again.", identity: identity(pending.input) }; renderDetail(); await recoverPending(); }
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
    by("library-list").replaceChildren(...items.map((media) => mediaRow(media, `${media.year || "Year unknown"} · ${media.kind === "movie" ? "Movie" : "TV show"}${media.library.state === "managed" ? ` · ${media.library.quality_profile_name}` : ""}`)));
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
    const signature = JSON.stringify([state.queue, state.requests, state.pending, [...state.queueStale], [...state.library.values()], state.search]);
    if (signature === state.activitySignature) return;
    state.activitySignature = signature;
    const previousFocus = by("activity-list").contains(document.activeElement) ? document.activeElement.id : null;
    const queuedIdentities = new Set(state.queue.map((q) => identity(q.media)));
    const queueRows = state.queue.map((item) => {
      const media = state.library.get(identity(item.media)) || state.search.find((m) => identity(m) === identity(item.media));
      const row = node("article", "media-row");
      if (media) row.append(poster(media));
      const copy = node("div", "media-row-copy"); copy.append(node("h3", "", item.title));
      if (item.episode) copy.append(node("p", "", `Season ${item.episode.season}, episode ${item.episode.number}`));
      const progress = progressElement(item.acquisition, state.queueStale.has(item.media.kind === "movie" ? "radarr" : "sonarr")); if (progress) copy.append(progress);
      const action = button("View details", "media-link", () => selectMedia(media || { ...item.media, title: item.title, year: null, overview: "", poster: null, default_profile: null, library: { state: "not_added" }, acquisition: item.acquisition, ...(item.media.kind === "series" ? { seasons: [] } : {}) })); action.id = `activity-queue-${encodeURIComponent(item.id)}-details`; copy.append(action);
      row.append(copy, node("span", "media-row-state", staleAcquisitionLabel(item.acquisition, item.media.kind, state.queueStale))); return row;
    });
    const requestRows = [...state.requests].sort((a, b) => b.created_at_unix_ms - a.created_at_unix_ms).filter((record) => !queuedIdentities.has(identity(record.input))).map((record) => {
      const media = state.library.get(identity(record.input)) || state.search.find((m) => identity(m) === identity(record.input)) || (state.selected && identity(state.selected) === identity(record.input) ? state.selected : null);
      const row = node("article", "media-row"); const copy = node("div", "media-row-copy");
      copy.append(node("h3", "", media?.title || `${record.input.kind === "movie" ? "Movie" : "TV"} request`), node("p", "", recordFeedback(record)));
      const action = button("View details", "media-link", () => selectMedia(media || { ...record.input, title: `${record.input.kind === "movie" ? "Movie" : "TV"} request`, year: null, overview: "", poster: null, default_profile: record.input.profile, library: { state: "not_added" }, acquisition: { state: "unknown" }, ...(record.input.kind === "series" ? { seasons: [] } : {}) })); action.id = `activity-request-${record.id}-details`; copy.append(action);
      row.append(copy, node("span", "media-row-state", ({ pending: "Accepted", applying: "Applying request", applied: "Request applied", reconciling: "Checking outcome", failed: "Needs attention" })[record.submission.state])); return row;
    });
    const unresolved = [];
    if (state.pending && !state.pending.id) {
      const pending = state.pending, row = node("article", "media-row"), copy = node("div", "media-row-copy");
      copy.append(node("h3", "", pending.title || "Unverified media request"), node("p", "", "The original submission may still be running. No new request will be created automatically."));
      const check = button("Check request status", "media-link", recoverPending); check.id = "activity-check-request"; copy.append(check);
      if (pending.allowRetry) copy.append(retryOriginalButton("activity-retry-request"));
      row.append(copy, node("span", "media-row-state", "Outcome unverified")); unresolved.push(row);
    }
    by("activity-list").replaceChildren(...unresolved, ...queueRows, ...requestRows);
    by("activity-empty").hidden = unresolved.length + queueRows.length + requestRows.length > 0;
    by("activity-empty").textContent = state.activityLoaded ? "No activity was returned by the available sources." : "Loading activity…";
    by("activity-more").hidden = !state.queueNext && !state.requestsNext;
    const count = unresolved.length + state.queue.length + state.requests.filter((r) => ["pending", "applying", "reconciling"].includes(r.submission.state) && !queuedIdentities.has(identity(r.input))).length;
    by("activity-count").textContent = String(count); by("activity-count").hidden = count === 0;
    if (previousFocus) (by(previousFocus) || by("activity-tab")).focus();
  }
  async function loadActivity(append = false) {
    const paths = [];
    if (!append || state.queueNext) paths.push({ kind: "queue", path: `queue?${new URLSearchParams({ limit: "20", ...(append ? { page_token: state.queueNext } : {}) })}` });
    if (!append || state.requestsNext) paths.push({ kind: "requests", path: `requests?${new URLSearchParams({ limit: "20", ...(append ? { page_token: state.requestsNext } : {}) })}` });
    const results = await Promise.allSettled(paths.map(async ({ kind, path }) => ({ kind, result: validatePage(await api(path), kind === "requests" ? validateRequest : (q) => { if (!validIdentity(q.media) || typeof q.id !== "string" || typeof q.title !== "string" || !validAcquisition(q.acquisition)) throw new Error("Unrecognized queue item"); }) })));
    if (!state.active) return; const messages = [];
    for (let i = 0; i < results.length; i++) {
      const item = results[i]; if (item.status === "rejected") { if (paths[i].kind === "queue") { state.queueStale.set("radarr", "unavailable"); state.queueStale.set("sonarr", "unavailable"); } messages.push(`${paths[i].kind === "queue" ? "Download queue" : "Request history"} unavailable; previous results may be stale`); continue; }
      const { kind, result } = item.value; const incomplete = coverageText(result.coverage); if (incomplete) messages.push(incomplete);
      if (kind === "queue") {
        for (const source of ["radarr", "sonarr"]) { const sourceState = result.coverage.find((c) => c.source === source)?.state || "unavailable"; sourceState === "available" ? state.queueStale.delete(source) : state.queueStale.set(source, sourceState); }
        const keep = append ? state.queue : state.queue.filter((q) => result.coverage.some((c) => c.source === (q.media.kind === "movie" ? "radarr" : "sonarr") && c.state !== "available"));
        state.queue = [...new Map([...keep, ...result.items].map((q) => [q.id, q])).values()]; state.queueNext = result.next_page;
      } else { state.requests = [...new Map([...(append ? state.requests : []), ...result.items].map((r) => [r.id, r])).values()]; for (const record of result.items) if (state.acceptedSelections.has(record.id)) state.acceptedSelections.set(record.id, record); state.requestsNext = result.next_page; }
    }
    state.activityLoaded = true; notice("activity-notice", messages.length ? `${messages.join(". ")}. Results may be incomplete.` : ""); renderActivity(); renderDetail();
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
    for (const readiness of value.readiness) setEvidenceDeadline(readiness, `operation:${readiness.operation}:${readiness.profile_id || "none"}`, value.generated_at_unix_ms, readiness.expires_at_unix_ms, value, timing);
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
      if (!stale && outcome === "degraded" && Array.isArray(observation.outcome.issues)) element.append(node("span", "", observation.outcome.issues.map((issue) => ({ indexer_unavailable: "Indexer unavailable", download_client_unavailable: "Downloader unavailable", storage_unavailable: "Storage unavailable", storage_low: "Storage low", protection_unverified: "Protection unverified", checks_stale: "Checks stale", upstream_health: "Upstream needs attention" })[issue.code] || "Check component settings").join(" · ")));
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
    by("stack-health-note").textContent = stale ? "Current readiness cannot be confirmed. Browsing remains available; requests require fresh target-specific checks." : blocked ? "Some operations need attention. Requests use the checks for their selected profile; optional services do not block unrelated work." : "Requesting, downloading, and importing are available. Playback is checked separately.";
    updateRequestButton();
  }
  async function refreshHealth() {
    try { const result = validateHealth(await api("health/stack")); if (!state.active) return; state.health = result; state.healthFailed = false; }
    catch { if (!state.active) return; state.healthFailed = true; state.scopedHealth = null; }
    renderHealth(); await refreshScopedHealth();
  }
  function changeView(view) {
    state.view = view;
    for (const name of ["search", "library", "activity"]) { by(`${name}-tab`).setAttribute("aria-selected", String(name === view)); by(`${name}-tab`).tabIndex = name === view ? 0 : -1; by(`${name}-panel`).hidden = name !== view; }
    if (view === "library" && !state.libraryLoaded) loadLibrary();
    if (view === "activity" && !state.activityLoaded) loadActivity();
  }
  async function loadProfiles() {
    const result = await api("profiles");
    if (!result || !Array.isArray(result.items) || result.items.some((p) => !["movie", "series"].includes(p.kind) || typeof p.id !== "string" || !Number.isInteger(p.revision) || p.revision < 1 || typeof p.name !== "string" || !p.settings)) throw new Error("Unrecognized profile response");
    state.profileSnapshot = profileForSelection() || state.profileSnapshot;
    state.profiles = result.items;
  }
  function field(label, element) { const wrapper = node("label", "media-field"); wrapper.append(node("span", "", label), element); return wrapper; }
  function selectControl(id, options, current) {
    const select = node("select"); select.id = id;
    for (const [value, label] of options) { const option = node("option", "", label); option.value = String(value); option.selected = String(current) === String(value); select.append(option); }
    return select;
  }
  function renderProfileSettings() {
    const details = by("profile-settings"); if (!details) return;
    details.hidden = !state.system?.capabilities.admin;
    const form = by("profile-form"), kind = by("profile-kind").value;
    const selectedId = by("profile-edit").value;
    const profile = state.profiles.find((p) => p.id === selectedId && p.kind === kind);
    const edit = by("profile-edit"); edit.replaceChildren();
    for (const [value, label] of [["", "Create a profile"], ...state.profiles.filter((p) => p.kind === kind).map((p) => [p.id, p.name])]) { const option = node("option", "", label); option.value = value; option.selected = value === (profile?.id || ""); edit.append(option); }
    const options = state.options.get(kind === "movie" ? "radarr" : "sonarr");
    form.replaceChildren();
    if (!options) { form.append(node("p", "media-muted", "Connect this manager in the Home Assistant app configuration, then refresh to load its profile choices.")); return; }
    const name = node("input"); name.id = "profile-name"; name.required = true; name.maxLength = 80; name.value = profile?.name || "";
    const quality = selectControl("profile-quality", [["", "Choose quality"], ...options.quality_profiles.map((p) => [p.id, p.name])], profile?.settings.quality_profile_id);
    const root = selectControl("profile-root", [["", "Choose library"], ...options.root_folders.filter((p) => p.accessible).map((p) => [p.id, p.name])], profile?.settings.root_folder_id);
    quality.required = true; root.required = true;
    form.append(field("Profile name", name), field("Quality profile", quality), field("Library folder", root));
    const tags = node("fieldset", "media-seasons"); tags.append(node("legend", "", "Tags"));
    for (const tag of options.tags) { const label = node("label", "media-profile-check"); const input = node("input"); input.type = "checkbox"; input.name = "profile-tag"; input.value = String(tag.id); input.checked = profile?.settings.tags.includes(tag.id) || false; label.append(input, node("span", "", tag.name)); tags.append(label); }
    if (options.tags.length) form.append(tags);
    if (kind === "movie") form.append(field("Minimum availability", selectControl("profile-availability", [["announced", "Announced"], ["in_cinemas", "In cinemas"], ["released", "Released"]], profile?.settings.minimum_availability || "released")));
    else {
      form.append(field("Series type", selectControl("profile-series-type", options.series_types.map((value) => [value, ({ standard: "Standard", daily: "Daily", anime: "Anime" })[value]]), profile?.settings.series_type || "standard")));
      form.append(field("Future episodes", selectControl("profile-future", [["off", "Do not monitor future episodes"], ["monitor", "Monitor future episodes"]], profile?.settings.future_monitoring || "off")));
      const label = node("label", "media-profile-check"), checkbox = node("input"); checkbox.id = "profile-season-folders"; checkbox.type = "checkbox"; checkbox.checked = profile?.settings.season_folders ?? true; label.append(checkbox, node("span", "", "Use season folders")); form.append(label);
    }
    const defaultLabel = node("label", "media-profile-check"), defaultInput = node("input"); defaultInput.id = "profile-default"; defaultInput.type = "checkbox"; defaultInput.checked = profile?.is_default || false; defaultLabel.append(defaultInput, node("span", "", "Use as the default for this media type")); form.append(defaultLabel);
    const submit = node("button", "media-secondary", profile ? "Save profile changes" : "Create profile"); submit.type = "submit"; form.append(submit);
    form.dataset.profileId = profile?.id || ""; form.dataset.revision = String(profile?.revision || "");
  }
  async function saveProfile(event) {
    event.preventDefault(); const form = by("profile-form"); if (!form.reportValidity()) return;
    const kind = by("profile-kind").value;
    const settings = { quality_profile_id: Number(by("profile-quality").value), root_folder_id: Number(by("profile-root").value), tags: [...form.querySelectorAll("input[name='profile-tag']:checked")].map((e) => Number(e.value)) };
    if (kind === "movie") settings.minimum_availability = by("profile-availability").value;
    else { settings.series_type = by("profile-series-type").value; settings.season_folders = by("profile-season-folders").checked; settings.future_monitoring = by("profile-future").value; }
    const input = { kind, name: by("profile-name").value.trim(), is_default: by("profile-default").checked, settings };
    const fingerprint = JSON.stringify(input); if (!profileSave || profileSave.fingerprint !== fingerprint) profileSave = { fingerprint, key: requestKey() };
    const submit = form.querySelector("button[type='submit']"); submit.disabled = true;
    try {
      await api(form.dataset.profileId ? `profiles/${encodeURIComponent(form.dataset.profileId)}` : "profiles", { method: form.dataset.profileId ? "PUT" : "POST", body: input, headers: form.dataset.profileId ? { "If-Match": `"${form.dataset.revision}"` } : { "Idempotency-Key": profileSave.key } });
      await loadProfiles(); profileSave = null; notice("profile-notice", "Profile saved. Requests already accepted keep their original settings."); renderProfileSettings(); renderDetail();
    } catch (error) { notice("profile-notice", error instanceof ApiError ? error.message : "The save outcome is unknown. Reload profiles before making another change; retrying this unchanged form uses the same save key."); }
    finally { submit.disabled = false; }
  }
  async function refreshMedia() {
    if (!state.active || document.hidden || refreshing) return;
    refreshing = true;
    try {
      await refreshHealth();
      if (state.view === "activity") await loadActivity();
      if (state.pending) await recoverPending();
    } finally { refreshing = false; if (state.active && !document.hidden) refreshTimer = setTimeout(refreshMedia, 5000); }
  }
  async function initialize() {
    try {
      const system = await api("system");
      if (!system || typeof system.capabilities?.read !== "boolean" || typeof system.capabilities?.request !== "boolean" || typeof system.capabilities?.admin !== "boolean") throw new Error("Unrecognized system response");
      state.system = system;
      canAdministerDownloader = system.capabilities.admin;
      renderVerifiedDownloaderStatus();
      by("show-advanced").hidden = !system.capabilities.admin;
      by("advanced-downloads").hidden = !system.capabilities.admin;
      const reads = await Promise.allSettled([loadProfiles(), ...["radarr", "sonarr"].map(async (manager) => { const options = await api(`managers/${manager}/options`); if (!Array.isArray(options.quality_profiles) || !Array.isArray(options.root_folders) || !Array.isArray(options.tags) || !Array.isArray(options.series_types)) throw new Error("Unrecognized manager options"); state.options.set(manager, options); })]);
      if (reads[0].status === "rejected") notice("search-notice", "Saved profiles could not be read. Search remains available, but requesting is disabled until profiles can be verified.");
      renderProfileSettings(); renderDetail();
    } catch { notice("search-notice", "Downloads media settings are unavailable. The existing connection and downloader details remain available in System health."); }
    if (state.active) refreshMedia();
  }
  const unknownNotice = node("div", "media-notice"); unknownNotice.id = "unknown-request-notice"; unknownNotice.hidden = true; unknownNotice.setAttribute("role", "status");
  by("search-panel").parentElement.prepend(unknownNotice);
  const searchMore = button("Load more results", "media-secondary", () => search(true)); searchMore.id = "search-more"; searchMore.hidden = true; by("search-results-container").append(searchMore);
  const profileDetails = node("details", "profile-settings"); profileDetails.id = "profile-settings"; profileDetails.hidden = true; profileDetails.append(node("summary", "", "Saved download profiles"));
  const kindSelect = selectControl("profile-kind", [["movie", "Movies"], ["series", "TV shows"]], "movie");
  const editSelect = selectControl("profile-edit", [["", "Create a profile"]], "");
  const profileForm = node("form", "media-profile-form"); profileForm.id = "profile-form";
  const profileNotice = node("p", "media-notice"); profileNotice.id = "profile-notice"; profileNotice.hidden = true; profileNotice.setAttribute("role", "status");
  const reloadProfiles = button("Reload saved profiles", "media-link", async () => {
    try { await loadProfiles(); renderProfileSettings(); renderDetail(); notice("profile-notice", "Saved profiles reloaded. Unsaved edits were discarded."); }
    catch { notice("profile-notice", "Saved profiles could not be refreshed. Existing edits were preserved."); }
  });
  profileDetails.append(field("Media type", kindSelect), field("Saved profile", editSelect), reloadProfiles, profileNotice, profileForm); by("stack-health-panel").append(profileDetails);
  kindSelect.addEventListener("change", () => { editSelect.value = ""; renderProfileSettings(); }); editSelect.addEventListener("change", renderProfileSettings); profileForm.addEventListener("submit", saveProfile);
  by("media-search-form").addEventListener("submit", (event) => { event.preventDefault(); search(); });
  by("media-query").addEventListener("input", () => { clearTimeout(searchTimer); state.searchAbort?.abort(); ++state.searchVersion; searchTimer = setTimeout(() => search(), 300); });
  for (const filter of document.querySelectorAll(".media-filter")) filter.addEventListener("click", () => { state.kind = filter.dataset.kind; for (const peer of document.querySelectorAll(".media-filter")) peer.setAttribute("aria-pressed", String(peer === filter)); search(); });
  const tabs = [...document.querySelectorAll(".media-tab")];
  for (const tab of tabs) {
    tab.addEventListener("click", () => changeView(tab.dataset.view));
    tab.addEventListener("keydown", (event) => { const index = tabs.indexOf(tab); let next;
      if (event.key === "ArrowRight") next = (index + 1) % tabs.length; else if (event.key === "ArrowLeft") next = (index + tabs.length - 1) % tabs.length; else if (event.key === "Home") next = 0; else if (event.key === "End") next = tabs.length - 1;
      if (next !== undefined) { event.preventDefault(); tabs[next].focus(); changeView(tabs[next].dataset.view); }
    });
  }
  by("stack-health-toggle").addEventListener("click", () => { const panel = by("stack-health-panel"); panel.hidden = !panel.hidden; by("stack-health-toggle").setAttribute("aria-expanded", String(!panel.hidden)); });
  by("show-advanced").addEventListener("click", () => { by("stack-health-panel").hidden = false; by("stack-health-toggle").setAttribute("aria-expanded", "true"); by("advanced-downloads").open = true; by("advanced-downloads").scrollIntoView({ block: "start", behavior: "auto" }); });
  by("library-more").addEventListener("click", () => loadLibrary(true)); by("activity-more").addEventListener("click", () => loadActivity(true));
  by("refresh").addEventListener("click", async () => { clearTimeout(refreshTimer); await refreshMedia(); if (state.view === "library") await loadLibrary(); if (state.query) await search(); });
  function stop() { state.active = false; clearTimeout(refreshTimer); clearTimeout(searchTimer); clearInterval(expiryTimer); clearTimeout(scopedExpiryTimer); state.controllers.forEach((controller) => controller.abort()); }
  function start() { if (document.hidden) return; state.active = true; clearInterval(expiryTimer); expiryTimer = setInterval(renderHealth, 1000); if (state.selected && !state.detailVerified) selectMedia(state.selected, true); refreshMedia(); }
  document.addEventListener("visibilitychange", () => { document.hidden ? stop() : start(); });
  window.addEventListener("pagehide", stop); window.addEventListener("pageshow", () => { if (!state.active) start(); });
  try { const saved = JSON.parse(sessionStorage.getItem(pendingStorage) || "null"); if (saved && typeof saved.key === "string" && /^[a-f0-9-]{36}$/.test(saved.key) && validIdentity(saved.input)) { saved.posting = false; saved.uncertain = true; state.pending = saved; notice("activity-notice", "An earlier request has an unverified outcome. Its original receipt will be checked without submitting it again."); } } catch { /* Ignore malformed local state. */ }
  updateUnknownNotice(); changeView("search"); expiryTimer = setInterval(renderHealth, 1000); initialize();
})();

})();
