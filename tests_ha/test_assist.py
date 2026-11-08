"""Use Core's real Assist tool discovery, schema converter and authenticated HTTP."""

import asyncio

import probatio
import pytest
from homeassistant.components.homeassistant.exposed_entities import async_expose_entity
from homeassistant.core import Context, HomeAssistant
from homeassistant.helpers import llm
from homeassistant.setup import async_setup_component
from homeassistant.util.json import JsonObjectType
from pytest_homeassistant.common import MockUser

from custom_components.downloads import DownloadsConfigEntry
from custom_components.downloads.access import gateway_entity_id
from custom_components.downloads.const import CONF_ALLOW_REQUESTS, DOMAIN

from .conftest import MOVIE_ITEM, MOVIE_REQUEST, Backend


async def api(hass: HomeAssistant, user: MockUser, entry: DownloadsConfigEntry) -> llm.APIInstance:
    entity_id = gateway_entity_id(hass, entry)
    assert entity_id is not None
    async_expose_entity(hass, "conversation", entity_id, True)
    assert await async_setup_component(hass, "llm", {})
    return await llm.async_get_api(
        hass,
        "assist",
        llm.LLMContext(
            platform="test",
            context=Context(user_id=user.id),
            language="en",
            assistant="conversation",
            device_id=None,
        ),
    )


async def call(
    instance: llm.APIInstance,
    name: str,
    args: JsonObjectType,
    *,
    tool_id: str = "one-call",
) -> llm.ToolResult:
    incoming = llm.ToolInput(tool_name=name, tool_args=args)
    incoming.id = tool_id
    return await instance.async_call_tool(incoming)


async def test_native_tools_and_real_provider_schema_conversion(
    hass: HomeAssistant, entry: DownloadsConfigEntry, hass_user: MockUser
) -> None:
    instance = await api(hass, hass_user, entry)
    tools = [tool for tool in instance.tools if tool.integration == DOMAIN]
    assert {tool.name for tool in tools} == {
        "downloads__search",
        "downloads__request",
        "downloads__status",
        "downloads__imports",
    }
    for tool in tools:
        schema = probatio.to_openapi(
            tool.parameters,
            custom_serializer=instance.custom_serializer,
            openapi_version="3.1.0",
        )
        assert schema["type"] == "object"
        assert schema["additionalProperties"] is False
    result = await call(instance, "downloads__search", {"query": "Example"})
    assert not result.error
    assert result.data["coverage"][0]["state"] == "unavailable"


async def test_request_uses_native_settings_and_stable_actor_bound_key(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    hass_user: MockUser,
    backend: Backend,
) -> None:
    instance = await api(hass, hass_user, entry)
    for tool_id in ("first-call", "model-retry-with-new-id"):
        result = await call(
            instance, "downloads__request", {"request": MOVIE_REQUEST}, tool_id=tool_id
        )
        assert not result.error
    writes = [item for item in backend.requests if item.method == "POST"]
    assert len(writes) == 2
    assert writes[0].body == writes[1].body
    assert writes[0].headers["Idempotency-Key"] == writes[1].headers["Idempotency-Key"]
    assert writes[0].headers["X-Downloads-Actor-Id"] == hass_user.id
    assert writes[0].headers["X-Downloads-Actor-Can-Request"] == "true"
    assert "X-Downloads-Admin" not in writes[0].headers


async def test_request_keys_canonicalize_seasons_and_distinguish_new_ha_contexts(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    hass_user: MockUser,
    backend: Backend,
) -> None:
    instance = await api(hass, hass_user, entry)
    for tool_id, seasons in (("first", [2, 1]), ("retry", [1, 2])):
        request = {
            "kind": "series",
            "tvdb_id": 10,
            "intent": {
                "mode": "search_now",
                "scope": {"kind": "seasons", "numbers": seasons},
            },
        }
        assert not (
            await call(instance, "downloads__request", {"request": request}, tool_id=tool_id)
        ).error
    writes = [item for item in backend.requests if item.method == "POST"]
    assert writes[0].body == writes[1].body
    assert writes[0].headers["Idempotency-Key"] == writes[1].headers["Idempotency-Key"]
    next_context = await api(hass, hass_user, entry)
    assert not (await call(next_context, "downloads__request", {"request": request})).error
    assert backend.requests[-1].headers["Idempotency-Key"] != writes[0].headers["Idempotency-Key"]


async def test_uncertain_request_error_exposes_original_key_for_status_recovery(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    hass_user: MockUser,
    backend: Backend,
) -> None:
    instance = await api(hass, hass_user, entry)
    backend.responses[("POST", "/api/v1/requests")] = (
        503,
        {"error_code": "upstream_unavailable", "message": "private upstream failure"},
    )
    result = await call(instance, "downloads__request", {"request": MOVIE_REQUEST})
    assert result.error
    key = result.data["idempotency_key"]
    original = next(item for item in backend.requests if item.method == "POST")
    assert key == original.headers["Idempotency-Key"]
    assert "private upstream" not in str(result.data)
    recovered = await call(
        instance,
        "downloads__status",
        {"target": {"kind": "idempotency_key", "id": key}},
    )
    assert not recovered.error
    assert backend.requests[-1].path == f"/api/v1/request-keys/{key}"


async def test_native_settings_error_preserves_key_and_directs_setup_to_parent(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    hass_user: MockUser,
    backend: Backend,
) -> None:
    instance = await api(hass, hass_user, entry)
    backend.responses[("POST", "/api/v1/requests")] = (
        409,
        {"error_code": "native_settings_required", "message": "private native configuration"},
    )
    result = await call(instance, "downloads__request", {"request": MOVIE_REQUEST})
    assert result.error and result.data["error"] == "native_settings_required"
    writes = [item for item in backend.requests if item.method == "POST"]
    assert len(writes) == 1
    assert result.data["idempotency_key"] == writes[0].headers["Idempotency-Key"]
    assert "Radarr or Sonarr" in result.data["message"]
    assert "private native" not in result.data["message"]


async def test_search_exposes_parent_setup_without_choosing_settings(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    hass_user: MockUser,
    backend: Backend,
) -> None:
    instance = await api(hass, hass_user, entry)
    backend.responses[("GET", "/api/v1/search")] = (
        200,
        {
            "items": [{**MOVIE_ITEM, "request": {"state": "parent_setup_required"}}],
            "coverage": [],
            "next_page": None,
        },
    )
    count = len(backend.requests)
    result = await call(instance, "downloads__search", {"query": "Example"})
    assert not result.error
    assert result.data["items"][0]["request"] == {
        "state": "parent_setup_required",
    }
    assert len(backend.requests) == count + 1
    assert backend.requests[-1].method == "GET"


async def test_redundant_setup_manager_is_not_returned_to_assist(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    hass_user: MockUser,
    backend: Backend,
) -> None:
    instance = await api(hass, hass_user, entry)
    backend.responses[("GET", "/api/v1/movies/14337")] = (
        200,
        {**MOVIE_ITEM, "request": {"state": "parent_setup_required", "manager": "sonarr"}},
    )
    result = await call(
        instance, "downloads__status", {"target": {"kind": "movie", "tmdb_id": 14337}}
    )
    assert result.error and result.data["error"] == "invalid_response"
    assert "request" not in result.data


@pytest.mark.parametrize(
    "invalid",
    [
        {**MOVIE_REQUEST, "tvdb_id": 1},
        {**MOVIE_REQUEST, "tmdb_id": True},
        {**MOVIE_REQUEST, "intent": {"mode": "search_now", "force": True}},
        {
            **MOVIE_REQUEST,
            "profile": {"id": "276319ec-c797-460c-a49e-6b90fc1f6ae8", "revision": 1},
        },
        {
            "kind": "series",
            "tvdb_id": 10,
            "intent": {
                "mode": "search_now",
                "scope": {"kind": "seasons", "numbers": [2, 2]},
            },
        },
    ],
)
async def test_invalid_requests_have_no_upstream_effects(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    hass_user: MockUser,
    backend: Backend,
    invalid: JsonObjectType,
) -> None:
    instance = await api(hass, hass_user, entry)
    count = len(backend.requests)
    result = await call(instance, "downloads__request", {"request": invalid})
    assert result.error
    assert len(backend.requests) == count


async def test_read_only_user_cannot_request(
    hass: HomeAssistant, entry: DownloadsConfigEntry, backend: Backend
) -> None:
    user = MockUser().add_to_hass(hass)
    entity_id = gateway_entity_id(hass, entry)
    assert entity_id is not None
    user.mock_policy({"entities": {"entity_ids": {entity_id: {"read": True}}}})
    instance = await api(hass, user, entry)
    assert not (await call(instance, "downloads__search", {"query": "Example"})).error
    count = len(backend.requests)
    result = await call(instance, "downloads__request", {"request": MOVIE_REQUEST})
    assert result.error and result.data["error"] == "permission_denied"
    assert len(backend.requests) == count


async def test_hidden_and_disabled_request_tools_are_not_discovered(
    hass: HomeAssistant, entry: DownloadsConfigEntry, hass_user: MockUser
) -> None:
    instance = await api(hass, hass_user, entry)
    entity_id = gateway_entity_id(hass, entry)
    assert entity_id is not None
    async_expose_entity(hass, "conversation", entity_id, False)
    hidden = await llm.async_get_api(hass, "assist", instance.llm_context)
    assert not any(tool.integration == DOMAIN for tool in hidden.tools)
    hass.config_entries.async_update_entry(entry, options={CONF_ALLOW_REQUESTS: False})
    await hass.async_block_till_done()
    read_only = await api(hass, hass_user, entry)
    assert {tool.name for tool in read_only.tools if tool.integration == DOMAIN} == {
        "downloads__search",
        "downloads__status",
        "downloads__imports",
    }


async def test_cached_tool_checks_exposure_after_network_await(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    hass_user: MockUser,
    backend: Backend,
) -> None:
    instance = await api(hass, hass_user, entry)
    backend.delay_path = "/api/v1/search"
    pending = hass.async_create_task(call(instance, "downloads__search", {"query": "Example"}))
    async with asyncio.timeout(5):
        await backend.received.wait()
    entity_id = gateway_entity_id(hass, entry)
    assert entity_id is not None
    async_expose_entity(hass, "conversation", entity_id, False)
    backend.release.set()
    result = await pending
    assert result.error and result.data["error"] == "not_exposed"
    assert "items" not in result.data


async def test_cached_tool_does_not_survive_unload_or_reload(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    hass_user: MockUser,
    backend: Backend,
) -> None:
    instance = await api(hass, hass_user, entry)
    assert await hass.config_entries.async_unload(entry.entry_id)
    count = len(backend.requests)
    result = await call(instance, "downloads__search", {"query": "Example"})
    assert result.error and result.data["error"] == "unavailable"
    assert len(backend.requests) == count
    assert await hass.config_entries.async_setup(entry.entry_id)
    await hass.async_block_till_done()
    count = len(backend.requests)
    result = await call(instance, "downloads__search", {"query": "Example"})
    assert result.error and result.data["error"] == "unavailable"
    assert len(backend.requests) == count
