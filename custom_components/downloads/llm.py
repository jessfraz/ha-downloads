"""Contribute an exposed, read-only Downloads health query to native Assist."""

import probatio as vol
from homeassistant.auth.permissions.const import POLICY_READ
from homeassistant.components import llm
from homeassistant.components.homeassistant.exposed_entities import async_should_expose
from homeassistant.config_entries import ConfigEntryState
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers import entity_registry as er
from homeassistant.helpers.llm import (
    LLMContext,
    Tool,
    ToolAnnotations,
    ToolInput,
    ToolResult,
)

from . import DownloadsConfigEntry
from .const import DOMAIN


def _entity_id(hass: HomeAssistant, entry: DownloadsConfigEntry) -> str | None:
    return er.async_get(hass).async_get_entity_id(
        "binary_sensor", DOMAIN, f"{entry.unique_id}_problem"
    )


@callback
def async_get_tools(
    hass: HomeAssistant, llm_context: LLMContext, api_id: str
) -> llm.LLMTools | None:
    """Only an explicitly exposed, loaded installation contributes a tool."""
    if api_id != "assist":
        return None
    for entry in hass.config_entries.async_loaded_entries(DOMAIN):
        entity_id = _entity_id(hass, entry)
        if entity_id and async_should_expose(hass, llm_context.assistant, entity_id):
            return llm.LLMTools(
                tools=[DownloadsHealth(entry)],
                prompt=(
                    "Use downloads__health for VPN protection and "
                    "downloader readiness. "
                    "Stale, unknown and unavailable data never establish readiness. "
                    "Idle or paused queues are not faults. This tool cannot change VPN "
                    "settings, start downloads, resume a queue, or restart anything."
                ),
            )
    return None


class DownloadsHealth(Tool):
    """Use only the current, credential-free coordinator snapshot."""

    name = "downloads__health"
    title = "Read Downloads health"
    description = "Read recent VPN, firewall, DNS and downloader readiness."
    integration = DOMAIN
    parameters = vol.Schema({}, extra=vol.PREVENT_EXTRA)
    annotations = ToolAnnotations(
        read_only=True, destructive=False, idempotent=True, open_world=False
    )

    def __init__(self, entry: DownloadsConfigEntry) -> None:
        self.entry = entry
        self.coordinator = entry.runtime_data

    async def async_call(
        self, hass: HomeAssistant, tool_input: ToolInput, llm_context: LLMContext
    ) -> ToolResult:
        """Recheck exposure, user permissions and entry ownership at each call."""
        try:
            self.parameters(tool_input.tool_args)
        except vol.Invalid:
            return ToolResult(data={"error": "invalid_arguments"}, error=True)
        entity_id = _entity_id(hass, self.entry)
        if not entity_id or not async_should_expose(
            hass, llm_context.assistant, entity_id
        ):
            return ToolResult(data={"error": "not_exposed"}, error=True)
        context = llm_context.context
        user = (
            await hass.auth.async_get_user(context.user_id)
            if context and context.user_id
            else None
        )
        if (
            user is None
            or not user.is_active
            or not user.permissions.check_entity(entity_id, POLICY_READ)
        ):
            return ToolResult(data={"error": "permission_denied"}, error=True)
        if (
            self.entry.state is not ConfigEntryState.LOADED
            or self.entry.runtime_data is not self.coordinator
            or not self.coordinator.last_update_success
            or hass.states.get(entity_id) is None
        ):
            return ToolResult(data={"error": "unavailable"}, error=True)
        if not async_should_expose(hass, llm_context.assistant, entity_id):
            return ToolResult(data={"error": "not_exposed"}, error=True)
        return ToolResult(data=self.coordinator.data.projection())
