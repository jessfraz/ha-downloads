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
    const outcomes = { passed: "Passed", not_checked: "Not checked", stopped: "Stopped", timeout: "Request timed out", transport_error: "Request failed", http_error: "HTTP request rejected", invalid_response: "Invalid response", health_failed: "Health check failed", unsafe_resolver: "Resolver protection failed", dns_lookup_failed: "DNS lookup failed", firewall_failed: "Firewall check failed" };
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
    const message = status.message.slice(0, 1000);

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
    setCheck("downloader", downloaderLabels[status.downloader.state], status.downloader.state === "running"
      ? queuePaused ? "Resume the queue in SABnzbd when ready." : status.downloader.paused === null ? "Queue managed in SABnzbd." : "SABnzbd is running."
      : status.downloader.state === "starting" ? "Starting SABnzbd." : "Waiting for a verified connection.",
    ready ? queuePaused ? "warning" : status.downloader.paused === false ? "success" : "neutral" : "neutral");

    if (ready) {
      setBanner("Connected and protected", message || "The VPN, firewall, and DNS checks have passed. Downloads are ready.", "Protected", "success");
      showDownloads(queuePaused);
      return;
    }
    closeDownloads();
    if (status.health?.reasons.some((reason) => reason === "checks_stale" || reason === "checks_missing") && !setup && !stopping) {
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
    } else if (failed) {
      setBanner("Protection check failed", message || "The connection checks need attention before downloads can open.", "Needs attention", "failure");
      showHelp("Check the app logs", "Review the VPN settings and app logs in Home Assistant. The downloads interface stays closed until all connection checks pass.");
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
  followHomeAssistantTheme();
  refresh();
})();
