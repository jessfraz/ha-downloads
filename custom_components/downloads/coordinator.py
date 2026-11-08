"""Entry-owned cached projections of the backend's health decisions."""

import logging
from collections.abc import Callable
from datetime import timedelta

from aiohttp import ClientError
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.exceptions import ConfigEntryAuthFailed
from homeassistant.helpers.event import async_call_later
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator, UpdateFailed
from homeassistant.util import dt as dt_util

from .client import Actor, DownloadsClient, ResponseTooLarge
from .const import API_PREFIX, DOMAIN
from .models import HealthSnapshot, System
from .native_health import NativeHealthCoordinator

_LOGGER = logging.getLogger(__name__)


def now_ms() -> int:
    return int(dt_util.utcnow().timestamp() * 1000)


class DownloadsCoordinator(DataUpdateCoordinator[HealthSnapshot]):
    """Do not reproduce dependency logic or probe providers inside HA."""

    def __init__(
        self,
        hass: HomeAssistant,
        entry: ConfigEntry,
        client: DownloadsClient,
        native_health: NativeHealthCoordinator | None = None,
    ) -> None:
        super().__init__(
            hass,
            _LOGGER,
            name=DOMAIN,
            config_entry=entry,
            update_interval=timedelta(seconds=15),
            always_update=False,
        )
        self.client = client
        self.native_health = native_health
        self.system: System | None = None
        self._cancel_expiry: Callable[[], None] | None = None
        entry.async_on_unload(self.async_cancel_expiry)

    @callback
    def async_cancel_expiry(self) -> None:
        """The shared HTTP session is Core-owned; only this timer is ours."""
        if self._cancel_expiry is not None:
            self._cancel_expiry()
            self._cancel_expiry = None

    @callback
    def _schedule_expiry(self, data: HealthSnapshot) -> None:
        self.async_cancel_expiry()
        now = now_ms()
        expiries = [item.expires_at for item in data.readiness]
        expiries.extend(item.expires_at for item in data.components if item.expires_at is not None)
        future = [expiry for expiry in expiries if expiry > now]
        if future:
            self._cancel_expiry = async_call_later(
                self.hass, (min(future) - now) / 1000, self._expired
            )

    @callback
    def _expired(self, _now: object) -> None:
        """Invalidate cached entity values exactly when their evidence expires."""
        self._cancel_expiry = None
        if self.data is not None:
            self.async_update_listeners()
            self._schedule_expiry(self.data)

    async def _async_update_data(self) -> HealthSnapshot:
        try:
            system_response = await self.client.request("GET", f"{API_PREFIX}/system")
            if system_response.status in {401, 403}:
                raise ConfigEntryAuthFailed("The Downloads credential was rejected")
            if system_response.status != 200:
                raise UpdateFailed("Cannot read Downloads capabilities")
            system = System.parse(system_response.json())
            if system.role != "integration" or not system.read or system.admin:
                raise ConfigEntryAuthFailed("A dedicated Downloads integration token is required")
            response = await self.client.request(
                "GET", f"{API_PREFIX}/health/stack", actor=Actor("integration")
            )
            if response.status in {401, 403}:
                raise ConfigEntryAuthFailed("The Downloads credential was rejected")
            if response.status != 200:
                raise UpdateFailed("Cannot read Downloads health")
            data = HealthSnapshot.parse(response.json())
        except ClientError, TimeoutError, ResponseTooLarge, ValueError:
            raise UpdateFailed("Cannot read Downloads health") from None
        self.system = system
        self._schedule_expiry(data)
        return data
