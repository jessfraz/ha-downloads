"""Native protection health, with separately paired media capabilities."""

from homeassistant.config_entries import ConfigEntry
from homeassistant.const import Platform
from homeassistant.core import HomeAssistant
from homeassistant.exceptions import ConfigEntryNotReady
from homeassistant.helpers import config_validation as cv
from homeassistant.helpers.aiohttp_client import async_get_clientsession
from homeassistant.helpers.typing import ConfigType

from ._app_kit.repairs import VersionRepairs, async_check_startup_version
from .client import DownloadsClient
from .const import CONF_ADDON_SLUG, CONF_BACKEND_URL, CONF_TOKEN, DOMAIN
from .coordinator import DownloadsCoordinator
from .http import DownloadsHealthView, DownloadsView
from .imports import async_setup_import_notifications
from .native_health import NativeHealthCoordinator
from .repairs import VERSION_REPAIR, async_remove
from .services import async_register_services

type DownloadsConfigEntry = ConfigEntry[DownloadsCoordinator | NativeHealthCoordinator]
CONFIG_SCHEMA = cv.config_entry_only_config_schema(DOMAIN)
MEDIA_PLATFORMS = [Platform.SENSOR, Platform.BINARY_SENSOR]
HEALTH_PLATFORMS = [Platform.BINARY_SENSOR]


async def async_setup(hass: HomeAssistant, config: ConfigType) -> bool:
    """Keep the installed health surface and add the scoped media bridge."""
    hass.http.register_view(DownloadsHealthView(hass))
    hass.http.register_view(DownloadsView(hass))
    async_register_services(hass)
    return True


async def async_migrate_entry(hass: HomeAssistant, entry: DownloadsConfigEntry) -> bool:
    """Retain slug identity, data and permissions of installed health-only entries."""
    if entry.version > 2:
        return False
    if entry.version == 1:
        slug = entry.data.get(CONF_ADDON_SLUG)
        url = entry.data.get(CONF_BACKEND_URL)
        token = entry.data.get(CONF_TOKEN)
        has_media = isinstance(url, str) and bool(url) and isinstance(token, str) and bool(token)
        has_health = isinstance(slug, str) and bool(slug)
        if not has_media and not has_health:
            return False
        if (CONF_BACKEND_URL in entry.data or CONF_TOKEN in entry.data) and not has_media:
            return False
        hass.config_entries.async_update_entry(entry, version=2)
    return True


async def async_setup_entry(hass: HomeAssistant, entry: DownloadsConfigEntry) -> bool:
    """Native health remains usable without a media token or provider access."""
    native_health = None
    try:
        if entry.data.get(CONF_ADDON_SLUG):
            native_health = NativeHealthCoordinator(hass, entry)
            await native_health.async_config_entry_first_refresh()
        if CONF_BACKEND_URL in entry.data and CONF_TOKEN in entry.data:
            client = DownloadsClient(
                async_get_clientsession(hass),
                entry.data[CONF_BACKEND_URL],
                entry.data[CONF_TOKEN],
            )
            coordinator = DownloadsCoordinator(hass, entry, client, native_health)
            await coordinator.async_config_entry_first_refresh()
            entry.runtime_data = coordinator
            platforms = MEDIA_PLATFORMS
        else:
            if native_health is None:
                return False
            entry.runtime_data = native_health
            platforms = HEALTH_PLATFORMS
    except ConfigEntryNotReady:
        await async_check_startup_version(hass, entry, VERSION_REPAIR)
        raise
    version_repairs = VersionRepairs(hass, entry, VERSION_REPAIR)
    entry.runtime_data.version_repairs = version_repairs
    version_repairs.async_start()
    await hass.config_entries.async_forward_entry_setups(entry, platforms)
    # Repairs are entry-owned coordinator consumers, independent of entity
    # enablement. Their listeners also receive the existing local expiry events.
    coordinators = [entry.runtime_data]
    if isinstance(entry.runtime_data, DownloadsCoordinator) and native_health is not None:
        coordinators.append(native_health)
    for health in coordinators:
        entry.async_on_unload(health.async_add_listener(health.async_reconcile_repairs))
        health.async_reconcile_repairs()
    if isinstance(entry.runtime_data, DownloadsCoordinator):
        entry.runtime_data.import_notifications = await async_setup_import_notifications(
            hass, entry, client
        )
    entry.async_on_unload(entry.add_update_listener(async_reload_entry))
    return True


async def async_reload_entry(hass: HomeAssistant, entry: DownloadsConfigEntry) -> None:
    await hass.config_entries.async_reload(entry.entry_id)


async def async_unload_entry(hass: HomeAssistant, entry: DownloadsConfigEntry) -> bool:
    """Shared HTTP sessions remain Core-owned; platform timers unload with their entities."""
    platforms = (
        MEDIA_PLATFORMS
        if isinstance(entry.runtime_data, DownloadsCoordinator)
        else HEALTH_PLATFORMS
    )
    if not await hass.config_entries.async_unload_platforms(entry, platforms):
        return False
    if isinstance(entry.runtime_data, DownloadsCoordinator):
        entry.runtime_data.async_cancel_expiry()
    return True


async def async_remove_entry(hass: HomeAssistant, entry: DownloadsConfigEntry) -> None:
    """Remove only the Repairs owned by this configuration entry."""
    async_remove(hass, entry)
