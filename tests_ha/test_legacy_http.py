"""Exercise the CLI health bridge through HA's real authentication middleware."""

from pathlib import Path

import pytest
from homeassistant.components.homeassistant.exposed_entities import async_expose_entity
from homeassistant.core import HomeAssistant
from pytest_homeassistant.common import MockUser
from pytest_homeassistant.test_util.aiohttp import AiohttpClientMocker
from pytest_homeassistant.typing import ClientSessionGenerator

from custom_components.downloads import DownloadsConfigEntry

URL = "/api/downloads/health"
ENTITY = "binary_sensor.downloads_problem"


async def test_http_health_requires_auth_and_read_permission_without_assist_exposure(
    hass: HomeAssistant,
    legacy_entry: DownloadsConfigEntry,
    hass_client: ClientSessionGenerator,
    hass_read_only_access_token: str,
    aioclient_mock: AiohttpClientMocker,
) -> None:
    anonymous = await hass_client(None)
    assert (await anonymous.get(URL)).status == 401
    client = await hass_client(hass_read_only_access_token)
    calls = aioclient_mock.call_count
    response = await client.get(URL)
    assert response.status == 200
    assert response.headers["Cache-Control"] == "no-store"
    data = await response.json()
    assert data["ready"] is True
    assert set(data) == {
        "ready",
        "state",
        "reasons",
        "checked_at_unix_ms",
        "age_ms",
        "max_age_ms",
    }
    assert aioclient_mock.call_count == calls
    assert (await client.get(URL + "?url=http://untrusted.example")).status == 400
    assert (await client.post(URL)).status == 405
    denied = MockUser().add_to_hass(hass)
    refresh = await hass.auth.async_create_refresh_token(
        denied, client_id="https://example.invalid"
    )
    token = hass.auth.async_create_access_token(refresh)
    denied_client = await hass_client(token)
    response = await denied_client.get(URL)
    assert response.status == 403
    assert await response.json() == {"error": "permission_denied"}


async def test_http_health_ages_cached_data_and_rejects_unloaded_entry(
    hass: HomeAssistant,
    legacy_entry: DownloadsConfigEntry,
    hass_client: ClientSessionGenerator,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async_expose_entity(hass, "conversation", ENTITY, True)
    client = await hass_client()
    await legacy_entry.runtime_data.async_shutdown()
    observed_at = legacy_entry.runtime_data.data.received_at
    monkeypatch.setattr(
        "custom_components.downloads.native_health.monotonic", lambda: observed_at + 35
    )
    response = await client.get(URL)
    assert response.status == 200
    data = await response.json()
    assert data["ready"] is False
    assert "checks_stale" in data["reasons"]
    assert await hass.config_entries.async_unload(legacy_entry.entry_id)
    response = await client.get(URL)
    assert response.status == 503
    assert await response.json() == {"error": "unavailable"}


async def test_real_cli_reads_native_ha_bridge_with_honest_exit_status(
    hass: HomeAssistant,
    legacy_entry: DownloadsConfigEntry,
    hass_client: ClientSessionGenerator,
    hass_read_only_access_token: str,
    monkeypatch: pytest.MonkeyPatch,
    socket_enabled: None,
    legacy_downloads_cli_binary: Path,
) -> None:
    """Run the actual Rust client against Core's authenticated HTTP listener."""
    import asyncio
    import json
    import os

    client = await hass_client(hass_read_only_access_token)
    environment = dict(os.environ)
    environment.update(
        {
            "HOME_ASSISTANT_URL": str(client.make_url("/")),
            "HOME_ASSISTANT_TOKEN": hass_read_only_access_token,
            "HTTP_PROXY": "http://127.0.0.1:1",
            "HTTPS_PROXY": "http://127.0.0.1:1",
            "ALL_PROXY": "http://127.0.0.1:1",
            "NO_PROXY": "",
        }
    )
    environment["DOWNLOADS_URL"] = "https://unused.example.invalid"
    environment["DOWNLOADS_TOKEN_FILE"] = "/unused/native-token"

    async def query() -> tuple[int, dict[str, object]]:
        process = await asyncio.create_subprocess_exec(
            str(legacy_downloads_cli_binary),
            "--transport",
            "home-assistant",
            "status",
            env=environment,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await process.communicate()
        assert hass_read_only_access_token.encode() not in stdout + stderr
        assert process.returncode is not None
        return process.returncode, json.loads(stdout or stderr)

    code, result = await query()
    assert code == 0
    assert result["transport"] == "home-assistant"
    assert isinstance(result["health"], dict)
    assert result["health"]["ready"] is True
    await legacy_entry.runtime_data.async_shutdown()
    observed_at = legacy_entry.runtime_data.data.received_at
    monkeypatch.setattr(
        "custom_components.downloads.native_health.monotonic", lambda: observed_at + 35
    )
    code, result = await query()
    assert code == 2
    assert isinstance(result["health"], dict)
    assert result["health"]["ready"] is False
    assert "checks_stale" in result["health"]["reasons"]
    assert await hass.config_entries.async_unload(legacy_entry.entry_id)
    code, result = await query()
    assert code == 1
    assert result["error"] == "unavailable"
    assert result["http_status"] == 503
