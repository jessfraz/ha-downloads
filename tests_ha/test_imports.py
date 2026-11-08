"""Real native HA notification, storage and HTTP observation paths."""

from datetime import timedelta
from unittest.mock import patch

import pytest
from homeassistant.components import persistent_notification
from homeassistant.components.homeassistant.exposed_entities import async_expose_entity
from homeassistant.core import Context, HomeAssistant, callback
from homeassistant.helpers import llm
from homeassistant.setup import async_setup_component
from homeassistant.util import dt as dt_util
from homeassistant.util.file import WriteError
from pytest_homeassistant.common import MockUser, async_capture_events
from pytest_homeassistant.typing import ClientSessionGenerator

from custom_components.downloads import DownloadsConfigEntry
from custom_components.downloads.access import gateway_entity_id
from custom_components.downloads.coordinator import DownloadsCoordinator
from custom_components.downloads.imports import ImportNotifications

from .conftest import Backend


def imported(manager: str, native_id: int, *, days_ago: int = 0) -> dict[str, object]:
    when = dt_util.utcnow() - timedelta(days=days_ago)
    return {
        "id": f"{manager}:{native_id}",
        "manager": manager,
        "media": {"kind": "movie", "tmdb_id": 14337}
        if manager == "radarr"
        else {"kind": "series", "tvdb_id": 10},
        "title": "Example import",
        "episode": None,
        "imported_at": when.isoformat().replace("+00:00", "Z"),
        "quality": None,
        "plex": {"state": "unavailable", "reason": "authentication"},
        "subtitles": {"state": "unknown", "reason": "plex_unverified"},
        "observed_at_unix_ms": int(dt_util.utcnow().timestamp() * 1000),
    }


def page(items: list[dict[str, object]]) -> dict[str, object]:
    return {
        "items": items,
        "next_page": None,
        "coverage": [
            {"source": source, "state": "available", "observed_at_unix_ms": 100}
            for source in ("radarr", "sonarr")
        ],
    }


@pytest.fixture
async def observer(entry: DownloadsConfigEntry) -> ImportNotifications:
    assert isinstance(entry.runtime_data, DownloadsCoordinator)
    assert entry.runtime_data.import_notifications is not None
    await entry.runtime_data.import_notifications.async_request_refresh()
    return entry.runtime_data.import_notifications


@pytest.fixture
def notices(hass: HomeAssistant) -> list[dict[str, persistent_notification.Notification]]:
    result = []

    @callback
    def capture(
        _kind: persistent_notification.UpdateType,
        notifications: dict[str, persistent_notification.Notification],
    ) -> None:
        result.append(notifications)

    remove = persistent_notification.async_register_callback(hass, capture)
    hass.bus.async_listen_once("homeassistant_stop", lambda _: remove())
    return result


async def test_native_imports_notify_once_without_a_downloads_receipt_and_survive_reload(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    backend: Backend,
    observer: ImportNotifications,
    notices: list[dict[str, persistent_notification.Notification]],
) -> None:
    events = async_capture_events(hass, "downloads_media_imported")
    old = imported("radarr", 1, days_ago=1)
    backend.responses[("GET", "/api/v1/imports")] = (200, page([old]))
    await observer.async_refresh()
    assert not notices
    movie, episode = imported("radarr", 2), imported("sonarr", 3)
    episode["episode"] = {"season": 2, "number": 4}
    backend.responses[("GET", "/api/v1/imports")] = (200, page([episode, movie, old]))
    await observer.async_refresh()
    await hass.async_block_till_done()
    assert len(notices) == len(events) == 2
    assert {event.data["import_id"] for event in events} == {"radarr:2", "sonarr:3"}
    assert all("not yet verified" in next(iter(item.values()))["message"] for item in notices)
    assert all(not event.data["plex_verified"] for event in events)
    await observer.async_refresh()
    assert len(notices) == 2
    # Reconstruct the real Store consumer as entry reload/Core restart would.
    replacement = ImportNotifications(hass, entry, observer.client)
    await replacement.async_refresh()
    assert replacement.last_update_success
    assert len(notices) == 2
    await replacement.async_shutdown()
    assert not any(read.method != "GET" for read in backend.requests)


async def test_playback_and_subtitle_proof_update_the_same_notice_without_repeating_import(
    hass: HomeAssistant,
    backend: Backend,
    observer: ImportNotifications,
    notices: list[dict[str, persistent_notification.Notification]],
) -> None:
    imports = async_capture_events(hass, "downloads_media_imported")
    ready = async_capture_events(hass, "downloads_media_ready")
    item = imported("radarr", 4)
    backend.responses[("GET", "/api/v1/imports")] = (200, page([item]))
    await observer.async_refresh()
    when = item["observed_at_unix_ms"]
    item["plex"] = {"state": "verified", "observed_at_unix_ms": when}
    item["subtitles"] = {"state": "missing", "observed_at_unix_ms": when}
    await observer.async_refresh()
    await hass.async_block_till_done()
    assert len(notices) == 2
    assert not ready
    assert "none reported" in next(iter(notices[-1].values()))["message"]
    item["subtitles"] = {"state": "verified", "languages": ["English"], "observed_at_unix_ms": when}
    await observer.async_refresh()
    await observer.async_refresh()
    await hass.async_block_till_done()
    assert len(notices) == 3
    assert len(imports) == len(ready) == 1
    assert len({key for change in notices for key in change}) == 1
    assert "English" in next(iter(notices[-1].values()))["message"]


async def test_source_gaps_and_malformed_evidence_do_not_advance_completion_state(
    hass: HomeAssistant,
    backend: Backend,
    observer: ImportNotifications,
    notices: list[dict[str, persistent_notification.Notification]],
) -> None:
    item = imported("sonarr", 5)
    backend.responses[("GET", "/api/v1/imports")] = (503, {})
    await observer.async_refresh()
    assert not observer.last_update_success
    assert not notices
    invalid = {
        **item,
        "subtitles": {"state": "verified", "languages": ["English"], "observed_at_unix_ms": 100},
    }
    backend.responses[("GET", "/api/v1/imports")] = (200, page([invalid]))
    await observer.async_refresh()
    assert not observer.last_update_success
    assert not notices
    item["title"] = "[Example](https://example.invalid) <script>"
    backend.responses[("GET", "/api/v1/imports")] = (200, page([item]))
    await observer.async_refresh()
    await hass.async_block_till_done()
    assert observer.last_update_success
    assert len(notices) == 1
    message = next(iter(notices[0].values()))["message"]
    assert "[Example](" not in message
    assert "<script>" not in message


async def test_import_history_bridge_is_read_only_and_rejects_configuration_arguments(
    hass_client: ClientSessionGenerator,
    hass_user_token: str,
    entry: DownloadsConfigEntry,
    backend: Backend,
) -> None:
    body = page([imported("radarr", 6)])
    backend.responses[("GET", "/api/v1/imports")] = (200, body)
    client = await hass_client(hass_user_token)
    response = await client.get("/api/downloads/v1/imports?limit=20")
    assert response.status == 200
    assert await response.json() == body
    count = len(backend.requests)
    assert (await client.post("/api/downloads/v1/imports", json={})).status == 405
    assert (await client.get("/api/downloads/v1/imports?quality_profile_id=1")).status == 400
    assert len(backend.requests) == count


async def test_out_of_order_native_imports_and_changed_presentation_do_not_lose_or_repeat_events(
    hass: HomeAssistant,
    backend: Backend,
    observer: ImportNotifications,
    notices: list[dict[str, persistent_notification.Notification]],
) -> None:
    imports = async_capture_events(hass, "downloads_media_imported")
    ready = async_capture_events(hass, "downloads_media_ready")
    later = imported("radarr", 20)
    earlier = imported("radarr", 21)
    for item, seconds in [(later, 20), (earlier, 10)]:
        item["imported_at"] = (
            (dt_util.utcnow() + timedelta(seconds=seconds)).isoformat().replace("+00:00", "Z")
        )
    backend.responses[("GET", "/api/v1/imports")] = (200, page([later]))
    await observer.async_refresh()
    backend.responses[("GET", "/api/v1/imports")] = (200, page([later, earlier]))
    await observer.async_refresh()
    await hass.async_block_till_done()
    assert {event.data["import_id"] for event in imports} == {"radarr:20", "radarr:21"}
    when = later["observed_at_unix_ms"]
    later["plex"] = {"state": "verified", "observed_at_unix_ms": when}
    later["subtitles"] = {
        "state": "verified",
        "languages": ["English", "French"],
        "observed_at_unix_ms": when,
    }
    await observer.async_refresh()
    await hass.async_block_till_done()
    count = len(notices)
    notice_id = next(iter(notices[-1]))
    persistent_notification.async_dismiss(hass, notice_id)
    later["title"] = "Corrected title"
    later["subtitles"] = {
        "state": "verified",
        "languages": ["French", "English"],
        "observed_at_unix_ms": when,
    }
    await observer.async_refresh()
    await hass.async_block_till_done()
    assert len(ready) == 1
    assert len(notices) == count + 1  # Only the user's dismissal, no recreated notice.


async def test_native_store_write_failure_is_detected_and_retried_without_duplicate_notices(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    backend: Backend,
    observer: ImportNotifications,
    notices: list[dict[str, persistent_notification.Notification]],
) -> None:
    item = imported("sonarr", 30)
    backend.responses[("GET", "/api/v1/imports")] = (200, page([item]))
    with patch(
        "homeassistant.helpers.storage.Store._async_write_data",
        side_effect=WriteError("disk unavailable"),
    ):
        await observer.async_refresh()
        assert not observer.last_update_success
        assert len(notices) == 1
        await observer.async_refresh()
        assert not observer.last_update_success
        assert len(notices) == 1
    await observer.async_refresh()
    assert observer.last_update_success
    replacement = ImportNotifications(hass, entry, observer.client)
    await replacement.async_refresh()
    assert replacement.last_update_success
    assert len(notices) == 1
    await replacement.async_shutdown()


async def test_native_assist_and_action_read_imports_with_authenticated_actor_and_exposure(
    hass: HomeAssistant,
    hass_user: MockUser,
    entry: DownloadsConfigEntry,
    backend: Backend,
) -> None:
    body = page([imported("radarr", 40)])
    backend.responses[("GET", "/api/v1/imports")] = (200, body)
    entity_id = gateway_entity_id(hass, entry)
    assert entity_id is not None
    async_expose_entity(hass, "conversation", entity_id, True)
    assert await async_setup_component(hass, "llm", {})
    context = Context(user_id=hass_user.id)
    instance = await llm.async_get_api(
        hass,
        "assist",
        llm.LLMContext(
            platform="test",
            context=context,
            language="en",
            assistant="conversation",
            device_id=None,
        ),
    )
    tool_input = llm.ToolInput(tool_name="downloads__imports", tool_args={"limit": 5})
    result = await instance.async_call_tool(tool_input)
    assert not result.error
    assert result.data == body
    assert backend.requests[-1].headers["X-Downloads-Actor-Id"] == hass_user.id
    action = await hass.services.async_call(
        "downloads",
        "imports",
        {"limit": 5},
        context=context,
        blocking=True,
        return_response=True,
    )
    assert action == body
    count = len(backend.requests)
    async_expose_entity(hass, "conversation", entity_id, False)
    result = await instance.async_call_tool(tool_input)
    assert result.error and result.data["error"] == "not_exposed"
    assert len(backend.requests) == count


async def test_unavailable_cached_source_does_not_block_another_managers_new_import(
    hass: HomeAssistant,
    backend: Backend,
    observer: ImportNotifications,
    notices: list[dict[str, persistent_notification.Notification]],
) -> None:
    events = async_capture_events(hass, "downloads_media_imported")
    cached = imported("sonarr", 51)
    healthy = imported("radarr", 52)
    partial = page([cached, healthy])
    partial["coverage"] = [
        {"source": "radarr", "state": "available", "observed_at_unix_ms": 100},
        {"source": "sonarr", "state": "unavailable", "observed_at_unix_ms": 50},
    ]
    backend.responses[("GET", "/api/v1/imports")] = (200, partial)
    await observer.async_refresh()
    await hass.async_block_till_done()
    assert observer.last_update_success
    assert len(notices) == 1
    assert [event.data["import_id"] for event in events] == ["radarr:52"]
    backend.responses[("GET", "/api/v1/imports")] = (200, page([cached, healthy]))
    await observer.async_refresh()
    await hass.async_block_till_done()
    assert len(notices) == 2
    assert {event.data["import_id"] for event in events} == {"radarr:52", "sonarr:51"}
