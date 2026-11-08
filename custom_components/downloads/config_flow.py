"""Explicit pairing and confirmed Supervisor discovery for Downloads."""

import asyncio
from collections.abc import Mapping
from uuid import UUID

import probatio as vol
from aiohasupervisor import SupervisorError
from aiohttp import ClientError
from homeassistant.config_entries import (
    SOURCE_REAUTH,
    SOURCE_RECONFIGURE,
    ConfigEntry,
    ConfigFlow,
    ConfigFlowResult,
    OptionsFlow,
)
from homeassistant.core import callback
from homeassistant.helpers.aiohttp_client import async_get_clientsession
from homeassistant.helpers.hassio import is_hassio
from homeassistant.helpers.selector import (
    SelectOptionDict,
    SelectSelector,
    SelectSelectorConfig,
    TextSelector,
    TextSelectorConfig,
    TextSelectorType,
)
from homeassistant.helpers.service_info.hassio import HassioServiceInfo
from homeassistant.helpers.update_coordinator import UpdateFailed

from .client import (
    DownloadsClient,
    InvalidBackendURL,
    ResponseTooLarge,
    normalize_backend_url,
)
from .const import (
    API_PREFIX,
    APP_URL,
    CONF_ADDON_SLUG,
    CONF_ALLOW_REQUESTS,
    CONF_BACKEND_URL,
    CONF_TOKEN,
    DOMAIN,
    REQUEST_TIMEOUT_SECONDS,
)
from .models import System
from .native_health import async_fetch_health


class DownloadsConfigFlow(ConfigFlow, domain=DOMAIN):
    VERSION = 2

    def __init__(self) -> None:
        self._discovered: dict[str, str] | None = None

    @staticmethod
    @callback
    def async_get_options_flow(config_entry: ConfigEntry) -> OptionsFlow:
        return DownloadsOptionsFlow()

    async def async_step_user(
        self, user_input: dict[str, object] | None = None
    ) -> ConfigFlowResult:
        if self._async_current_entries():
            return self.async_abort(reason="single_instance_allowed")
        return self.async_show_menu(step_id="user", menu_options=["health", "app", "standalone"])

    async def async_step_health(self, user_input: dict[str, str] | None = None) -> ConfigFlowResult:
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
                    return self.async_create_entry(title="Downloads", data={CONF_ADDON_SLUG: slug})
        return self.async_show_form(
            step_id="health",
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

    def _updating_entry(self) -> ConfigEntry | None:
        if self.source == SOURCE_REAUTH:
            return self._get_reauth_entry()
        if self.source == SOURCE_RECONFIGURE:
            return self._get_reconfigure_entry()
        return None

    def _finish_update(self, entry: ConfigEntry, values: dict[str, str]) -> ConfigFlowResult:
        """A loaded entry's update listener owns reload; never schedule it twice."""
        if entry.update_listeners:
            self.hass.config_entries.async_update_entry(entry, data={**entry.data, **values})
            return self.async_abort(
                reason="reauth_successful"
                if self.source == SOURCE_REAUTH
                else "reconfigure_successful"
            )
        return self.async_update_reload_and_abort(entry, data_updates=values)

    async def async_step_reconfigure(
        self, user_input: dict[str, object] | None = None
    ) -> ConfigFlowResult:
        """Pair media deliberately without replacing the installed health entry."""
        if self._get_reconfigure_entry().data.get(CONF_ADDON_SLUG):
            return await self.async_step_app(user_input)
        return await self._async_configure(user_input)

    async def async_step_app(self, user_input: dict[str, object] | None = None) -> ConfigFlowResult:
        error = "app_not_found"
        if not is_hassio(self.hass):
            error = "supervisor_required"
        else:
            from homeassistant.components.hassio import get_supervisor_client

            try:
                async with asyncio.timeout(REQUEST_TIMEOUT_SECONDS):
                    discoveries = await get_supervisor_client(self.hass).discovery.list()
                target = self._updating_entry()
                expected_slug = target.data.get(CONF_ADDON_SLUG) if target else None
                matches = [
                    item
                    for item in discoveries
                    if item.service == DOMAIN
                    and (expected_slug is None or item.addon == expected_slug)
                ]
                if len(matches) == 1:
                    item = matches[0]
                    return await self.async_step_hassio(
                        HassioServiceInfo(
                            config=item.config,
                            name="Downloads",
                            slug=item.addon,
                            uuid=item.uuid.hex,
                        )
                    )
                if len(matches) > 1:
                    error = "multiple_apps"
            except SupervisorError, ClientError, TimeoutError:
                error = "supervisor_unavailable"
        return self.async_show_form(
            step_id="app", data_schema=vol.Schema({}), errors={"base": error}
        )

    async def async_step_hassio(self, discovery_info: HassioServiceInfo) -> ConfigFlowResult:
        """Do not send discovered credentials until the user confirms pairing."""
        if not isinstance(discovery_info, HassioServiceInfo) or not isinstance(
            discovery_info.config, dict
        ):
            return self.async_abort(reason="invalid_discovery")
        config = discovery_info.config
        token = config.get(CONF_TOKEN)
        url = config.get(CONF_BACKEND_URL)
        if (
            not isinstance(token, str)
            or not token
            or any(char.isspace() for char in token)
            or not isinstance(url, str)
            or not discovery_info.slug
        ):
            return self.async_abort(reason="invalid_discovery")
        try:
            backend_url = normalize_backend_url(url)
            UUID(discovery_info.uuid)
            target = self._updating_entry()
            if target is not None and target.data.get(CONF_ADDON_SLUG) != discovery_info.slug:
                return self.async_abort(reason="unique_id_mismatch")
            unique_id = target.unique_id if target is not None else discovery_info.slug
        except ValueError, TypeError, AttributeError:
            return self.async_abort(reason="invalid_discovery")
        await self.async_set_unique_id(unique_id)
        if self._updating_entry() is not None:
            self._abort_if_unique_id_mismatch()
        else:
            self._abort_if_unique_id_configured()
            if self._async_current_entries():
                return self.async_abort(reason="single_instance_allowed")
        self._discovered = {
            CONF_BACKEND_URL: backend_url,
            CONF_TOKEN: token,
            CONF_ADDON_SLUG: discovery_info.slug,
        }
        return await self.async_step_hassio_confirm()

    async def async_step_hassio_confirm(
        self, user_input: dict[str, object] | None = None
    ) -> ConfigFlowResult:
        if self._discovered is None:
            return self.async_abort(reason="invalid_discovery")
        errors: dict[str, str] = {}
        if user_input is not None:
            if error := await self._async_validate(self._discovered):
                errors["base"] = error
            elif (target := self._updating_entry()) is not None:
                return self._finish_update(target, self._discovered)
            else:
                return self.async_create_entry(title="Downloads", data=self._discovered)
        return self.async_show_form(
            step_id="hassio_confirm", data_schema=vol.Schema({}), errors=errors
        )

    async def async_step_standalone(
        self, user_input: dict[str, str] | None = None
    ) -> ConfigFlowResult:
        return await self._async_configure(user_input)

    async def async_step_reauth(self, entry_data: Mapping[str, object]) -> ConfigFlowResult:
        if self._get_reauth_entry().data.get(CONF_ADDON_SLUG):
            return await self.async_step_app()
        return await self.async_step_reauth_confirm()

    async def async_step_reauth_confirm(
        self, user_input: dict[str, str] | None = None
    ) -> ConfigFlowResult:
        return await self._async_configure(user_input, reauth=True)

    async def _async_validate(self, values: dict[str, str]) -> str | None:
        try:
            client = DownloadsClient(
                async_get_clientsession(self.hass),
                values[CONF_BACKEND_URL],
                values[CONF_TOKEN],
            )
            response = await client.request("GET", f"{API_PREFIX}/system")
            if response.status in {401, 403}:
                return "invalid_auth"
            if response.status != 200:
                return "cannot_connect"
            system = System.parse(response.json())
            if system.role != "integration" or not system.read or system.admin:
                return "integration_required"
        except InvalidBackendURL:
            return "invalid_url"
        except ClientError, TimeoutError, ResponseTooLarge, ValueError:
            return "cannot_connect"
        return None

    async def _async_configure(
        self, user_input: dict[str, str] | None, *, reauth: bool = False
    ) -> ConfigFlowResult:
        errors: dict[str, str] = {}
        if user_input is not None:
            if error := await self._async_validate(user_input):
                errors[CONF_BACKEND_URL if error == "invalid_url" else "base"] = error
            else:
                values = {
                    CONF_BACKEND_URL: normalize_backend_url(user_input[CONF_BACKEND_URL]),
                    CONF_TOKEN: user_input[CONF_TOKEN],
                }
                if (target := self._updating_entry()) is not None:
                    return self._finish_update(target, values)
                if self._async_current_entries():
                    return self.async_abort(reason="single_instance_allowed")
                return self.async_create_entry(title="Downloads", data=values)
        target = self._updating_entry()
        default_url = target.data.get(CONF_BACKEND_URL, "") if target else ""
        return self.async_show_form(
            step_id="reauth_confirm" if reauth else "standalone",
            data_schema=vol.Schema(
                {
                    vol.Required(CONF_BACKEND_URL, default=default_url): TextSelector(),
                    vol.Required(CONF_TOKEN): TextSelector(
                        TextSelectorConfig(type=TextSelectorType.PASSWORD)
                    ),
                }
            ),
            errors=errors,
        )


class DownloadsOptionsFlow(OptionsFlow):
    async def async_step_init(self, user_input: dict[str, bool] | None = None) -> ConfigFlowResult:
        if not self.config_entry.data.get(CONF_TOKEN):
            return self.async_abort(reason="media_not_paired")
        if user_input is not None:
            return self.async_create_entry(title="", data=user_input)
        return self.async_show_form(
            step_id="init",
            data_schema=vol.Schema(
                {
                    vol.Required(
                        CONF_ALLOW_REQUESTS,
                        default=self.config_entry.options.get(CONF_ALLOW_REQUESTS, False),
                    ): bool
                }
            ),
        )
