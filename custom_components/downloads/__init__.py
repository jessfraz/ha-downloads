"""Expose the app's safe functional health through native HA entities."""

from homeassistant.config_entries import ConfigEntry
from homeassistant.const import Platform
from homeassistant.core import HomeAssistant

from .coordinator import DownloadsCoordinator

type DownloadsConfigEntry = ConfigEntry[DownloadsCoordinator]
PLATFORMS = [Platform.BINARY_SENSOR]


async def async_setup_entry(hass: HomeAssistant, entry: DownloadsConfigEntry) -> bool:
    """Use HA's existing Supervisor client and shared HTTP session."""
    coordinator = DownloadsCoordinator(hass, entry)
    await coordinator.async_config_entry_first_refresh()
    entry.runtime_data = coordinator
    await hass.config_entries.async_forward_entry_setups(entry, PLATFORMS)
    return True


async def async_unload_entry(hass: HomeAssistant, entry: DownloadsConfigEntry) -> bool:
    """Unload polling and entities without closing HA's shared session."""
    return await hass.config_entries.async_unload_platforms(entry, PLATFORMS)
