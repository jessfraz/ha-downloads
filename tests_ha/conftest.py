"""Real Core, authentication, registry and HTTP fixtures; no provider credentials."""

import asyncio
import os
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from ipaddress import IPv4Address
from pathlib import Path
from uuid import UUID

import pytest
from aiohttp import web
from homeassistant.auth.const import GROUP_ID_USER
from homeassistant.auth.providers import AuthProvider
from homeassistant.core import HomeAssistant
from homeassistant.setup import async_setup_component
from homeassistant.util import dt as dt_util
from pytest_aiohttp.plugin import AiohttpServer
from pytest_homeassistant.common import MockConfigEntry, MockUser
from pytest_homeassistant.test_util.aiohttp import AiohttpClientMocker

from custom_components.downloads import DownloadsConfigEntry
from custom_components.downloads.const import (
    APP_URL,
    CONF_ADDON_SLUG,
    CONF_ALLOW_REQUESTS,
    CONF_BACKEND_URL,
    CONF_TOKEN,
    DOMAIN,
)
from custom_components.downloads.models import Component

REQUEST_ID = "2db98f62-f0f0-4c4b-b79b-a36eb2f90810"
KEY = "ba90478e-67aa-4fbc-944a-48928e841098"
MOVIE_REQUEST = {
    "kind": "movie",
    "tmdb_id": 14337,
    "intent": {"mode": "search_now"},
}
MOVIE_ITEM = {
    "kind": "movie",
    "tmdb_id": 14337,
    "title": "Example title",
    "year": 2004,
    "overview": "A fictional fixture.",
    "poster": None,
    "request": {"state": "ready"},
    "library": {"state": "not_added"},
    "acquisition": {"state": "not_requested"},
}
SERIES_ITEM = {
    "kind": "series",
    "tvdb_id": 10,
    "title": "Example series",
    "year": 2005,
    "overview": "A fictional fixture.",
    "poster": None,
    "request": {"state": "ready"},
    "library": {"state": "not_added"},
    "seasons": [],
    "acquisition": {"state": "not_requested"},
}


def health(*, lifetime_ms: int = 30000) -> dict[str, object]:
    now = int(dt_util.utcnow().timestamp() * 1000)
    return {
        "components": [
            {
                "component": component.value,
                "observation": {
                    "state": "observed",
                    "outcome": {"state": "healthy"},
                    "freshness": "fresh",
                    "observed_at_unix_ms": now,
                    "expires_at_unix_ms": now + lifetime_ms,
                },
            }
            for component in Component
        ],
        "readiness": [
            {
                "operation": "request_movie",
                "state": "ready",
                "reasons": [],
                "expires_at_unix_ms": now + lifetime_ms,
            }
        ],
        "generated_at_unix_ms": now,
    }


@dataclass
class Received:
    method: str
    path: str
    query: dict[str, str]
    headers: dict[str, str]
    body: bytes


@dataclass
class Backend:
    url: str = ""
    role: str = "integration"
    lifetime_ms: int = 30000
    requests: list[Received] = field(default_factory=list)
    responses: dict[tuple[str, str], tuple[int, object]] = field(default_factory=dict)
    response_headers: dict[tuple[str, str], dict[str, str]] = field(default_factory=dict)
    delay_path: str | None = None
    received: asyncio.Event = field(default_factory=asyncio.Event)
    release: asyncio.Event = field(default_factory=asyncio.Event)

    async def handle(self, request: web.Request) -> web.Response:
        self.requests.append(
            Received(
                request.method,
                request.path,
                dict(request.query),
                dict(request.headers),
                await request.read(),
            )
        )
        if request.path == self.delay_path:
            self.received.set()
            await self.release.wait()
        if response := self.responses.get((request.method, request.path)):
            status, body = response
            return web.json_response(
                body,
                status=status,
                headers=self.response_headers.get((request.method, request.path)),
            )
        if request.path == "/api/v1/system":
            return web.json_response(
                {
                    "version": "0.1.5",
                    "credential_role": self.role,
                    "capabilities": {
                        "read": True,
                        "request": False,
                        "admin": self.role == "administrator",
                    },
                    "transport": "native",
                }
            )
        if request.path == "/api/v1/health/stack":
            return web.json_response(health(lifetime_ms=self.lifetime_ms))
        if request.path == "/api/v1/search":
            return web.json_response(
                {
                    "items": [MOVIE_ITEM],
                    "next_page": None,
                    "coverage": [
                        {
                            "source": "sonarr",
                            "state": "unavailable",
                            "observed_at_unix_ms": 0,
                        }
                    ],
                }
            )
        if request.path == "/api/v1/requests" and request.method == "POST":
            return web.json_response(
                {
                    "id": REQUEST_ID,
                    "created_at_unix_ms": 0,
                    "input": MOVIE_REQUEST,
                    "submission": {"state": "pending"},
                },
                status=202,
            )
        if request.path == "/api/v1/movies/14337":
            return web.json_response(MOVIE_ITEM)
        return web.json_response(
            {
                "id": REQUEST_ID,
                "input": MOVIE_REQUEST,
                "submission": {
                    "state": "reconciling",
                    "reason": "upstream_outcome_unknown",
                },
            }
        )


@pytest.fixture(autouse=True)
def custom_integrations(enable_custom_integrations: None) -> None:
    """Use this checkout's integration, not an installed provider or harness."""


@pytest.fixture(autouse=True)
async def core_component(hass: HomeAssistant) -> None:
    assert await async_setup_component(hass, "homeassistant", {})


@pytest.fixture
async def hass_user(hass: HomeAssistant, local_auth: AuthProvider) -> MockUser:
    group = await hass.auth.async_get_group(GROUP_ID_USER)
    assert group is not None
    return MockUser(groups=[group]).add_to_hass(hass)


@pytest.fixture
async def hass_user_token(hass: HomeAssistant, hass_user: MockUser) -> str:
    refresh = await hass.auth.async_create_refresh_token(
        hass_user, client_id="https://example.invalid"
    )
    return hass.auth.async_create_access_token(refresh)


@pytest.fixture
async def backend(aiohttp_server: AiohttpServer, socket_enabled: None) -> AsyncIterator[Backend]:
    upstream = Backend()
    app = web.Application()
    app.router.add_route("*", "/{path:.*}", upstream.handle)
    server = await aiohttp_server(app)
    upstream.url = str(server.make_url("")).rstrip("/")
    yield upstream
    upstream.release.set()


@pytest.fixture
async def entry(hass: HomeAssistant, backend: Backend) -> DownloadsConfigEntry:
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Downloads",
        data={CONF_BACKEND_URL: backend.url, CONF_TOKEN: "test-integration-token"},
        options={CONF_ALLOW_REQUESTS: True},
    )
    entry.add_to_hass(hass)
    assert await hass.config_entries.async_setup(entry.entry_id)
    await hass.async_block_till_done()
    return entry


LEGACY_APP_SLUG = "testrepo_downloads"
LEGACY_HEALTH_URL = "http://172.30.33.7:8080/healthz"


def legacy_health_record(*, ready: bool = True) -> dict[str, object]:
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
class LegacyAppInfo:
    """Only the real Supervisor fields consumed by this adapter."""

    slug: str = LEGACY_APP_SLUG
    name: str = "Downloads"
    url: str = APP_URL
    state: str = "started"
    ip_address: IPv4Address = IPv4Address("172.30.33.7")


@dataclass
class LegacyDiscovery:
    service: str
    addon: str
    config: dict[str, str]
    uuid: UUID


class LegacyDiscoveryClient:
    def __init__(self) -> None:
        self.items: list[LegacyDiscovery] = []

    async def list(self) -> list[LegacyDiscovery]:
        return self.items


class LegacySupervisor:
    """A bounded external API fixture; all health parsing remains production code."""

    def __init__(self) -> None:
        self.app = LegacyAppInfo()
        self.addons = self
        self.discovery = LegacyDiscoveryClient()

    async def list(self) -> list[LegacyAppInfo]:
        return [self.app]

    async def addon_info(self, slug: str) -> LegacyAppInfo:
        assert slug == self.app.slug
        return self.app


@pytest.fixture
def legacy_supervisor(monkeypatch: pytest.MonkeyPatch) -> LegacySupervisor:
    client = LegacySupervisor()
    monkeypatch.setattr(
        "homeassistant.components.hassio.get_supervisor_client", lambda hass: client
    )
    monkeypatch.setattr("custom_components.downloads.config_flow.is_hassio", lambda hass: True)
    return client


@pytest.fixture
async def legacy_entry(
    hass: HomeAssistant,
    legacy_supervisor: LegacySupervisor,
    aioclient_mock: AiohttpClientMocker,
) -> DownloadsConfigEntry:
    aioclient_mock.get(LEGACY_HEALTH_URL, json=legacy_health_record())
    config = MockConfigEntry(
        domain=DOMAIN,
        unique_id=LEGACY_APP_SLUG,
        data={CONF_ADDON_SLUG: LEGACY_APP_SLUG},
    )
    config.add_to_hass(hass)
    assert await hass.config_entries.async_setup(config.entry_id)
    await hass.async_block_till_done()
    return config


@pytest.fixture
def legacy_downloads_cli_binary() -> Path:
    """Require the real built CLI rather than skipping its native HTTP proof."""
    binary = Path(
        os.environ.get(
            "DOWNLOADS_TEST_BINARY",
            Path(__file__).resolve().parents[1] / "target/debug/ha-downloads",
        )
    )
    assert binary.is_file(), "Build the CLI with cargo build --bin ha-downloads first"
    return binary
