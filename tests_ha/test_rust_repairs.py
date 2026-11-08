"""Verify the real Rust health producer through HA's native issue registry."""

import os

import pytest
from homeassistant.core import HomeAssistant
from homeassistant.helpers import issue_registry as ir
from pytest_homeassistant.common import MockConfigEntry

from custom_components.downloads.const import CONF_BACKEND_URL, CONF_TOKEN, DOMAIN


async def test_rust_manager_health_drives_native_repairs(
    hass: HomeAssistant, socket_enabled: None
) -> None:
    if not (url := os.environ.get("DOWNLOADS_INTEROP_URL")):
        pytest.skip("Run through the real_home_assistant_repairs Rust test")
    entry = MockConfigEntry(
        domain=DOMAIN,
        data={CONF_BACKEND_URL: url, CONF_TOKEN: os.environ["DOWNLOADS_INTEROP_TOKEN"]},
    )
    entry.add_to_hass(hass)
    assert await hass.config_entries.async_setup(entry.entry_id)
    await hass.async_block_till_done()
    registry = ir.async_get(hass)
    issue = registry.async_get_issue(DOMAIN, f"{entry.entry_id}_component_prowlarr")
    if os.environ["DOWNLOADS_INTEROP_HEALTHY"] == "true":
        assert issue is None, "a routine release notice is not a service Repair"
    else:
        assert issue is not None, "a failed update check remains actionable"
        assert issue.config_entry_id == entry.entry_id
        assert issue.data == {"codes": os.environ["DOWNLOADS_INTEROP_CODE"]}
        assert issue.severity in {ir.IssueSeverity.WARNING, ir.IssueSeverity.ERROR}
        assert "Unable to check" not in str(issue.translation_placeholders)
    assert await hass.config_entries.async_remove(entry.entry_id)
    assert registry.async_get_issue(DOMAIN, f"{entry.entry_id}_component_prowlarr") is None
