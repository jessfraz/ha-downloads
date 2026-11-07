"""Exercise native configuration, entities, transport failures and Assist access."""

from dataclasses import replace
from datetime import timedelta
from ipaddress import IPv4Address
from time import monotonic

import pytest
from homeassistant.components.homeassistant.exposed_entities import async_expose_entity
from homeassistant.config_entries import SOURCE_USER
from homeassistant.core import Context, HomeAssistant
from homeassistant.data_entry_flow import FlowResultType
from homeassistant.helpers.llm import APIInstance, LLMContext, ToolInput, async_get_api
from homeassistant.setup import async_setup_component
from homeassistant.util import dt as dt_util
from pytest_homeassistant_custom_component.common import (
    MockUser,
    async_fire_time_changed,
)
from pytest_homeassistant_custom_component.test_util.aiohttp import AiohttpClientMocker

from custom_components.downloads import DownloadsConfigEntry
from custom_components.downloads.const import CONF_ADDON_SLUG, DOMAIN
from custom_components.downloads.coordinator import HealthData

from .conftest import APP_SLUG, HEALTH_URL, Supervisor, health_record


async def test_native_config_flow_selects_only_the_installed_app(
    hass: HomeAssistant, supervisor: Supervisor, aioclient_mock: AiohttpClientMocker
) -> None:
    aioclient_mock.get(HEALTH_URL, json=health_record())
    result = await hass.config_entries.flow.async_init(
        DOMAIN, context={"source": SOURCE_USER}
    )
    assert result["type"] is FlowResultType.FORM
    assert aioclient_mock.call_count == 0
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], {CONF_ADDON_SLUG: APP_SLUG}
    )
    assert result["type"] is FlowResultType.CREATE_ENTRY
    assert result["data"] == {CONF_ADDON_SLUG: APP_SLUG}
    assert aioclient_mock.call_count >= 1
    await hass.async_block_till_done()


async def test_native_problem_sensor_recovers_after_transport_failure(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    aioclient_mock: AiohttpClientMocker,
) -> None:
    entity = "binary_sensor.downloads_problem"
    assert hass.states.get(entity).state == "off"
    assert hass.states.get(entity).attributes["reasons"] == []
    aioclient_mock.clear_requests()
    aioclient_mock.get(HEALTH_URL, status=503, json=health_record(ready=False))
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    assert hass.states.get(entity).state == "on"
    assert "vpn_unavailable" in hass.states.get(entity).attributes["reasons"]
    aioclient_mock.clear_requests()
    aioclient_mock.get(HEALTH_URL, status=502)
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    assert hass.states.get(entity).state == "unavailable"
    aioclient_mock.clear_requests()
    aioclient_mock.get(HEALTH_URL, json=health_record())
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    assert hass.states.get(entity).state == "off"


async def test_address_lookup_tracks_supervisor_and_never_sends_credentials(
    entry: DownloadsConfigEntry,
    supervisor: Supervisor,
    aioclient_mock: AiohttpClientMocker,
) -> None:
    supervisor.app.ip_address = IPv4Address("172.30.33.8")
    aioclient_mock.get("http://172.30.33.8:8080/healthz", json=health_record())
    await entry.runtime_data.async_refresh()
    assert entry.runtime_data.last_update_success
    _, url, _, headers = aioclient_mock.mock_calls[-1]
    assert str(url) == "http://172.30.33.8:8080/healthz"
    assert "Authorization" not in (headers or {})
    assert "Cookie" not in (headers or {})


async def test_stopped_app_and_malformed_health_never_become_healthy(
    entry: DownloadsConfigEntry,
    supervisor: Supervisor,
    aioclient_mock: AiohttpClientMocker,
) -> None:
    supervisor.app.state = "stopped"
    before = aioclient_mock.call_count
    await entry.runtime_data.async_refresh()
    assert not entry.runtime_data.last_update_success
    assert aioclient_mock.call_count == before
    supervisor.app.state = "started"
    aioclient_mock.clear_requests()
    aioclient_mock.get(HEALTH_URL, json={"state": "ready"})
    await entry.runtime_data.async_refresh()
    assert not entry.runtime_data.last_update_success


def test_cached_health_ages_without_io_and_omits_unknown_fields() -> None:
    record = health_record()
    record["unexpected_secret"] = "never expose this"
    snapshot = replace(HealthData.parse(record), received_at=monotonic() - 31)
    result = snapshot.projection()
    assert result["ready"] is False
    assert "checks_stale" in result["reasons"]
    assert "unexpected_secret" not in result


async def assist_api(hass: HomeAssistant, user_id: str) -> APIInstance:
    assert await async_setup_component(hass, "llm", {})
    return await async_get_api(
        hass,
        "assist",
        LLMContext(
            platform="test",
            context=Context(user_id=user_id),
            language="en",
            assistant="conversation",
            device_id=None,
        ),
    )


async def test_native_assist_requires_exposure_user_read_permission_and_current_entry(
    hass: HomeAssistant, entry: DownloadsConfigEntry, hass_read_only_user: MockUser
) -> None:
    entity = "binary_sensor.downloads_problem"
    api = await assist_api(hass, hass_read_only_user.id)
    assert not any(tool.integration == DOMAIN for tool in api.tools)
    async_expose_entity(hass, "conversation", entity, True)
    api = await assist_api(hass, hass_read_only_user.id)
    assert {tool.name for tool in api.tools if tool.integration == DOMAIN} == {
        "downloads__health"
    }
    tool = next(tool for tool in api.tools if tool.integration == DOMAIN)
    assert tool.annotations.read_only is True
    without_assistant_api = await async_get_api(
        hass, "assist", replace(api.llm_context, assistant=None)
    )
    assert not any(tool.integration == DOMAIN for tool in without_assistant_api.tools)
    result = await api.async_call_tool(
        ToolInput(tool_name="downloads__health", tool_args={})
    )
    assert not result.error
    assert result.data["ready"] is True
    denied = MockUser().add_to_hass(hass)
    denied_api = await assist_api(hass, denied.id)
    denied_result = await denied_api.async_call_tool(
        ToolInput(tool_name="downloads__health", tool_args={})
    )
    assert denied_result.error
    assert denied_result.data["error"] == "permission_denied"
    result = await api.async_call_tool(
        ToolInput(tool_name="downloads__health", tool_args={"restart": True})
    )
    assert result.error
    assert result.data["error"] == "invalid_arguments"
    async_expose_entity(hass, "conversation", entity, False)
    result = await api.async_call_tool(
        ToolInput(tool_name="downloads__health", tool_args={})
    )
    assert result.error
    assert result.data["error"] == "not_exposed"
    # A cached tool cannot turn a missing assistant into an HTTP-style query.
    without_assistant = replace(api, llm_context=replace(api.llm_context, assistant=None))
    result = await without_assistant.async_call_tool(
        ToolInput(tool_name="downloads__health", tool_args={})
    )
    assert result.error
    assert result.data["error"] == "not_exposed"
    async_expose_entity(hass, "conversation", entity, True)
    assert await hass.config_entries.async_unload(entry.entry_id)
    result = await api.async_call_tool(
        ToolInput(tool_name="downloads__health", tool_args={})
    )
    assert result.error
    assert result.data["error"] == "unavailable"


async def test_installed_problem_sensor_ages_when_its_poller_stops(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    aioclient_mock: AiohttpClientMocker,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    entity = "binary_sensor.downloads_problem"
    assert hass.states.get(entity).state == "off"
    await entry.runtime_data.async_shutdown()
    calls = aioclient_mock.call_count
    # Advance the snapshot's monotonic clock without running its stopped poller.
    observed_at = entry.runtime_data.data.received_at
    monkeypatch.setattr(
        "custom_components.downloads.coordinator.monotonic", lambda: observed_at + 35
    )
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(seconds=35))
    await hass.async_block_till_done()
    assert aioclient_mock.call_count == calls
    assert hass.states.get(entity).state == "on"
    assert "checks_stale" in hass.states.get(entity).attributes["reasons"]
    assert await hass.config_entries.async_unload(entry.entry_id)
    await hass.async_block_till_done()
    unloaded = hass.states.get(entity)
    assert unloaded.state == "unavailable"
    assert unloaded.attributes["restored"] is True
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(seconds=70))
    await hass.async_block_till_done()
    assert hass.states.get(entity) is unloaded
    assert aioclient_mock.call_count == calls
