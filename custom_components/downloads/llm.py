"""Contribute an exposed, read-only Downloads health query to native Assist."""

import probatio as vol
from homeassistant.components import llm
from homeassistant.components.homeassistant.exposed_entities import async_should_expose
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.llm import (
    LLMContext,
    Tool,
    ToolAnnotations,
    ToolInput,
    ToolResult,
)

from . import DownloadsConfigEntry
from .access import HealthAccessError, async_health_snapshot, health_entity_id
from .const import DOMAIN


@callback
def async_get_tools(
    hass: HomeAssistant, llm_context: LLMContext, api_id: str
) -> llm.LLMTools | None:
    """Only an explicitly exposed, loaded installation contributes a tool."""
    if api_id != "assist" or llm_context.assistant is None:
        return None
    for entry in hass.config_entries.async_loaded_entries(DOMAIN):
        entity_id = health_entity_id(hass, entry)
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
        if llm_context.assistant is None:
            return ToolResult(data={"error": "not_exposed"}, error=True)
        context = llm_context.context
        try:
            result = await async_health_snapshot(
                hass,
                self.entry,
                self.coordinator,
                context.user_id if context else None,
                llm_context.assistant,
            )
        except HealthAccessError as error:
            return ToolResult(data={"error": error.code.value}, error=True)
        return ToolResult(data=result)
