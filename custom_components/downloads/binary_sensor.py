"""A problem sensor based on functional readiness, never queue activity."""

from homeassistant.components.binary_sensor import (
    BinarySensorDeviceClass,
    BinarySensorEntity,
)
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant
from homeassistant.helpers.device_registry import DeviceInfo
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.update_coordinator import CoordinatorEntity
from homeassistant.util.json import JsonObjectType

from . import DownloadsConfigEntry
from .const import CONF_ADDON_SLUG, DOMAIN
from .coordinator import DownloadsCoordinator


async def async_setup_entry(
    hass: HomeAssistant,
    entry: DownloadsConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Create one canonical health signal for the selected app."""
    async_add_entities([DownloadsProblem(entry)])


class DownloadsProblem(CoordinatorEntity[DownloadsCoordinator], BinarySensorEntity):
    """Unavailable polling stays unavailable rather than becoming healthy."""

    _attr_has_entity_name = True
    _attr_translation_key = "problem"
    _attr_device_class = BinarySensorDeviceClass.PROBLEM
    _attr_entity_category = EntityCategory.DIAGNOSTIC

    def __init__(self, entry: DownloadsConfigEntry) -> None:
        super().__init__(entry.runtime_data)
        self._attr_unique_id = f"{entry.unique_id}_problem"
        self._attr_device_info = DeviceInfo(
            identifiers={(DOMAIN, entry.data[CONF_ADDON_SLUG])},
            name="Downloads",
            manufacturer="Downloads",
            configuration_url=f"homeassistant://hassio/ingress/{entry.data['addon_slug']}",
        )

    @property
    def is_on(self) -> bool:
        return self.coordinator.data.projection()["ready"] is not True

    @property
    def extra_state_attributes(self) -> JsonObjectType:
        return self.coordinator.data.projection()
