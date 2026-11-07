"""Select an installed Downloads app without entering an address or token."""

import asyncio

import probatio as vol
from aiohasupervisor import SupervisorError
from aiohttp import ClientError
from homeassistant.config_entries import ConfigFlow, ConfigFlowResult
from homeassistant.helpers.hassio import is_hassio
from homeassistant.helpers.selector import (
    SelectOptionDict,
    SelectSelector,
    SelectSelectorConfig,
)
from homeassistant.helpers.update_coordinator import UpdateFailed

from .const import APP_URL, CONF_ADDON_SLUG, DOMAIN, REQUEST_TIMEOUT_SECONDS
from .coordinator import async_fetch_health


class DownloadsConfigFlow(ConfigFlow, domain=DOMAIN):
    """Pair only explicitly selected, installed copies of the owning app."""

    VERSION = 1

    async def async_step_user(
        self, user_input: dict[str, str] | None = None
    ) -> ConfigFlowResult:
        if not is_hassio(self.hass):
            return self.async_abort(reason="supervisor_required")
        if self._async_current_entries():
            return self.async_abort(reason="single_instance_allowed")
        from homeassistant.components.hassio import get_supervisor_client

        try:
            async with asyncio.timeout(REQUEST_TIMEOUT_SECONDS):
                apps = await get_supervisor_client(self.hass).addons.list()
        except SupervisorError, ClientError, TimeoutError:
            return self.async_abort(reason="supervisor_unavailable")
        choices = {app.slug: app.name for app in apps if app.url == APP_URL}
        if not choices:
            return self.async_abort(reason="app_not_found")
        errors: dict[str, str] = {}
        if user_input is not None:
            slug = user_input.get(CONF_ADDON_SLUG)
            if slug not in choices:
                errors["base"] = "app_not_found"
            else:
                await self.async_set_unique_id(slug)
                self._abort_if_unique_id_configured()
                try:
                    await async_fetch_health(self.hass, slug)
                except UpdateFailed:
                    errors["base"] = "cannot_connect"
                else:
                    return self.async_create_entry(
                        title="Downloads", data={CONF_ADDON_SLUG: slug}
                    )
        return self.async_show_form(
            step_id="user",
            data_schema=vol.Schema(
                {
                    vol.Required(CONF_ADDON_SLUG): SelectSelector(
                        SelectSelectorConfig(
                            options=[
                                SelectOptionDict(value=slug, label=name)
                                for slug, name in choices.items()
                            ]
                        )
                    )
                }
            ),
            errors=errors,
        )
