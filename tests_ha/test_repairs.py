"""Publish failures through Core's real issue registry, without device actions."""

import asyncio
from copy import deepcopy
from datetime import timedelta

import pytest
from homeassistant.core import HomeAssistant
from homeassistant.data_entry_flow import FlowResultType
from homeassistant.helpers import entity_registry as er
from homeassistant.helpers import issue_registry as ir
from homeassistant.setup import async_setup_component
from homeassistant.util import dt as dt_util
from pytest_homeassistant.common import async_fire_time_changed
from pytest_homeassistant.test_util.aiohttp import AiohttpClientMocker

from custom_components.downloads import DownloadsConfigEntry
from custom_components.downloads.const import DOMAIN

from .conftest import LEGACY_HEALTH_URL, Backend, LegacySupervisor, health, legacy_health_record


async def test_protection_repair_requires_confirmed_recovery_and_entry_removal(
    hass: HomeAssistant,
    legacy_entry: DownloadsConfigEntry,
    aioclient_mock: AiohttpClientMocker,
) -> None:
    registry = ir.async_get(hass)
    issue_id = f"{legacy_entry.entry_id}_protection"
    assert registry.async_get_issue(DOMAIN, issue_id) is None

    aioclient_mock.clear_requests()
    failed = legacy_health_record(ready=False)
    failed["unexpected_secret"] = "private backend error"
    aioclient_mock.get(LEGACY_HEALTH_URL, json=failed)
    await legacy_entry.runtime_data.async_refresh()
    issue = registry.async_get_issue(DOMAIN, issue_id)
    assert issue is not None
    assert issue.config_entry_id == legacy_entry.entry_id
    assert issue.data == {"codes": "vpn_unavailable"}
    assert "private backend error" not in str(issue.translation_placeholders)
    assert issue.translation_placeholders["logs_url"].endswith("/testrepo_downloads/logs")

    aioclient_mock.clear_requests()
    aioclient_mock.get(LEGACY_HEALTH_URL, status=502)
    await legacy_entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, issue_id) is issue
    aioclient_mock.clear_requests()
    stale = legacy_health_record(ready=False)
    stale["reasons"] = ["checks_stale"]
    stale["age_ms"] = 31000
    aioclient_mock.get(LEGACY_HEALTH_URL, json=stale)
    await legacy_entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, issue_id) is issue

    aioclient_mock.clear_requests()
    aioclient_mock.get(LEGACY_HEALTH_URL, json=legacy_health_record())
    await legacy_entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, issue_id) is None
    aioclient_mock.clear_requests()
    aioclient_mock.get(LEGACY_HEALTH_URL, json=failed)
    await legacy_entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, issue_id) is not None
    assert await hass.config_entries.async_unload(legacy_entry.entry_id)
    assert registry.async_get_issue(DOMAIN, issue_id) is not None
    assert await hass.config_entries.async_remove(legacy_entry.entry_id)
    assert registry.async_get_issue(DOMAIN, issue_id) is None


async def test_stopped_app_is_confirmed_but_transport_loss_is_unknown(
    hass: HomeAssistant,
    legacy_entry: DownloadsConfigEntry,
    legacy_supervisor: LegacySupervisor,
    aioclient_mock: AiohttpClientMocker,
) -> None:
    registry = ir.async_get(hass)
    app_id = f"{legacy_entry.entry_id}_app"
    connection_id = f"{legacy_entry.entry_id}_protection_connection"
    legacy_supervisor.app.state = "stopped"
    await legacy_entry.runtime_data.async_refresh()
    issue = registry.async_get_issue(DOMAIN, app_id)
    assert issue is not None and issue.data == {"codes": "app_stopped"}
    legacy_supervisor.app.state = "started"
    aioclient_mock.clear_requests()
    aioclient_mock.get(LEGACY_HEALTH_URL, status=502)
    await legacy_entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, app_id) is issue
    unknown = registry.async_get_issue(DOMAIN, connection_id)
    assert unknown is not None and unknown.translation_key == "health_unavailable"
    aioclient_mock.clear_requests()
    aioclient_mock.get(LEGACY_HEALTH_URL, json=legacy_health_record())
    await legacy_entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, app_id) is None
    assert registry.async_get_issue(DOMAIN, connection_id) is None


async def test_media_authentication_uses_core_reauth_and_owned_cleanup(
    hass: HomeAssistant, entry: DownloadsConfigEntry, backend: Backend
) -> None:
    registry = ir.async_get(hass)
    connection_id = f"{entry.entry_id}_media_connection"
    backend.responses[("GET", "/api/v1/system")] = (503, {})
    await entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, connection_id) is not None
    backend.responses[("GET", "/api/v1/system")] = (401, {})
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    assert registry.async_get_issue(DOMAIN, connection_id) is None
    assert any(
        flow["context"].get("source") == "reauth"
        and flow["context"].get("entry_id") == entry.entry_id
        for flow in hass.config_entries.flow.async_progress()
    )
    ir.async_create_issue(
        hass,
        DOMAIN,
        "unrelated",
        is_fixable=False,
        severity=ir.IssueSeverity.WARNING,
        translation_key="test_only",
    )
    assert await hass.config_entries.async_remove(entry.entry_id)
    assert registry.async_get_issue(DOMAIN, "unrelated") is not None


async def test_media_repairs_preserve_failed_checks_even_with_warning_severity(
    hass: HomeAssistant, entry: DownloadsConfigEntry, backend: Backend
) -> None:
    registry = ir.async_get(hass)
    issue_id = f"{entry.entry_id}_component_sonarr"
    response = health()
    sonarr = next(item for item in response["components"] if item["component"] == "sonarr")
    assert registry.async_get_issue(DOMAIN, issue_id) is None
    sonarr["observation"]["outcome"] = {
        "state": "degraded",
        "issues": [{"code": "upstream_health", "severity": "warning", "message": "private"}],
    }
    backend.responses[("GET", "/api/v1/health/stack")] = (200, response)
    await entry.runtime_data.async_refresh()
    warning = registry.async_get_issue(DOMAIN, issue_id)
    assert warning is not None and warning.severity is ir.IssueSeverity.WARNING

    sonarr["observation"]["outcome"]["issues"].append(
        {"code": "download_client_unavailable", "severity": "error", "message": "private"}
    )
    await entry.runtime_data.async_refresh()
    issue = registry.async_get_issue(DOMAIN, issue_id)
    assert issue is not None
    assert issue.config_entry_id == entry.entry_id
    assert issue.data == {"codes": "download_client_unavailable, upstream_health"}
    assert "private" not in str(issue.translation_placeholders)

    backend.responses[("GET", "/api/v1/health/stack")] = (503, {})
    await entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, issue_id) is issue
    stale = deepcopy(response)
    stale["components"][1]["observation"]["freshness"] = "stale"
    backend.responses[("GET", "/api/v1/health/stack")] = (200, stale)
    await entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, issue_id) is issue
    # The producer omits routine release notices, so healthy means no service fault.
    sonarr["observation"]["outcome"] = {"state": "healthy"}
    backend.responses[("GET", "/api/v1/health/stack")] = (200, response)
    await entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, issue_id) is None


async def test_available_home_assistant_package_is_not_a_functional_repair(
    hass: HomeAssistant, entry: DownloadsConfigEntry, backend: Backend
) -> None:
    registry = ir.async_get(hass)
    issue_id = f"{entry.entry_id}_component_prowlarr"
    response = health()
    prowlarr = next(item for item in response["components"] if item["component"] == "prowlarr")
    notice = {"code": "update_available", "severity": "warning", "message": "Available in HA"}
    failure = {"code": "indexer_unavailable", "severity": "warning", "message": "Safe fixed text"}
    prowlarr["observation"]["outcome"] = {"state": "degraded", "issues": [notice]}
    backend.responses[("GET", "/api/v1/health/stack")] = (200, response)
    await entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, issue_id) is None
    prowlarr["observation"]["outcome"]["issues"].append(failure)
    await entry.runtime_data.async_refresh()
    issue = registry.async_get_issue(DOMAIN, issue_id)
    assert issue is not None and issue.data == {"codes": "indexer_unavailable"}
    prowlarr["observation"]["outcome"]["issues"] = [notice]
    await entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, issue_id) is None


@pytest.mark.parametrize("gap", ["missing", "stale", "future"])
async def test_incomplete_media_evidence_is_a_native_unknown_repair(
    hass: HomeAssistant, entry: DownloadsConfigEntry, backend: Backend, gap: str
) -> None:
    registry = ir.async_get(hass)
    issue_id = f"{entry.entry_id}_media_connection"
    response = health()
    if gap == "missing":
        response["components"].pop()
    elif gap == "stale":
        response["components"][0]["observation"]["freshness"] = "stale"
    else:
        response["generated_at_unix_ms"] += 120000
    backend.responses[("GET", "/api/v1/health/stack")] = (200, response)
    await entry.runtime_data.async_refresh()
    issue = registry.async_get_issue(DOMAIN, issue_id)
    assert issue is not None and issue.translation_key == "health_unavailable"
    assert issue.data == {"codes": "checks_stale"}
    assert issue.config_entry_id == entry.entry_id
    backend.responses[("GET", "/api/v1/health/stack")] = (200, health())
    await entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, issue_id) is None


@pytest.mark.parametrize("remove", [False, True])
async def test_late_refresh_cannot_change_repairs_after_its_owner_unloads(
    hass: HomeAssistant, entry: DownloadsConfigEntry, backend: Backend, remove: bool
) -> None:
    registry = ir.async_get(hass)
    issue_id = f"{entry.entry_id}_component_sonarr"
    fault = health()
    fault["components"][1]["observation"]["outcome"] = {
        "state": "unavailable",
        "reason": "connection",
    }
    coordinator = entry.runtime_data
    backend.responses[("GET", "/api/v1/health/stack")] = (200, fault)
    await coordinator.async_refresh()
    issue = registry.async_get_issue(DOMAIN, issue_id)
    assert issue is not None
    # A late recovery cannot clear an unloaded owner's unresolved issue. A late
    # failure cannot recreate an issue after the configuration entry is removed.
    backend.responses[("GET", "/api/v1/health/stack")] = (200, fault if remove else health())
    backend.delay_path = "/api/v1/health/stack"
    refresh = hass.async_create_background_task(coordinator.async_refresh(), "late health fixture")
    try:
        async with asyncio.timeout(2):
            await backend.received.wait()
            if remove:
                assert await hass.config_entries.async_remove(entry.entry_id)
            else:
                assert await hass.config_entries.async_unload(entry.entry_id)
    finally:
        backend.release.set()
        await refresh
    assert registry.async_get_issue(DOMAIN, issue_id) is (None if remove else issue)
    assert coordinator._cancel_expiry is None


async def test_intentionally_unconfigured_component_ends_only_its_repair(
    hass: HomeAssistant, entry: DownloadsConfigEntry, backend: Backend
) -> None:
    registry = ir.async_get(hass)
    response = health()
    for item in response["components"]:
        if item["component"] in {"sonarr", "prowlarr"}:
            item["observation"]["outcome"] = {"state": "unavailable", "reason": "connection"}
    backend.responses[("GET", "/api/v1/health/stack")] = (200, response)
    await entry.runtime_data.async_refresh()
    prowlarr_id = f"{entry.entry_id}_component_prowlarr"
    sonarr_id = f"{entry.entry_id}_component_sonarr"
    assert registry.async_get_issue(DOMAIN, prowlarr_id) is not None
    sonarr = registry.async_get_issue(DOMAIN, sonarr_id)
    assert sonarr is not None
    next(item for item in response["components"] if item["component"] == "prowlarr")["observation"][
        "outcome"
    ] = {"state": "not_configured"}
    await entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, prowlarr_id) is None
    assert registry.async_get_issue(DOMAIN, sonarr_id) is sonarr


async def test_native_protection_expiry_creates_an_unknown_repair_without_http(
    hass: HomeAssistant,
    legacy_entry: DownloadsConfigEntry,
    aioclient_mock: AiohttpClientMocker,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    coordinator = legacy_entry.runtime_data
    coordinator._async_unsub_refresh()
    calls = aioclient_mock.call_count
    observed = coordinator.data.received_at
    monkeypatch.setattr(
        "custom_components.downloads.native_health.monotonic", lambda: observed + 35
    )
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(seconds=35))
    await hass.async_block_till_done()
    registry = ir.async_get(hass)
    issue_id = f"{legacy_entry.entry_id}_protection_connection"
    issue = registry.async_get_issue(DOMAIN, issue_id)
    assert issue is not None and issue.translation_key == "health_unavailable"
    assert aioclient_mock.call_count == calls
    assert await hass.config_entries.async_unload(legacy_entry.entry_id)
    assert coordinator._cancel_expiry is None
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(seconds=70))
    await hass.async_block_till_done()
    assert registry.async_get_issue(DOMAIN, issue_id) is issue
    assert aioclient_mock.call_count == calls


async def test_media_expiry_retains_a_confirmed_fault_without_another_http_read(
    hass: HomeAssistant, entry: DownloadsConfigEntry, backend: Backend, freezer: object
) -> None:
    response = health(lifetime_ms=1000)
    response["components"][1]["observation"]["outcome"] = {
        "state": "unavailable",
        "reason": "connection",
    }
    backend.responses[("GET", "/api/v1/health/stack")] = (200, response)
    coordinator = entry.runtime_data
    await coordinator.async_refresh()
    coordinator._async_unsub_refresh()
    registry = ir.async_get(hass)
    fault_id = f"{entry.entry_id}_component_sonarr"
    fault = registry.async_get_issue(DOMAIN, fault_id)
    assert fault is not None
    calls = len(backend.requests)
    freezer.tick(timedelta(seconds=2))
    async_fire_time_changed(hass, dt_util.utcnow())
    await hass.async_block_till_done()
    unknown = registry.async_get_issue(DOMAIN, f"{entry.entry_id}_media_connection")
    assert unknown is not None and unknown.translation_key == "health_unavailable"
    assert registry.async_get_issue(DOMAIN, fault_id) is fault
    assert len(backend.requests) == calls
    assert await hass.config_entries.async_unload(entry.entry_id)
    assert coordinator._cancel_expiry is None


async def test_native_repairs_keep_polling_when_the_problem_entity_is_disabled(
    hass: HomeAssistant,
    legacy_entry: DownloadsConfigEntry,
    aioclient_mock: AiohttpClientMocker,
) -> None:
    er.async_get(hass).async_update_entity(
        "binary_sensor.downloads_problem", disabled_by=er.RegistryEntryDisabler.USER
    )
    await hass.async_block_till_done()
    aioclient_mock.clear_requests()
    aioclient_mock.get(LEGACY_HEALTH_URL, json=legacy_health_record(ready=False))
    async_fire_time_changed(hass, dt_util.utcnow() + timedelta(seconds=11))
    await hass.async_block_till_done()
    issue = ir.async_get(hass).async_get_issue(DOMAIN, f"{legacy_entry.entry_id}_protection")
    assert issue is not None and issue.data == {"codes": "vpn_unavailable"}
    assert hass.states.get("binary_sensor.downloads_problem") is None


async def test_native_version_repair_uses_fresh_owned_supervisor_observation(
    hass: HomeAssistant,
    legacy_entry: DownloadsConfigEntry,
    legacy_supervisor: LegacySupervisor,
    aioclient_mock: AiohttpClientMocker,
) -> None:
    registry = ir.async_get(hass)
    issue_id = f"{legacy_entry.entry_id}_version_mismatch"
    matching_version = legacy_supervisor.app.version
    legacy_supervisor.app.version = "9.8.7"
    await legacy_entry.runtime_data.async_refresh()

    issue = registry.async_get_issue(DOMAIN, issue_id)
    assert issue is not None and issue.is_fixable
    assert issue.config_entry_id == legacy_entry.entry_id
    assert issue.translation_placeholders == {
        "app_version": "9.8.7",
        "integration_version": matching_version,
    }
    assert await async_setup_component(hass, "repairs", {})
    flow = await hass.data["repairs"]["flow_manager"].async_init(
        DOMAIN, context={"issue_id": issue_id}
    )
    assert flow["type"] is FlowResultType.FORM
    assert flow["step_id"] == "retry"
    assert flow["errors"] == {"base": "managed_update_unavailable"}
    assert registry.async_get_issue(DOMAIN, issue_id) is issue

    legacy_supervisor.app.version = matching_version
    aioclient_mock.clear_requests()
    aioclient_mock.get(LEGACY_HEALTH_URL, status=502)
    await legacy_entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, issue_id) is issue

    aioclient_mock.clear_requests()
    aioclient_mock.get(LEGACY_HEALTH_URL, json=legacy_health_record())
    await legacy_entry.runtime_data.async_refresh()
    assert registry.async_get_issue(DOMAIN, issue_id) is None
