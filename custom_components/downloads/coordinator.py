"""Poll only the existing internal, credential-free health endpoint."""

import asyncio
import json
import logging
from dataclasses import dataclass, field
from datetime import timedelta
from enum import StrEnum
from ipaddress import ip_address
from time import monotonic
from typing import Self

from aiohasupervisor import SupervisorError
from aiohttp import ClientError
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.helpers.aiohttp_client import async_get_clientsession
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator, UpdateFailed
from homeassistant.util.json import JsonObjectType

from .const import (
    APP_URL,
    CONF_ADDON_SLUG,
    DOMAIN,
    HEALTH_PORT,
    REQUEST_TIMEOUT_SECONDS,
)

_LOGGER = logging.getLogger(__name__)
_MAX_RESPONSE_BYTES = 16_384
_MAX_AGE_MS = 30_000


class Reason(StrEnum):
    """Allowlisted readiness reasons from the Rust service."""

    NOT_READY = "not_ready"
    CHECKS_MISSING = "checks_missing"
    CHECKS_STALE = "checks_stale"
    VPN_UNAVAILABLE = "vpn_unavailable"
    FIREWALL_UNVERIFIED = "firewall_unverified"
    DNS_UNVERIFIED = "dns_unverified"
    DOWNLOADER_UNAVAILABLE = "downloader_unavailable"
    DOWNLOADER_MODULES_UNAVAILABLE = "downloader_modules_unavailable"


@dataclass(frozen=True)
class HealthData:
    """A bounded projection with a monotonic age for cached Assist queries."""

    ready: bool
    state: str
    reasons: tuple[Reason, ...]
    checked_at_unix_ms: int | None
    age_ms: int | None
    received_at: float = field(default_factory=monotonic)

    @classmethod
    def parse(cls, value: object) -> Self:
        """Reject old or malformed contracts instead of inferring readiness."""
        if not isinstance(value, dict):
            raise ValueError("Invalid health response")
        ready, state, reasons = (
            value.get("ready"),
            value.get("state"),
            value.get("reasons"),
        )
        checked, age = value.get("checked_at_unix_ms"), value.get("age_ms")
        if (
            type(ready) is not bool
            or not isinstance(state, str)
            or state
            not in {"needs_configuration", "starting", "ready", "degraded", "stopping"}
            or not isinstance(reasons, list)
            or len(reasons) > len(Reason)
            or any(not isinstance(reason, str) for reason in reasons)
            or (checked is not None and (type(checked) is not int or checked < 0))
            or (age is not None and (type(age) is not int or age < 0))
            or value.get("max_age_ms") != _MAX_AGE_MS
        ):
            raise ValueError("Invalid health response")
        parsed_reasons = tuple(Reason(reason) for reason in reasons)
        if ready and (
            state != "ready"
            or parsed_reasons
            or checked is None
            or age is None
            or age > _MAX_AGE_MS
        ):
            raise ValueError("Invalid readiness evidence")
        return cls(ready, state, parsed_reasons, checked, age)

    def projection(self) -> JsonObjectType:
        """Never return connection identifiers, options or backend free text."""
        age = (
            self.age_ms + max(0, round((monotonic() - self.received_at) * 1000))
            if self.age_ms is not None
            else None
        )
        reasons = [reason.value for reason in self.reasons]
        if (
            age is not None
            and age > _MAX_AGE_MS
            and Reason.CHECKS_STALE not in self.reasons
        ):
            reasons.append(Reason.CHECKS_STALE.value)
        return {
            "ready": self.ready and age is not None and age <= _MAX_AGE_MS,
            "state": self.state,
            "reasons": reasons,
            "checked_at_unix_ms": self.checked_at_unix_ms,
            "age_ms": age,
            "max_age_ms": _MAX_AGE_MS,
        }


class DownloadsCoordinator(DataUpdateCoordinator[HealthData]):
    """Resolve the current app IP from Supervisor on every update."""

    def __init__(self, hass: HomeAssistant, entry: ConfigEntry) -> None:
        super().__init__(
            hass,
            _LOGGER,
            name=DOMAIN,
            config_entry=entry,
            update_interval=timedelta(seconds=10),
        )
        self.slug: str = entry.data[CONF_ADDON_SLUG]

    async def _async_update_data(self) -> HealthData:
        return await async_fetch_health(self.hass, self.slug)


async def async_fetch_health(hass: HomeAssistant, slug: str) -> HealthData:
    """Resolve only a Supervisor-owned private app endpoint, without credentials."""
    from homeassistant.components.hassio import get_supervisor_client

    try:
        async with asyncio.timeout(REQUEST_TIMEOUT_SECONDS):
            app = await get_supervisor_client(hass).addons.addon_info(slug)
            if app.url != APP_URL or app.state != "started":
                raise UpdateFailed("The Downloads app is not running.")
            address = ip_address(app.ip_address)
            if not address.is_private or address.is_unspecified or address.is_loopback:
                raise UpdateFailed("The Downloads app has no private address.")
            host = f"[{address}]" if address.version == 6 else str(address)
            async with async_get_clientsession(hass).get(
                f"http://{host}:{HEALTH_PORT}/healthz", allow_redirects=False
            ) as response:
                if response.status not in {200, 503}:
                    raise UpdateFailed("Downloads health is unavailable.")
                try:
                    body = await response.content.readexactly(_MAX_RESPONSE_BYTES + 1)
                except asyncio.IncompleteReadError as error:
                    body = error.partial
                if len(body) > _MAX_RESPONSE_BYTES:
                    raise UpdateFailed("Downloads health response is too large.")
                data = HealthData.parse(json.loads(body))
                if response.status == 503 and data.ready:
                    raise UpdateFailed("Downloads returned inconsistent health.")
                return data
    except SupervisorError, ClientError, TimeoutError, ValueError, TypeError:
        raise UpdateFailed("Downloads health could not be verified.") from None
