"""Backend-owned operation readiness, with entry-owned evidence expiration."""

from datetime import datetime, timedelta

from homeassistant.components.binary_sensor import (
    BinarySensorDeviceClass,
    BinarySensorEntity,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.entity_platform import AddConfigEntryEntitiesCallback
from homeassistant.helpers.event import async_track_time_interval
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .coordinator import DownloadsCoordinator, now_ms
from .entity import device_info
from .health import native_health_coordinator
from .models import Operation
from .native_health import NativeHealthCoordinator


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry[DownloadsCoordinator],
    async_add_entities: AddConfigEntryEntitiesCallback,
) -> None:
    coordinator = native_health_coordinator(entry)
    entities = [DownloadsProblem(entry, coordinator)] if coordinator is not None else []
    if isinstance(entry.runtime_data, DownloadsCoordinator):
        entities.extend(
            DownloadsReadiness(entry.runtime_data, entry, operation) for operation in Operation
        )
    async_add_entities(entities)


class DownloadsReadiness(CoordinatorEntity[DownloadsCoordinator], BinarySensorEntity):
    _attr_has_entity_name = True

    def __init__(
        self,
        coordinator: DownloadsCoordinator,
        entry: ConfigEntry[DownloadsCoordinator],
        operation: Operation,
    ) -> None:
        super().__init__(coordinator)
        self._operation = operation
        self._attr_unique_id = f"{entry.entry_id}_{operation.value}_ready"
        self._attr_translation_key = f"{operation.value}_ready"
        self._attr_device_info = device_info(entry)
        self._update_attributes()

    @callback
    def _update_attributes(self) -> None:
        self._attr_is_on = self.coordinator.data.ready_at(self._operation, now_ms())

    @callback
    def _handle_coordinator_update(self) -> None:
        self._update_attributes()
        super()._handle_coordinator_update()


class DownloadsProblem(CoordinatorEntity[NativeHealthCoordinator], BinarySensorEntity):
    """Unavailable polling stays unavailable rather than becoming healthy."""

    _attr_has_entity_name = True
    _attr_translation_key = "problem"
    _attr_device_class = BinarySensorDeviceClass.PROBLEM
    _attr_entity_category = EntityCategory.DIAGNOSTIC
    _unrecorded_attributes = frozenset({"checked_at_unix_ms", "age_ms"})

    def __init__(self, entry: ConfigEntry, coordinator: NativeHealthCoordinator) -> None:
        super().__init__(coordinator)
        self._attr_unique_id = f"{entry.unique_id}_problem"
        self._attr_device_info = device_info(entry)

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
        self._update_health()
        # Expire a frozen snapshot even when the coordinator stops publishing.
        self.async_on_remove(
            async_track_time_interval(self.hass, self._async_check_freshness, timedelta(seconds=10))
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
