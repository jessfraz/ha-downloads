"""Entry-owned Repairs from current backend evidence, never device controls."""

import re

from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers import issue_registry as ir

from .const import CONF_ADDON_SLUG, DOMAIN
from .models import Component, HealthIssueCode, HealthSnapshot, HealthState, IssueSeverity
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
_KEYS = ("protection", "app", "configuration", "protection_connection", "media_connection")


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


@callback
def async_remove(hass: HomeAssistant, entry: ConfigEntry) -> None:
    """Unloading is not recovery; removing the configured owner ends its Repairs."""
    for key in (*_KEYS, *(f"component_{component.value}" for component in Component)):
        async_clear(hass, entry, key)
