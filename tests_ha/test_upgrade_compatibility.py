"""Prove the deployed protection monitor survives deliberate media pairing."""

from dataclasses import replace
from uuid import UUID

from homeassistant.components.homeassistant.exposed_entities import (
    async_expose_entity,
    async_should_expose,
)
from homeassistant.config_entries import SOURCE_RECONFIGURE
from homeassistant.core import HomeAssistant
from homeassistant.data_entry_flow import FlowResultType
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers import entity_registry as er
from homeassistant.helpers.llm import ToolInput
from pytest_homeassistant.common import MockUser
from pytest_homeassistant.test_util.aiohttp import AiohttpClientMocker
from pytest_homeassistant.typing import ClientSessionGenerator

from custom_components.downloads import DownloadsConfigEntry
from custom_components.downloads.const import (
    CONF_ADDON_SLUG,
    CONF_ALLOW_REQUESTS,
    CONF_BACKEND_URL,
    CONF_TOKEN,
    DOMAIN,
)
from custom_components.downloads.coordinator import DownloadsCoordinator
from custom_components.downloads.native_health import NativeHealthCoordinator

from .conftest import (
    LEGACY_APP_SLUG,
    LegacyDiscovery,
    LegacySupervisor,
    health,
)
from .test_legacy_health import assist_api


async def test_health_only_entry_migrates_without_credentials_or_new_exposure(
    hass: HomeAssistant, legacy_entry: DownloadsConfigEntry
) -> None:
    assert legacy_entry.version == 2
    assert legacy_entry.unique_id == LEGACY_APP_SLUG
    assert legacy_entry.data == {CONF_ADDON_SLUG: LEGACY_APP_SLUG}
    assert isinstance(legacy_entry.runtime_data, NativeHealthCoordinator)
    problem = er.async_get(hass).async_get("binary_sensor.downloads_problem")
    assert problem is not None
    assert problem.unique_id == f"{LEGACY_APP_SLUG}_problem"
    assert (
        er.async_get(hass).async_get_entity_id(
            "sensor", DOMAIN, f"{legacy_entry.entry_id}_stack_health"
        )
        is None
    )
    assert not async_should_expose(hass, "conversation", problem.entity_id)


async def test_same_entry_media_pairing_preserves_problem_device_and_health_only_exposure(
    hass: HomeAssistant,
    legacy_entry: DownloadsConfigEntry,
    legacy_supervisor: LegacySupervisor,
    aioclient_mock: AiohttpClientMocker,
    hass_read_only_user: MockUser,
    hass_client: ClientSessionGenerator,
) -> None:
    problem_id = "binary_sensor.downloads_problem"
    async_expose_entity(hass, "conversation", problem_id, True)
    registry = er.async_get(hass)
    before = registry.async_get(problem_id)
    assert before is not None
    old_device_id = before.device_id
    old_entry_id = legacy_entry.entry_id
    old_health_api = await assist_api(hass, hass_read_only_user.id)
    assert {tool.name for tool in old_health_api.tools if tool.integration == DOMAIN} == {
        "downloads__health"
    }
    origin = "http://downloads.example:8080"
    legacy_supervisor.discovery.items = [
        LegacyDiscovery(
            service=DOMAIN,
            addon=LEGACY_APP_SLUG,
            config={CONF_BACKEND_URL: origin, CONF_TOKEN: "test-integration-token"},
            uuid=UUID("276319ec-c797-460c-a49e-6b90fc1f6ae8"),
        )
    ]
    aioclient_mock.get(
        origin + "/api/v1/system",
        json={
            "version": "0.2.0",
            "credential_role": "integration",
            "capabilities": {"read": True, "request": False, "admin": False},
            "transport": "native",
        },
    )
    aioclient_mock.get(origin + "/api/v1/health/stack", json=health())
    calls = aioclient_mock.call_count
    flow = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_RECONFIGURE, "entry_id": legacy_entry.entry_id},
    )
    assert flow["type"] is FlowResultType.FORM
    assert flow["step_id"] == "hassio_confirm"
    assert aioclient_mock.call_count == calls
    result = await hass.config_entries.flow.async_configure(flow["flow_id"], {})
    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "reconfigure_successful"
    await hass.async_block_till_done()
    assert legacy_entry.entry_id == old_entry_id
    assert legacy_entry.unique_id == LEGACY_APP_SLUG
    assert legacy_entry.data[CONF_ADDON_SLUG] == LEGACY_APP_SLUG
    assert isinstance(legacy_entry.runtime_data, DownloadsCoordinator)
    assert legacy_entry.runtime_data.native_health is not None
    after = registry.async_get(problem_id)
    assert after is not None
    assert after.unique_id == before.unique_id
    assert after.device_id == old_device_id
    device = dr.async_get(hass).async_get_device_by_identifier(
        (DOMAIN, LEGACY_APP_SLUG), legacy_entry.entry_id
    )
    assert device is not None and device.id == old_device_id
    assert async_should_expose(hass, "conversation", problem_id)
    stack_id = registry.async_get_entity_id(
        "sensor", DOMAIN, f"{legacy_entry.entry_id}_stack_health"
    )
    assert stack_id is not None
    assert not async_should_expose(hass, "conversation", stack_id)
    assert not legacy_entry.options.get(CONF_ALLOW_REQUESTS, False)
    assert {
        tool.name
        for tool in (await assist_api(hass, hass_read_only_user.id)).tools
        if tool.integration == DOMAIN
    } == {"downloads__health"}
    # Cached tools cannot silently acquire a replacement runtime after reload.
    stale = await old_health_api.async_call_tool(
        ToolInput(tool_name="downloads__health", tool_args={})
    )
    assert stale.error and stale.data["error"] == "unavailable"
    client = await hass_client()
    response = await client.get("/api/downloads/health")
    assert response.status == 200
    assert (await response.json())["ready"] is True
    async_expose_entity(hass, "conversation", stack_id, True)
    combined = await assist_api(hass, hass_read_only_user.id)
    assert {tool.name for tool in combined.tools if tool.integration == DOMAIN} == {
        "downloads__health",
        "downloads__search",
        "downloads__status",
    }
    without_assistant = replace(combined, llm_context=replace(combined.llm_context, assistant=None))
    denied = await without_assistant.async_call_tool(
        ToolInput(
            tool_name="downloads__status",
            tool_args={"target": {"kind": "movie", "tmdb_id": 14337}},
        )
    )
    assert denied.error and denied.data["error"] == "not_exposed"


async def test_wrong_app_discovery_cannot_replace_the_installed_health_identity(
    hass: HomeAssistant,
    legacy_entry: DownloadsConfigEntry,
    legacy_supervisor: LegacySupervisor,
    aioclient_mock: AiohttpClientMocker,
) -> None:
    legacy_supervisor.discovery.items = [
        LegacyDiscovery(
            service=DOMAIN,
            addon="another_downloads",
            config={
                CONF_BACKEND_URL: "http://other.example:8080",
                CONF_TOKEN: "other-token",
            },
            uuid=UUID("276319ec-c797-460c-a49e-6b90fc1f6ae8"),
        )
    ]
    calls = aioclient_mock.call_count
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_RECONFIGURE, "entry_id": legacy_entry.entry_id},
    )
    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "app"
    assert result["errors"] == {"base": "app_not_found"}
    assert aioclient_mock.call_count == calls
    assert legacy_entry.data == {CONF_ADDON_SLUG: LEGACY_APP_SLUG}
    assert legacy_entry.unique_id == LEGACY_APP_SLUG
