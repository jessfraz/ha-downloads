"""A fixed, authenticated read-only health view using HA's existing listener."""

from aiohttp import web
from homeassistant.core import HomeAssistant
from homeassistant.helpers.http import HomeAssistantView

from .access import HealthAccessError, HealthErrorCode, async_health_snapshot
from .const import DOMAIN


class DownloadsHealthView(HomeAssistantView):
    """Never forward HA credentials, accept destinations, or fetch app options."""

    url = "/api/downloads/health"
    name = "api:downloads:health"
    requires_auth = True

    def __init__(self, hass: HomeAssistant) -> None:
        self.hass = hass

    async def get(self, request: web.Request) -> web.Response:
        """Read cached health using the authenticated user's entity permission."""
        headers = {"Cache-Control": "no-store"}
        if request.query:
            return self.json({"error": "invalid_arguments"}, 400, headers)
        for entry in self.hass.config_entries.async_loaded_entries(DOMAIN):
            try:
                result = await async_health_snapshot(
                    self.hass,
                    entry,
                    entry.runtime_data,
                    request["hass_user"].id,
                    None,
                )
            except HealthAccessError as error:
                code = 503 if error.code is HealthErrorCode.UNAVAILABLE else 403
                return self.json({"error": error.code.value}, code, headers)
            return self.json(result, headers=headers)
        return self.json({"error": "unavailable"}, 503, headers)
