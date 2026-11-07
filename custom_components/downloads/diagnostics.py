"""Cached aggregate diagnostics, without credentials, titles, roots or actors."""

from typing import Literal, TypedDict

from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant

from .const import CONF_ADDON_SLUG, CONF_ALLOW_REQUESTS
from .coordinator import DownloadsCoordinator, now_ms


class DownloadsDiagnostics(TypedDict):
    connection: Literal["app", "standalone"]
    entry_state: str
    last_update_success: bool | None
    last_error_type: str | None
    requests_enabled: bool
    backend_version: str | None
    component_states: dict[str, str]
    operation_counts: dict[str, int]


async def async_get_config_entry_diagnostics(
    hass: HomeAssistant, entry: ConfigEntry[DownloadsCoordinator]
) -> DownloadsDiagnostics:
    """Diagnostics must not refresh or export arbitrary backend/provider values."""
    coordinator = getattr(entry, "runtime_data", None)
    components: dict[str, str] = {}
    counts = {"ready": 0, "blocked": 0, "stale": 0}
    success = None
    error_type = None
    version = None
    if isinstance(coordinator, DownloadsCoordinator):
        success = coordinator.last_update_success
        error_type = (
            type(coordinator.last_exception).__name__
            if coordinator.last_exception is not None
            else None
        )
        version = coordinator.system.version if coordinator.system is not None else None
        if coordinator.data is not None:
            now = now_ms()
            components = {
                item.component.value: item.state_at(now).value
                for item in coordinator.data.components
            }
            for item in coordinator.data.readiness:
                counts[
                    "stale" if item.expires_at <= now else "ready" if item.ready else "blocked"
                ] += 1
    return {
        "connection": "app" if entry.data.get(CONF_ADDON_SLUG) else "standalone",
        "entry_state": entry.state.value,
        "last_update_success": success,
        "last_error_type": error_type,
        "requests_enabled": bool(entry.options.get(CONF_ALLOW_REQUESTS, False)),
        "backend_version": version,
        "component_states": components,
        "operation_counts": counts,
    }
