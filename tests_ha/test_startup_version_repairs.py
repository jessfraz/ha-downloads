"""Failed media setup keeps the native version Repair usable."""

from homeassistant.config_entries import ConfigEntryState
from homeassistant.core import HomeAssistant
from homeassistant.helpers import issue_registry as ir
from homeassistant.setup import async_setup_component
from pytest_homeassistant.common import MockConfigEntry
from pytest_homeassistant.test_util.aiohttp import AiohttpClientMocker
from pytest_homeassistant.typing import ClientSessionGenerator

from custom_components.downloads._app_kit.version import LOADED_VERSION
from custom_components.downloads.const import (
    CONF_ADDON_SLUG,
    CONF_BACKEND_URL,
    CONF_TOKEN,
    DOMAIN,
)

from .conftest import (
    LEGACY_APP_SLUG,
    LEGACY_HEALTH_URL,
    Backend,
    LegacySupervisor,
    legacy_health_record,
)


async def test_failed_startup_version_repair_opens_in_native_repairs(
    hass: HomeAssistant, backend: Backend, hass_client: ClientSessionGenerator
) -> None:
    system = {
        "version": "0.0.1",
        "credential_role": "integration",
        "capabilities": {"read": True, "request": False, "admin": False},
        "transport": "native",
    }
    backend.responses[("GET", "/api/v1/system")] = (200, system)
    backend.responses[("GET", "/api/v1/health/stack")] = (404, {})
    entry = MockConfigEntry(
        domain=DOMAIN,
        data={CONF_BACKEND_URL: backend.url, CONF_TOKEN: "test-integration-token"},
    )
    entry.add_to_hass(hass)
    assert not await hass.config_entries.async_setup(entry.entry_id)
    assert entry.state is ConfigEntryState.SETUP_RETRY
    issue_id = f"{entry.entry_id}_version_mismatch"
    issue = ir.async_get(hass).async_get_issue(DOMAIN, issue_id)
    assert issue is not None and issue.is_fixable and issue.is_persistent
    assert issue.config_entry_id == entry.entry_id

    assert await async_setup_component(hass, "repairs", {})
    client = await hass_client()
    response = await client.post(
        "/api/repairs/issues/fix", json={"handler": DOMAIN, "issue_id": issue_id}
    )
    assert response.status == 200
    flow = await response.json()
    assert flow["type"] == "form" and flow["step_id"] == "retry"
    assert flow["errors"] == {"base": "managed_update_unavailable"}
    assert ir.async_get(hass).async_get_issue(DOMAIN, issue_id) is issue
    assert all(request.method == "GET" for request in backend.requests)

    backend.responses[("GET", "/api/v1/system")] = (
        200,
        system | {"version": LOADED_VERSION},
    )
    del backend.responses[("GET", "/api/v1/health/stack")]
    assert await hass.config_entries.async_reload(entry.entry_id)
    await hass.async_block_till_done(wait_background_tasks=True)
    assert entry.state is ConfigEntryState.LOADED
    assert ir.async_get(hass).async_get_issue(DOMAIN, issue_id) is None


async def test_failed_native_health_setup_keeps_version_repair_usable(
    hass: HomeAssistant,
    legacy_supervisor: LegacySupervisor,
    aioclient_mock: AiohttpClientMocker,
    hass_client: ClientSessionGenerator,
) -> None:
    legacy_supervisor.app.version = "0.0.1"
    aioclient_mock.get(LEGACY_HEALTH_URL, status=404)
    entry = MockConfigEntry(
        domain=DOMAIN,
        unique_id=LEGACY_APP_SLUG,
        data={CONF_ADDON_SLUG: LEGACY_APP_SLUG},
    )
    entry.add_to_hass(hass)
    assert not await hass.config_entries.async_setup(entry.entry_id)
    assert entry.state is ConfigEntryState.SETUP_RETRY
    issue_id = f"{entry.entry_id}_version_mismatch"
    issue = ir.async_get(hass).async_get_issue(DOMAIN, issue_id)
    assert issue is not None and issue.is_fixable and issue.is_persistent
    assert issue.config_entry_id == entry.entry_id

    assert await async_setup_component(hass, "repairs", {})
    client = await hass_client()
    response = await client.post(
        "/api/repairs/issues/fix", json={"handler": DOMAIN, "issue_id": issue_id}
    )
    assert response.status == 200
    flow = await response.json()
    assert flow["type"] == "form" and flow["step_id"] == "retry"
    assert flow["errors"] == {"base": "managed_update_unavailable"}
    assert ir.async_get(hass).async_get_issue(DOMAIN, issue_id) is issue
    legacy_supervisor.app.version = LOADED_VERSION
    aioclient_mock.clear_requests()
    aioclient_mock.get(LEGACY_HEALTH_URL, json=legacy_health_record())
    assert await hass.config_entries.async_reload(entry.entry_id)
    await hass.async_block_till_done(wait_background_tasks=True)
    assert entry.state is ConfigEntryState.LOADED
    assert ir.async_get(hass).async_get_issue(DOMAIN, issue_id) is None
