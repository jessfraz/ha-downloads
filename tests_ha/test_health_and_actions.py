"""Observe real entities, native actions and entry-owned expiration/cleanup."""

import json
from datetime import timedelta

from homeassistant.core import Context, HomeAssistant
from homeassistant.helpers import entity_registry as er
from homeassistant.util import dt as dt_util
from pytest_homeassistant.common import (
    MockUser,
    async_fire_time_changed,
)

from custom_components.downloads import DownloadsConfigEntry
from custom_components.downloads.access import gateway_entity_id
from custom_components.downloads.diagnostics import async_get_config_entry_diagnostics
from custom_components.downloads.models import Component

from .conftest import KEY, MOVIE_REQUEST, Backend, health


async def test_native_action_returns_structured_request_without_lookup_or_retry(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    hass_user: MockUser,
    backend: Backend,
) -> None:
    result = await hass.services.async_call(
        "downloads",
        "request",
        {"request": MOVIE_REQUEST, "idempotency_key": KEY},
        blocking=True,
        return_response=True,
        context=Context(user_id=hass_user.id),
    )
    assert result is not None and result["submission"]["state"] == "pending"
    writes = [item for item in backend.requests if item.method == "POST"]
    assert len(writes) == 1
    assert json.loads(writes[0].body) == MOVIE_REQUEST
    assert writes[0].headers["Idempotency-Key"] == KEY


async def test_component_and_readiness_evidence_expires_before_next_poll(
    hass: HomeAssistant, entry: DownloadsConfigEntry, backend: Backend, freezer: object
) -> None:
    backend.lifetime_ms = 1000
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    health_id = gateway_entity_id(hass, entry)
    ready_id = er.async_get(hass).async_get_entity_id(
        "binary_sensor", "downloads", f"{entry.entry_id}_request_movie_ready"
    )
    assert health_id is not None and ready_id is not None
    assert hass.states.get(health_id).state == "healthy"
    assert hass.states.get(ready_id).state == "on"
    freezer.tick(timedelta(seconds=2))
    async_fire_time_changed(hass, dt_util.utcnow())
    await hass.async_block_till_done()
    assert hass.states.get(health_id).state == "stale"
    assert hass.states.get(ready_id).state == "unknown"


async def test_diagnostics_are_cached_allowlist_and_unload_removes_entities(
    hass: HomeAssistant, entry: DownloadsConfigEntry, backend: Backend
) -> None:
    count = len(backend.requests)
    result = await async_get_config_entry_diagnostics(hass, entry)
    assert len(backend.requests) == count
    serialized = json.dumps(result)
    assert "test-integration-token" not in serialized
    assert backend.url not in serialized
    assert "Example title" not in serialized
    assert result["component_states"] == {component.value: "healthy" for component in Component}
    entity_id = gateway_entity_id(hass, entry)
    assert entity_id is not None
    coordinator = entry.runtime_data
    assert await hass.config_entries.async_unload(entry.entry_id)
    assert hass.states.get(entity_id).state == "unavailable"
    assert coordinator._cancel_expiry is None


async def test_retired_profile_target_is_not_accepted_as_readiness_evidence(
    hass: HomeAssistant, entry: DownloadsConfigEntry, backend: Backend
) -> None:
    response = health()
    response["readiness"][0]["profile_id"] = "276319ec-c797-460c-a49e-6b90fc1f6ae8"
    backend.responses[("GET", "/api/v1/health/stack")] = (200, response)
    await entry.runtime_data.async_refresh()
    assert not entry.runtime_data.last_update_success


async def test_by_key_recovery_is_a_read_only_native_action(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    hass_user: MockUser,
    backend: Backend,
) -> None:
    count = len(backend.requests)
    result = await hass.services.async_call(
        "downloads",
        "status",
        {"target": {"kind": "idempotency_key", "id": KEY}},
        blocking=True,
        return_response=True,
        context=Context(user_id=hass_user.id),
    )
    assert result is not None and result["submission"]["state"] == "reconciling"
    assert len(backend.requests) == count + 1
    assert backend.requests[-1].method == "GET"
    assert backend.requests[-1].path == f"/api/v1/request-keys/{KEY}"
