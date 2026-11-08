"""Real HA HTTP authentication, route boundaries and authenticated attribution."""

import json

import pytest
from homeassistant.core import HomeAssistant
from pytest_homeassistant.common import MockUser
from pytest_homeassistant.typing import ClientSessionGenerator

from custom_components.downloads import DownloadsConfigEntry
from custom_components.downloads.const import (
    CONF_ALLOW_REQUESTS,
    MAX_REQUEST_BYTES,
    MAX_RESPONSE_BYTES,
)

from .conftest import KEY, MOVIE_ITEM, MOVIE_REQUEST, SERIES_ITEM, Backend

PREFIX = "/api/downloads/v1"


async def test_bridge_auth_and_request_actor_cannot_be_forged(
    hass_client: ClientSessionGenerator,
    hass_user_token: str,
    hass_user: MockUser,
    entry: DownloadsConfigEntry,
    backend: Backend,
) -> None:
    anonymous = await hass_client(None)
    assert (await anonymous.get(f"{PREFIX}/search?query=Example")).status == 401
    client = await hass_client(hass_user_token)
    response = await client.post(
        f"{PREFIX}/requests",
        json=MOVIE_REQUEST,
        headers={
            "Idempotency-Key": KEY,
            "X-Downloads-Actor-Id": "forged",
            "X-Downloads-Actor-Can-Request": "false",
            "X-Downloads-Admin": "true",
        },
    )
    assert response.status == 202
    write = next(item for item in backend.requests if item.method == "POST")
    assert write.headers["Authorization"] == "Bearer test-integration-token"
    assert write.headers["X-Downloads-Actor-Id"] == hass_user.id
    assert write.headers["X-Downloads-Actor-Can-Request"] == "true"
    assert "X-Downloads-Admin" not in write.headers
    assert write.headers["Idempotency-Key"] == KEY
    assert json.loads(write.body) == MOVIE_REQUEST
    assert response.headers["Location"].startswith(f"{PREFIX}/requests/")


@pytest.mark.parametrize(
    "method,path",
    [
        ("put", "profiles/276319ec-c797-460c-a49e-6b90fc1f6ae8"),
        ("post", "profiles"),
        ("get", "profiles"),
        ("get", "profiles/276319ec-c797-460c-a49e-6b90fc1f6ae8"),
        ("get", "managers/radarr/options"),
        ("get", "managers/sonarr/options"),
        ("get", "sabnzbd/api"),
        ("post", "vpn/stop"),
        ("get", "../system"),
        ("get", "managers/radarr/credentials"),
    ],
)
async def test_no_admin_or_generic_proxy_routes(
    hass_client: ClientSessionGenerator,
    hass_user_token: str,
    entry: DownloadsConfigEntry,
    backend: Backend,
    method: str,
    path: str,
) -> None:
    client = await hass_client(hass_user_token)
    count = len(backend.requests)
    response = await client.request(method, f"{PREFIX}/{path}")
    assert response.status in {404, 405}
    assert len(backend.requests) == count


async def test_retired_settings_are_rejected_without_backend_io(
    hass_client: ClientSessionGenerator,
    hass_user_token: str,
    entry: DownloadsConfigEntry,
    backend: Backend,
) -> None:
    client = await hass_client(hass_user_token)
    count = len(backend.requests)
    for field in ("profile", "quality_profile_id", "root_folder_id"):
        response = await client.post(
            f"{PREFIX}/requests",
            json={**MOVIE_REQUEST, field: 1},
            headers={"Idempotency-Key": KEY},
        )
        assert response.status == 400
    response = await client.get(
        f"{PREFIX}/health/stack?profile_id=276319ec-c797-460c-a49e-6b90fc1f6ae8"
    )
    assert response.status == 400
    assert len(backend.requests) == count


async def test_parent_setup_error_is_public_but_native_message_is_not(
    hass_client: ClientSessionGenerator,
    hass_user_token: str,
    entry: DownloadsConfigEntry,
    backend: Backend,
) -> None:
    backend.responses[("POST", "/api/v1/requests")] = (
        409,
        {"error_code": "native_settings_required", "message": "private upstream settings"},
    )
    client = await hass_client(hass_user_token)
    response = await client.post(
        f"{PREFIX}/requests", json=MOVIE_REQUEST, headers={"Idempotency-Key": KEY}
    )
    assert response.status == 409
    body = await response.json()
    assert body["error_code"] == "native_settings_required"
    assert "Radarr or Sonarr" in body["message"]
    assert "private upstream" not in body["message"]


async def test_parent_setup_availability_is_typed_for_both_media_kinds(
    hass_client: ClientSessionGenerator,
    hass_user_token: str,
    entry: DownloadsConfigEntry,
    backend: Backend,
) -> None:
    items = [
        {**MOVIE_ITEM, "request": {"state": "parent_setup_required"}},
        {**SERIES_ITEM, "request": {"state": "parent_setup_required"}},
    ]
    backend.responses[("GET", "/api/v1/search")] = (
        200,
        {"items": items, "next_page": None, "coverage": []},
    )
    client = await hass_client(hass_user_token)
    response = await client.get(f"{PREFIX}/search?query=Example")
    assert response.status == 200
    assert (await response.json())["items"] == items
    backend.responses[("GET", "/api/v1/series/10")] = (
        200,
        {**SERIES_ITEM, "request": {"state": "parent_setup_required", "manager": "radarr"}},
    )
    response = await client.get(f"{PREFIX}/series/10")
    assert response.status == 502
    assert (await response.json())["error_code"] == "invalid_response"


@pytest.mark.parametrize(
    "availability",
    [
        {"state": "ready", "manager": "radarr"},
        {"state": "parent_setup_required", "manager": "sonarr"},
        {},
        {"state": "unknown"},
    ],
)
async def test_invalid_native_request_availability_is_not_bridged(
    hass_client: ClientSessionGenerator,
    hass_user_token: str,
    entry: DownloadsConfigEntry,
    backend: Backend,
    availability: dict[str, object],
) -> None:
    backend.responses[("GET", "/api/v1/search")] = (
        200,
        {"items": [{**MOVIE_ITEM, "request": availability}], "next_page": None, "coverage": []},
    )
    client = await hass_client(hass_user_token)
    response = await client.get(f"{PREFIX}/search?query=Example")
    assert response.status == 502
    assert (await response.json())["error_code"] == "invalid_response"


@pytest.mark.parametrize(
    "retired",
    [
        {"default_profile": {"id": "276319ec-c797-460c-a49e-6b90fc1f6ae8", "revision": 1}},
        {"library": {"state": "managed", "upstream_id": 1, "quality_profile_id": 1}},
        {"library": {"state": "managed", "upstream_id": 1, "quality_profile_name": "Private"}},
    ],
)
async def test_retired_native_settings_metadata_is_not_bridged(
    hass_client: ClientSessionGenerator,
    hass_user_token: str,
    entry: DownloadsConfigEntry,
    backend: Backend,
    retired: dict[str, object],
) -> None:
    backend.responses[("GET", "/api/v1/movies/14337")] = (200, {**MOVIE_ITEM, **retired})
    client = await hass_client(hass_user_token)
    response = await client.get(f"{PREFIX}/movies/14337")
    assert response.status == 502
    assert (await response.json())["error_code"] == "invalid_response"


async def test_invalid_write_and_duplicate_query_never_reach_backend(
    hass_client: ClientSessionGenerator,
    hass_user_token: str,
    entry: DownloadsConfigEntry,
    backend: Backend,
) -> None:
    client = await hass_client(hass_user_token)
    count = len(backend.requests)
    invalid = {**MOVIE_REQUEST, "tvdb_id": 12}
    assert (
        await client.post(f"{PREFIX}/requests", json=invalid, headers={"Idempotency-Key": KEY})
    ).status == 400
    assert (await client.get(f"{PREFIX}/search?query=Example&query=Other")).status == 400
    assert len(backend.requests) == count


async def test_request_opt_in_is_checked_each_time(
    hass: HomeAssistant,
    hass_client: ClientSessionGenerator,
    hass_user_token: str,
    entry: DownloadsConfigEntry,
    backend: Backend,
) -> None:
    client = await hass_client(hass_user_token)
    hass.config_entries.async_update_entry(entry, options={CONF_ALLOW_REQUESTS: False})
    count = len(backend.requests)
    assert (
        await client.post(
            f"{PREFIX}/requests", json=MOVIE_REQUEST, headers={"Idempotency-Key": KEY}
        )
    ).status in {403, 503}
    assert not any(item.method == "POST" for item in backend.requests[count:])
    await hass.async_block_till_done()


async def test_unknown_upstream_error_is_sanitized_without_retry(
    hass_client: ClientSessionGenerator,
    hass_user_token: str,
    entry: DownloadsConfigEntry,
    backend: Backend,
) -> None:
    backend.responses[("GET", "/api/v1/search")] = (
        500,
        {
            "error_code": "PRIVATE-TOKEN",
            "message": "https://secret.invalid/private?token=PRIVATE-TOKEN",
        },
    )
    client = await hass_client(hass_user_token)
    count = len(backend.requests)
    response = await client.get(f"{PREFIX}/search?query=Example")
    assert response.status == 500
    assert "PRIVATE-TOKEN" not in await response.text()
    assert len(backend.requests) == count + 1


async def test_body_limits_and_redirects_fail_without_retry(
    hass_client: ClientSessionGenerator,
    hass_user_token: str,
    entry: DownloadsConfigEntry,
    backend: Backend,
) -> None:
    client = await hass_client(hass_user_token)
    count = len(backend.requests)
    oversized = b"x" * (MAX_REQUEST_BYTES + 1)
    assert (await client.post(f"{PREFIX}/requests", data=oversized)).status == 413
    assert len(backend.requests) == count
    backend.responses[("GET", "/api/v1/search")] = (302, {"message": "private"})
    backend.response_headers[("GET", "/api/v1/search")] = {
        "Location": backend.url + "/api/v1/system"
    }
    assert (await client.get(f"{PREFIX}/search?query=Example")).status == 502
    assert len(backend.requests) == count + 1
    backend.responses[("GET", "/api/v1/search")] = (
        200,
        {"items": ["x" * MAX_RESPONSE_BYTES]},
    )
    assert (await client.get(f"{PREFIX}/search?query=Example")).status == 502
    assert len(backend.requests) == count + 2
