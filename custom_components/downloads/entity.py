"""Stable installation identity shared by the summary platforms."""

from homeassistant.config_entries import ConfigEntry
from homeassistant.helpers.entity import DeviceInfo

from .const import CONF_ADDON_SLUG, CONF_BACKEND_URL, DOMAIN


def device_info(entry: ConfigEntry) -> DeviceInfo:
    return DeviceInfo(
        identifiers={(DOMAIN, entry.data.get(CONF_ADDON_SLUG) or entry.entry_id)},
        name="Downloads",
        manufacturer="Downloads",
        model="Media requests and stack health",
        configuration_url=(
            f"homeassistant://hassio/ingress/{entry.data[CONF_ADDON_SLUG]}"
            if entry.data.get(CONF_ADDON_SLUG)
            else entry.data[CONF_BACKEND_URL]
        ),
    )
