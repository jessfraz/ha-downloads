"""One health summary, not a recorder entity for every movie or episode."""

from homeassistant.components.sensor import SensorDeviceClass, SensorEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.entity_platform import AddConfigEntryEntitiesCallback
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .coordinator import DownloadsCoordinator, now_ms
from .entity import device_info
from .models import HealthState


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry[DownloadsCoordinator],
    async_add_entities: AddConfigEntryEntitiesCallback,
) -> None:
    async_add_entities([DownloadsHealth(entry.runtime_data, entry)])


class DownloadsHealth(CoordinatorEntity[DownloadsCoordinator], SensorEntity):
    _attr_has_entity_name = True
    _attr_translation_key = "stack_health"
    _attr_device_class = SensorDeviceClass.ENUM
    _attr_options = [item.value for item in HealthState]
    _attr_icon = "mdi:download-network-outline"

    def __init__(
        self,
        coordinator: DownloadsCoordinator,
        entry: ConfigEntry[DownloadsCoordinator],
    ) -> None:
        super().__init__(coordinator)
        self._attr_unique_id = f"{entry.entry_id}_stack_health"
        self._attr_device_info = device_info(entry)
        self._update_attributes()

    @callback
    def _update_attributes(self) -> None:
        snapshot = self.coordinator.data
        now = now_ms()
        self._attr_native_value = snapshot.stack_state_at(now).value
        self._attr_extra_state_attributes = {
            "components": {
                item.component.value: item.state_at(now).value for item in snapshot.components
            },
            "observed_at_unix_ms": snapshot.generated_at,
        }

    @callback
    def _handle_coordinator_update(self) -> None:
        self._update_attributes()
        super()._handle_coordinator_update()
