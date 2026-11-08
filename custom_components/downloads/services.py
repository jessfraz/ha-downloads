"""Native HA actions use the same fixed API and authorization as Assist."""

from homeassistant.core import (
    HomeAssistant,
    ServiceCall,
    ServiceResponse,
    SupportsResponse,
)
from homeassistant.exceptions import ServiceValidationError

from .access import current_guard
from .const import DOMAIN
from .operations import (
    IMPORTS_SCHEMA,
    REQUEST_SCHEMA,
    SEARCH_SCHEMA,
    STATUS_SCHEMA,
    async_imports,
    async_request,
    async_search,
    async_status,
)


def async_register_services(hass: HomeAssistant) -> None:
    async def handle(call: ServiceCall) -> ServiceResponse:
        guard = current_guard(hass, call.context, request=call.service == "request")
        try:
            if call.service == "search":
                return await async_search(guard, call.data)
            if call.service == "request":
                return await async_request(guard, call.data)
            if call.service == "imports":
                return await async_imports(guard, call.data)
            return await async_status(guard, call.data)
        except ValueError:
            raise ServiceValidationError("Use the declared Downloads action fields.") from None

    for name, schema in (
        ("search", SEARCH_SCHEMA),
        ("request", REQUEST_SCHEMA),
        ("status", STATUS_SCHEMA),
        ("imports", IMPORTS_SCHEMA),
    ):
        hass.services.async_register(
            DOMAIN, name, handle, schema=schema, supports_response=SupportsResponse.ONLY
        )
