"""Entry-owned Repairs from current backend evidence, never device controls."""

import re

from homeassistant.components.repairs import RepairsFlow
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers import issue_registry as ir
from homeassistant.helpers.aiohttp_client import async_get_clientsession

from ._app_kit.repairs import VersionRepairSpec, create_version_fix_flow
from ._app_kit.version import LOADED_VERSION
from ._app_kit.versions import ReleaseVersion
from .client import DownloadsClient
from .const import API_PREFIX, APP_URL, CONF_ADDON_SLUG, CONF_BACKEND_URL, CONF_TOKEN, DOMAIN
from .models import Component, HealthIssueCode, HealthSnapshot, HealthState, IssueSeverity, System
from .native_health import HealthData, Reason

_INTEGRATION_URL = "/config/integrations/integration/downloads"
_PROTECTION_FAILURES = frozenset(
    {
        Reason.VPN_UNAVAILABLE,
        Reason.FIREWALL_UNVERIFIED,
        Reason.DNS_UNVERIFIED,
        Reason.DOWNLOADER_UNAVAILABLE,
        Reason.DOWNLOADER_MODULES_UNAVAILABLE,
    }
)
_UNKNOWN_ISSUES = frozenset({HealthIssueCode.CHECKS_MISSING, HealthIssueCode.CHECKS_STALE})
_KEYS = (
    "protection",
    "app",
    "configuration",
    "protection_connection",
    "media_connection",
    "version_mismatch",
)


@callback
def async_clear(hass: HomeAssistant, entry: ConfigEntry, key: str) -> None:
    ir.async_delete_issue(hass, DOMAIN, f"{entry.entry_id}_{key}")


@callback
def async_report(
    hass: HomeAssistant,
    entry: ConfigEntry,
    key: str,
    component: str,
    codes: tuple[str, ...],
    *,
    unknown: bool = False,
    severity: ir.IssueSeverity = ir.IssueSeverity.ERROR,
) -> None:
    """Only fixed labels/codes and native routes enter the issue registry."""
    slug = entry.data.get(CONF_ADDON_SLUG)
    logs_url = (
        f"/config/app/{slug}/logs"
        if isinstance(slug, str) and re.fullmatch(r"[a-z0-9_]+", slug)
        else _INTEGRATION_URL
    )
    ir.async_create_issue(
        hass,
        DOMAIN,
        f"{entry.entry_id}_{key}",
        config_entry_id=entry.entry_id,
        data={"codes": ", ".join(codes)},
        is_fixable=False,
        is_persistent=True,
        severity=ir.IssueSeverity.WARNING if unknown else severity,
        translation_key="health_unavailable" if unknown else "health_failed",
        translation_placeholders={
            "component": component,
            "codes": ", ".join(codes),
            "logs_url": logs_url,
            "integration_url": _INTEGRATION_URL,
        },
    )


@callback
def async_update_protection(hass: HomeAssistant, entry: ConfigEntry, data: HealthData) -> None:
    """A running app does not establish healthy protection or downloader checks."""
    async_clear(hass, entry, "app")
    if data.state == "needs_configuration":
        async_report(hass, entry, "configuration", "Downloads setup", ("configuration_required",))
        return
    if data.state in {"starting", "stopping"}:
        return
    async_clear(hass, entry, "configuration")
    projection = data.projection()
    if (
        projection["age_ms"] is None
        or projection["age_ms"] > projection["max_age_ms"]
        or Reason.CHECKS_MISSING in data.reasons
        or Reason.CHECKS_STALE in data.reasons
    ):
        async_report(
            hass,
            entry,
            "protection_connection",
            "Downloads protection",
            ("checks_stale",),
            unknown=True,
        )
        return
    async_clear(hass, entry, "protection_connection")
    if projection["ready"]:
        async_clear(hass, entry, "protection")
    else:
        codes = tuple(
            sorted(reason.value for reason in data.reasons if reason in _PROTECTION_FAILURES)
        )
        if codes:
            async_report(hass, entry, "protection", "Downloads protection", codes)


@callback
def async_update_components(
    hass: HomeAssistant, entry: ConfigEntry, data: HealthSnapshot, now_ms: int
) -> None:
    """Use producer-classified faults; absent/stale components cannot clear them."""
    states = {item.component: item.state_at(now_ms) for item in data.components}
    unknown = (
        data.generated_at > now_ms
        or states.keys() != set(Component)
        or any(state in {HealthState.UNKNOWN, HealthState.STALE} for state in states.values())
        or any(issue.code in _UNKNOWN_ISSUES for item in data.components for issue in item.issues)
    )
    if unknown:
        async_report(
            hass,
            entry,
            "media_connection",
            "Downloads media health",
            ("checks_stale",),
            unknown=True,
        )
    else:
        async_clear(hass, entry, "media_connection")
    if data.generated_at > now_ms:
        return
    for component in data.components:
        key = f"component_{component.component.value}"
        state = component.state_at(now_ms)
        if state in {HealthState.HEALTHY, HealthState.NOT_CONFIGURED}:
            async_clear(hass, entry, key)
        elif state is HealthState.UNAVAILABLE and component.unavailable_reason is not None:
            async_report(
                hass,
                entry,
                key,
                component.component.value.title(),
                (component.unavailable_reason.value,),
            )
        elif state is HealthState.DEGRADED:
            codes = tuple(
                sorted(
                    {
                        item.code.value
                        for item in component.issues
                        if item.code not in _UNKNOWN_ISSUES
                        and item.code is not HealthIssueCode.UPDATE_AVAILABLE
                    }
                )
            )
            if codes:
                severity = (
                    ir.IssueSeverity.ERROR
                    if any(item.severity is IssueSeverity.ERROR for item in component.issues)
                    else ir.IssueSeverity.WARNING
                )
                async_report(
                    hass, entry, key, component.component.value.title(), codes, severity=severity
                )
            elif component.issues and all(
                item.code is HealthIssueCode.UPDATE_AVAILABLE for item in component.issues
            ):
                # Available packages belong to HA Updates, not service-failure Repairs.
                async_clear(hass, entry, key)


@callback
def async_remove(hass: HomeAssistant, entry: ConfigEntry) -> None:
    """Unloading is not recovery; removing the configured owner ends its Repairs."""
    for key in (*_KEYS, *(f"component_{component.value}" for component in Component)):
        async_clear(hass, entry, key)


async def _async_probe_version(hass: HomeAssistant, entry: ConfigEntry) -> ReleaseVersion:
    if CONF_BACKEND_URL in entry.data and CONF_TOKEN in entry.data:
        client = DownloadsClient(
            async_get_clientsession(hass),
            entry.data[CONF_BACKEND_URL],
            entry.data[CONF_TOKEN],
        )
        response = await client.request("GET", f"{API_PREFIX}/system")
        if response.status != 200:
            raise ValueError("Cannot verify the Downloads version")
        system = System.parse(response.json())
        if system.role != "integration" or not system.read or system.admin:
            raise ValueError("Cannot verify the Downloads version")
        return ReleaseVersion.parse(system.version)

    from homeassistant.components.hassio import get_supervisor_client

    slug = _app_slug(entry)
    if slug is None:
        raise ValueError("Cannot verify the Downloads app")
    app = await get_supervisor_client(hass).addons.addon_info(slug)
    if app.url != APP_URL or app.state != "started":
        raise ValueError("Cannot verify the Downloads app")
    return ReleaseVersion.parse(app.version)


def _app_slug(entry: ConfigEntry) -> str | None:
    slug = entry.data.get(CONF_ADDON_SLUG)
    return slug if isinstance(slug, str) else None


VERSION_REPAIR = VersionRepairSpec(
    DOMAIN,
    "Downloads",
    "1443494457",
    ReleaseVersion.parse(LOADED_VERSION),
    _async_probe_version,
    _app_slug,
)


async def async_create_fix_flow(
    hass: HomeAssistant, issue_id: str, _data: dict[str, str | int | float | None] | None
) -> RepairsFlow:
    """Resolve the issue against its current loaded configuration entry."""
    return create_version_fix_flow(hass, issue_id, VERSION_REPAIR)
