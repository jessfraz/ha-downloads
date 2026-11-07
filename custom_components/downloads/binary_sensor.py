"""A problem sensor based on functional readiness, never queue activity."""

from datetime import datetime, timedelta

from homeassistant.components.binary_sensor import (
    BinarySensorDeviceClass,
    BinarySensorEntity,
)
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.device_registry import DeviceInfo
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.event import async_track_time_interval
from homeassistant.helpers.update_coordinator import CoordinatorEntity

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
    _unrecorded_attributes = frozenset({"checked_at_unix_ms", "age_ms"})

    def __init__(self, entry: DownloadsConfigEntry) -> None:
        super().__init__(entry.runtime_data)
        self._attr_unique_id = f"{entry.unique_id}_problem"
        self._attr_device_info = DeviceInfo(
            identifiers={(DOMAIN, entry.data[CONF_ADDON_SLUG])},
            name="Downloads",
            manufacturer="Downloads",
            configuration_url=f"homeassistant://hassio/ingress/{entry.data['addon_slug']}",
        )

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
        self._update_health()
        # Expire a frozen snapshot even when the coordinator stops publishing.
        self.async_on_remove(
            async_track_time_interval(
                self.hass, self._async_check_freshness, timedelta(seconds=10)
            )
        )

    @callback
    def _update_health(self) -> None:
        attributes = self.coordinator.data.projection()
        self._attr_is_on = attributes["ready"] is not True
        self._attr_extra_state_attributes = attributes

    @callback
    def _handle_coordinator_update(self) -> None:
        self._update_health()
        super()._handle_coordinator_update()

    @callback
    def _async_check_freshness(self, _now: datetime) -> None:
        self._handle_coordinator_update()
