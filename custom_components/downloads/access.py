"""One authorization boundary for cached health queries from Assist and HTTP."""

from enum import StrEnum

from homeassistant.auth.permissions.const import POLICY_READ
from homeassistant.components.homeassistant.exposed_entities import async_should_expose
from homeassistant.config_entries import ConfigEntry, ConfigEntryState
from homeassistant.core import HomeAssistant
from homeassistant.helpers import entity_registry as er
from homeassistant.util.json import JsonObjectType

from .const import DOMAIN
from .coordinator import DownloadsCoordinator


class HealthErrorCode(StrEnum):
    """Safe errors without backend identifiers or response bodies."""

    UNAVAILABLE = "unavailable"
    NOT_EXPOSED = "not_exposed"
    PERMISSION_DENIED = "permission_denied"


class HealthAccessError(Exception):
    """A fixed reason, suitable for both native API clients."""

    def __init__(self, code: HealthErrorCode) -> None:
        super().__init__(code.value)
        self.code = code


def health_entity_id(hass: HomeAssistant, entry: ConfigEntry) -> str | None:
    """Find the selected installation's canonical problem sensor."""
    return er.async_get(hass).async_get_entity_id(
        "binary_sensor", DOMAIN, f"{entry.unique_id}_problem"
    )


async def async_health_snapshot(
    hass: HomeAssistant,
    entry: ConfigEntry[DownloadsCoordinator],
    coordinator: DownloadsCoordinator,
    user_id: str | None,
    assistant: str | None,
) -> JsonObjectType:
    """Recheck permissions and captured entry ownership after the auth lookup."""
    if (
        entry.state is not ConfigEntryState.LOADED
        or entry.runtime_data is not coordinator
    ):
        raise HealthAccessError(HealthErrorCode.UNAVAILABLE)
    entity_id = health_entity_id(hass, entry)
    if entity_id is None or hass.states.get(entity_id) is None:
        raise HealthAccessError(HealthErrorCode.UNAVAILABLE)
    if assistant is not None and not async_should_expose(hass, assistant, entity_id):
        raise HealthAccessError(HealthErrorCode.NOT_EXPOSED)
    user = await hass.auth.async_get_user(user_id) if user_id else None
    if (
        user is None
        or not user.is_active
        or not user.permissions.check_entity(entity_id, POLICY_READ)
    ):
        raise HealthAccessError(HealthErrorCode.PERMISSION_DENIED)
    if (
        entry.state is not ConfigEntryState.LOADED
        or entry.runtime_data is not coordinator
        or not coordinator.last_update_success
        or hass.states.get(entity_id) is None
    ):
        raise HealthAccessError(HealthErrorCode.UNAVAILABLE)
    if assistant is not None and not async_should_expose(hass, assistant, entity_id):
        raise HealthAccessError(HealthErrorCode.NOT_EXPOSED)
    return coordinator.data.projection()
