"""Run explicitly from the Rust harness against the real Dropshot HTTP service."""

import os

import pytest
from homeassistant.core import HomeAssistant
from pytest_homeassistant_custom_component.common import MockConfigEntry
from pytest_homeassistant_custom_component.typing import ClientSessionGenerator

from custom_components.downloads.const import (
    CONF_ALLOW_REQUESTS,
    CONF_BACKEND_URL,
    CONF_TOKEN,
    DOMAIN,
)


async def test_real_rust_contract(
    hass: HomeAssistant, hass_client: ClientSessionGenerator, socket_enabled: None
) -> None:
    if not (url := os.environ.get("DOWNLOADS_INTEROP_URL")):
        pytest.skip("Run through the Rust real_home_assistant_bridge harness")
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Downloads",
        data={CONF_BACKEND_URL: url, CONF_TOKEN: os.environ["DOWNLOADS_INTEROP_TOKEN"]},
        options={CONF_ALLOW_REQUESTS: True},
    )
    entry.add_to_hass(hass)
    assert await hass.config_entries.async_setup(entry.entry_id)
    await hass.async_block_till_done()
    client = await hass_client()
    system = await client.get("/api/downloads/v1/system")
    assert system.status == 200
    capabilities = (await system.json())["capabilities"]
    assert capabilities == {"read": True, "request": True, "admin": False}
    status = await client.get("/api/downloads/v1/status")
    assert status.status == 200
    assert (await status.json())["state"] == "needs_configuration"
    health = await client.get("/api/downloads/v1/health/stack")
    assert health.status == 200
    assert len((await health.json())["components"]) == 9
    search = await client.get("/api/downloads/v1/search?query=Primer&kind=movie")
    assert search.status == 200
    assert (await search.json())["items"][0]["tmdb_id"] == 14337
    key = "e482ef73-217c-49d1-8eb4-a0eaa680a33c"
    request = {
        "kind": "movie",
        "tmdb_id": 14337,
        "profile": {"id": os.environ["DOWNLOADS_INTEROP_PROFILE"], "revision": 1},
        "intent": {"mode": "search_now"},
    }
    response = await client.post(
        "/api/downloads/v1/requests", json=request, headers={"Idempotency-Key": key}
    )
    assert response.status == 202
    receipt = await response.json()
    assert response.headers["Location"].endswith(receipt["id"])
    recovered = await client.get(f"/api/downloads/v1/request-keys/{key}")
    assert recovered.status == 200
    assert (await recovered.json())["id"] == receipt["id"]
    replay = await client.post(
        "/api/downloads/v1/requests", json=request, headers={"Idempotency-Key": key}
    )
    assert replay.status == 202
    assert (await replay.json())["id"] == receipt["id"]
    denied = await client.post("/api/downloads/v1/profiles", json={})
    assert denied.status == 405
    assert await hass.config_entries.async_unload(entry.entry_id)
