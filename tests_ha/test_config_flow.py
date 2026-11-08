"""Pair through Core's actual flow manager and the native HTTP handshake."""

from uuid import UUID

import pytest
from homeassistant.config_entries import SOURCE_HASSIO, SOURCE_USER
from homeassistant.core import HomeAssistant
from homeassistant.data_entry_flow import FlowResultType
from homeassistant.helpers.service_info.hassio import HassioServiceInfo
from pytest_homeassistant.test_util.aiohttp import AiohttpClientMocker

from custom_components.downloads.const import (
    CONF_ALLOW_REQUESTS,
    CONF_BACKEND_URL,
    CONF_TOKEN,
    DOMAIN,
)

from .conftest import (
    LEGACY_HEALTH_URL,
    Backend,
    LegacySupervisor,
    health,
    legacy_health_record,
)


async def flow(hass: HomeAssistant, data: dict[str, str]) -> dict[str, object]:
    result = await hass.config_entries.flow.async_init(DOMAIN, context={"source": SOURCE_USER})
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], {"next_step_id": "standalone"}
    )
    return await hass.config_entries.flow.async_configure(result["flow_id"], data)


async def test_pairing_checks_dedicated_role_and_defaults_requests_off(
    hass: HomeAssistant, backend: Backend
) -> None:
    result = await flow(hass, {CONF_BACKEND_URL: backend.url, CONF_TOKEN: "test-integration-token"})
    assert result["type"] is FlowResultType.CREATE_ENTRY
    await hass.async_block_till_done()
    entry = result["result"]
    assert not entry.options.get(CONF_ALLOW_REQUESTS, False)
    assert backend.requests[0].headers["Authorization"] == "Bearer test-integration-token"
    assert "X-Downloads-Actor-Id" not in backend.requests[0].headers


@pytest.mark.parametrize("role", ["read_only", "requester", "administrator"])
async def test_reject_other_credential_roles(
    hass: HomeAssistant, backend: Backend, role: str
) -> None:
    backend.role = role
    result = await flow(hass, {CONF_BACKEND_URL: backend.url, CONF_TOKEN: "private-example-token"})
    assert result["errors"] == {"base": "integration_required"}
    assert not hass.config_entries.async_entries(DOMAIN)


@pytest.mark.parametrize(
    "url",
    [
        "http://user:password@example.invalid",
        "http://example.invalid/api/hassio_ingress/id/",
        "http://example.invalid?url=https://elsewhere.invalid",
        "http://example.invalid\\@elsewhere.invalid",
        "http://example.invalid:0",
    ],
)
async def test_invalid_origins_make_no_request(
    hass: HomeAssistant, backend: Backend, url: str
) -> None:
    result = await flow(hass, {CONF_BACKEND_URL: url, CONF_TOKEN: "test-integration-token"})
    assert result["errors"] == {CONF_BACKEND_URL: "invalid_url"}
    assert backend.requests == []


async def test_supervisor_discovery_requires_confirmation_before_auth(
    hass: HomeAssistant,
    backend: Backend,
    legacy_supervisor: LegacySupervisor,
    aioclient_mock: AiohttpClientMocker,
) -> None:
    legacy_supervisor.app.slug = "example_downloads"
    aioclient_mock.get(LEGACY_HEALTH_URL, json=legacy_health_record())
    aioclient_mock.get(
        backend.url + "/api/v1/system",
        json={
            "version": "0.2.0",
            "credential_role": "integration",
            "capabilities": {"read": True, "request": False, "admin": False},
            "transport": "native",
        },
    )
    aioclient_mock.get(backend.url + "/api/v1/health/stack", json=health())
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_HASSIO},
        data=HassioServiceInfo(
            config={
                CONF_BACKEND_URL: backend.url,
                CONF_TOKEN: "test-integration-token",
            },
            name="Downloads",
            slug="example_downloads",
            uuid=UUID("276319ec-c797-460c-a49e-6b90fc1f6ae8").hex,
        ),
    )
    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "hassio_confirm"
    assert backend.requests == []
    assert aioclient_mock.call_count == 0
    result = await hass.config_entries.flow.async_configure(result["flow_id"], {})
    assert result["type"] is FlowResultType.CREATE_ENTRY
    await hass.async_block_till_done()
