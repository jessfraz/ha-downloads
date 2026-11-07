"""Shared typed operations for native actions and scoped Assist tools."""

import json
from urllib.parse import urlencode

import probatio as vol
from aiohttp import ClientError
from homeassistant.exceptions import HomeAssistantError
from homeassistant.util.json import JsonObjectType

from .access import AccessFailure, AccessGuard
from .client import BackendResponse, ResponseTooLarge
from .const import API_PREFIX, MAX_REQUEST_BYTES
from .models import (
    ErrorCode,
    SearchKind,
    identity,
    integer,
    record,
    request_document,
    text,
)

UUID_SCHEMA = vol.All(str, vol.Match(r"^[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$"))
POSITIVE_ID_SCHEMA = vol.All(int, vol.Range(min=1, max=2**31 - 1))
PROFILE_SCHEMA = vol.Schema(
    {
        vol.Required("id"): UUID_SCHEMA,
        vol.Required("revision"): vol.All(int, vol.Range(min=1, max=2**64 - 1)),
    }
)
SCOPE_SCHEMA = vol.Any(
    vol.Schema({vol.Required("kind"): vol.In(["all_aired"])}),
    vol.Schema(
        {
            vol.Required("kind"): vol.In(["seasons"]),
            vol.Required("numbers"): vol.All(
                [vol.All(int, vol.Range(min=0, max=65535))], vol.Length(min=1, max=100)
            ),
        }
    ),
)
REQUEST_DOCUMENT_SCHEMA = vol.Any(
    vol.Schema(
        {
            vol.Required("kind"): vol.In(["movie"]),
            vol.Required("tmdb_id"): POSITIVE_ID_SCHEMA,
            vol.Required("profile"): PROFILE_SCHEMA,
            vol.Required("intent"): vol.Schema(
                {vol.Required("mode"): vol.In(["search_now", "monitor_only"])}
            ),
        }
    ),
    vol.Schema(
        {
            vol.Required("kind"): vol.In(["series"]),
            vol.Required("tvdb_id"): POSITIVE_ID_SCHEMA,
            vol.Required("profile"): PROFILE_SCHEMA,
            vol.Required("intent"): vol.Any(
                vol.Schema(
                    {
                        vol.Required("mode"): vol.In(["search_now"]),
                        vol.Required("scope"): SCOPE_SCHEMA,
                    }
                ),
                vol.Schema({vol.Required("mode"): vol.In(["monitor_only"])}),
            ),
        }
    ),
)
TARGET_SCHEMA = vol.Any(
    vol.Schema({vol.Required("kind"): vol.In(["request"]), vol.Required("id"): UUID_SCHEMA}),
    vol.Schema(
        {
            vol.Required("kind"): vol.In(["idempotency_key"]),
            vol.Required("id"): UUID_SCHEMA,
        }
    ),
    vol.Schema(
        {
            vol.Required("kind"): vol.In(["movie"]),
            vol.Required("tmdb_id"): POSITIVE_ID_SCHEMA,
        }
    ),
    vol.Schema(
        {
            vol.Required("kind"): vol.In(["series"]),
            vol.Required("tvdb_id"): POSITIVE_ID_SCHEMA,
        }
    ),
)
SEARCH_SCHEMA = vol.Schema(
    {
        vol.Required("query"): vol.All(str, vol.Length(min=1, max=200)),
        vol.Optional("kind", default="all"): vol.In([item.value for item in SearchKind]),
        vol.Optional("limit", default=5): vol.All(int, vol.Range(min=1, max=20)),
    }
)
REQUEST_SCHEMA = vol.Schema(
    {
        vol.Required("request"): REQUEST_DOCUMENT_SCHEMA,
        vol.Required("idempotency_key"): UUID_SCHEMA,
    }
)
STATUS_SCHEMA = vol.Schema({vol.Required("target"): TARGET_SCHEMA})


def response_document(response: BackendResponse) -> JsonObjectType:
    """Never relay raw provider errors or unrecognized error messages."""
    if not 200 <= response.status < 300:
        code = "backend_error"
        try:
            value = record(response.json()).get("error_code")
            if isinstance(value, str):
                code = ErrorCode(value).value
        except ValueError:
            pass
        raise AccessFailure(code, f"Downloads rejected the request (HTTP {response.status}).")
    try:
        data = response.json()
    except ValueError:
        raise AccessFailure("invalid_response", "Downloads returned an invalid response.") from None
    # All success responses are service-owned JSON objects. Their nested domain
    # values are returned as data, never evaluated as instructions or URLs.
    return data


async def _call(
    guard: AccessGuard,
    method: str,
    path: str,
    *,
    body: JsonObjectType | None = None,
    idempotency_key: str | None = None,
) -> JsonObjectType:
    payload = json.dumps(body).encode() if body is not None else None
    if payload is not None and len(payload) > MAX_REQUEST_BYTES:
        raise AccessFailure("invalid_arguments", "The request is too large.")
    actor = await guard.check()
    try:
        response = await guard.coordinator.client.request(
            method,
            f"{API_PREFIX}/{path}",
            actor=actor,
            body=payload,
            idempotency_key=idempotency_key,
        )
    except ClientError, TimeoutError, ResponseTooLarge:
        # A lost acknowledgement does not prove that a submitted request failed.
        await guard.check()
        raise HomeAssistantError(
            "Cannot contact Downloads. A media request may already have been accepted; "
            "check its original idempotency key before retrying."
        ) from None
    await guard.check()
    return response_document(response)


async def async_search(guard: AccessGuard, values: dict[str, object]) -> JsonObjectType:
    query = text(values["query"])
    kind = SearchKind(text(values.get("kind", "all")))
    limit = integer(values.get("limit", 5), minimum=1, maximum=20)
    return await _call(
        guard,
        "GET",
        "search?" + urlencode({"query": query, "kind": kind.value, "limit": limit}),
    )


async def async_request(guard: AccessGuard, values: dict[str, object]) -> JsonObjectType:
    if not guard.request:
        raise AccessFailure("permission_denied", "Media request authority is required.")
    body = request_document(values["request"])
    return await _call(
        guard,
        "POST",
        "requests",
        body=body,
        idempotency_key=identity(values["idempotency_key"]),
    )


async def async_status(guard: AccessGuard, values: dict[str, object]) -> JsonObjectType:
    target = TARGET_SCHEMA(values["target"])
    kind = target["kind"]
    if kind == "request":
        path = f"requests/{identity(target['id'])}"
    elif kind == "idempotency_key":
        path = f"request-keys/{identity(target['id'])}"
    elif kind == "movie":
        path = f"movies/{integer(target['tmdb_id'], minimum=1, maximum=2**31 - 1)}"
    else:
        path = f"series/{integer(target['tvdb_id'], minimum=1, maximum=2**31 - 1)}"
    return await _call(guard, "GET", path)
