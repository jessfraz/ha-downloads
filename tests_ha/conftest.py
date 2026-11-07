"""Real Home Assistant entities with mocked Supervisor and HTTP boundaries."""

from dataclasses import dataclass
from ipaddress import IPv4Address

import pytest
from homeassistant.core import HomeAssistant
from homeassistant.setup import async_setup_component

from custom_components.downloads.const import APP_URL

APP_SLUG = "testrepo_downloads"
HEALTH_URL = "http://172.30.33.7:8080/healthz"


def health_record(*, ready: bool = True) -> dict[str, object]:
    """The Rust producer's credential-free public health contract."""
    return {
        "ready": ready,
        "state": "ready" if ready else "degraded",
        "reasons": [] if ready else ["not_ready", "vpn_unavailable"],
        "checked_at_unix_ms": 1_800_000_000_000,
        "age_ms": 100,
        "max_age_ms": 30_000,
        "last_failure": None,
    }


@dataclass
class AppInfo:
    """Only the real Supervisor fields consumed by this adapter."""

    slug: str = APP_SLUG
    name: str = "Downloads"
    url: str = APP_URL
    state: str = "started"
    ip_address: IPv4Address = IPv4Address("172.30.33.7")


class Supervisor:
    """A bounded external API fixture; all health parsing remains production code."""

    def __init__(self) -> None:
        self.app = AppInfo()
        self.addons = self

    async def list(self) -> list[AppInfo]:
        return [self.app]

    async def addon_info(self, slug: str) -> AppInfo:
        assert slug == self.app.slug
        return self.app


@pytest.fixture(autouse=True)
def custom_integrations(enable_custom_integrations: None) -> None:
    """Enable the integration from this checkout."""


@pytest.fixture(autouse=True)
async def core(hass: HomeAssistant) -> None:
    """Use Core's real exposure registry and Assist API."""
    assert await async_setup_component(hass, "homeassistant", {})


@pytest.fixture
def supervisor(monkeypatch: pytest.MonkeyPatch) -> Supervisor:
    client = Supervisor()
    monkeypatch.setattr(
        "homeassistant.components.hassio.get_supervisor_client", lambda hass: client
    )
    monkeypatch.setattr(
        "custom_components.downloads.config_flow.is_hassio", lambda hass: True
    )
    return client
