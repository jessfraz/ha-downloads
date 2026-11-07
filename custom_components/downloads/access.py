"""Per-invocation identity, lifecycle, permission and Assist opt-in checks."""

from dataclasses import dataclass

from homeassistant.auth.permissions.const import POLICY_CONTROL, POLICY_READ
from homeassistant.components.homeassistant.exposed_entities import async_should_expose
from homeassistant.config_entries import ConfigEntry, ConfigEntryState
from homeassistant.core import Context, HomeAssistant
from homeassistant.exceptions import HomeAssistantError
from homeassistant.helpers import entity_registry as er

from .client import Actor
from .const import CONF_ALLOW_REQUESTS, DOMAIN
from .coordinator import DownloadsCoordinator


class AccessFailure(HomeAssistantError):
    """Safe, fixed failures contain no configuration or provider records."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


def loaded_entry(hass: HomeAssistant) -> ConfigEntry[DownloadsCoordinator] | None:
    for entry in hass.config_entries.async_entries(DOMAIN):
        if entry.state is ConfigEntryState.LOADED and isinstance(
            entry.runtime_data, DownloadsCoordinator
        ):
            return entry
    return None


def gateway_entity_id(hass: HomeAssistant, entry: ConfigEntry) -> str | None:
    return er.async_get(hass).async_get_entity_id(
        "sensor", DOMAIN, f"{entry.entry_id}_stack_health"
    )


@dataclass(frozen=True, slots=True)
class AccessGuard:
    """A cached tool remains bound to the exact entry runtime that supplied it."""

    hass: HomeAssistant
    entry: ConfigEntry[DownloadsCoordinator]
    coordinator: DownloadsCoordinator
    context: Context | None
    assistant: str | None = None
    request: bool = False

    def _check_current(self) -> str:
        if (
            self.entry.state is not ConfigEntryState.LOADED
            or self.entry.runtime_data is not self.coordinator
        ):
            raise AccessFailure("unavailable", "Downloads is unavailable.")
        entity_id = gateway_entity_id(self.hass, self.entry)
        if entity_id is None or self.hass.states.get(entity_id) is None:
            raise AccessFailure("unavailable", "Downloads is unavailable.")
        if self.assistant is not None and not async_should_expose(
            self.hass, self.assistant, entity_id
        ):
            raise AccessFailure(
                "not_exposed", "Expose the Downloads stack health entity to Assist."
            )
        system = self.coordinator.system
        if system is None or system.role != "integration" or not system.read or system.admin:
            raise AccessFailure(
                "permission_denied",
                "The integration credential cannot access Downloads.",
            )
        if self.request and not self.entry.options.get(CONF_ALLOW_REQUESTS, False):
            raise AccessFailure(
                "permission_denied",
                "Media requests are disabled in the Downloads integration options.",
            )
        return entity_id

    async def check(self) -> Actor:
        """Check again after each await; exposure never creates admin authority."""
        entity_id = self._check_current()
        user_id = self.context.user_id if self.context is not None else None
        if user_id is None:
            return Actor(
                "assist" if self.assistant is not None else "automation",
                bool(self.entry.options.get(CONF_ALLOW_REQUESTS, False)),
            )
        user = await self.hass.auth.async_get_user(user_id)
        entity_id = self._check_current()
        if (
            user is None
            or not user.is_active
            or not user.permissions.check_entity(
                entity_id, POLICY_CONTROL if self.request else POLICY_READ
            )
        ):
            raise AccessFailure("permission_denied", "The current user cannot access Downloads.")
        # Capabilities describe this actor, not just the verb of the current
        # call. A GET /system must report request authority truthfully too.
        can_request = bool(self.entry.options.get(CONF_ALLOW_REQUESTS, False)) and (
            user.permissions.check_entity(entity_id, POLICY_CONTROL)
        )
        return Actor(user.id, can_request)


def current_guard(
    hass: HomeAssistant,
    context: Context | None,
    *,
    assistant: str | None = None,
    request: bool = False,
) -> AccessGuard:
    entry = loaded_entry(hass)
    if entry is None:
        raise AccessFailure("unavailable", "Downloads is unavailable.")
    return AccessGuard(hass, entry, entry.runtime_data, context, assistant, request)
