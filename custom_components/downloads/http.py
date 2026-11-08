"""Authenticated native HA bridge with a closed route/method allowlist."""

import json
import re
from urllib.parse import urlencode

from aiohttp import ClientError, web
from homeassistant.core import Context, HomeAssistant
from homeassistant.helpers.http import HomeAssistantView

from .access import AccessFailure, current_guard
from .client import ResponseTooLarge
from .const import API_PREFIX, DOMAIN, MAX_REQUEST_BYTES
from .health import (
    HealthAccessError,
    HealthErrorCode,
    async_health_snapshot,
    native_health_coordinator,
)
from .models import Component, identity, request_document
from .operations import response_document

_UUID = r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"
_COMPONENT = "(?:" + "|".join(item.value for item in Component) + ")"
_READ_ROUTES = (
    re.compile(r"system|status|health|health/stack|managers|search|movies|series|queue|requests"),
    re.compile(rf"health/components/{_COMPONENT}"),
    re.compile(rf"requests/{_UUID}|request-keys/{_UUID}"),
    re.compile(r"movies/[1-9][0-9]*|series/[1-9][0-9]*(?:/episodes)?"),
)
_PAGE_QUERY = {"limit", "page_token"}


def _query_keys(path: str) -> set[str]:
    if path == "search":
        return _PAGE_QUERY | {"query", "kind"}
    if path in {"movies", "series", "queue", "requests"}:
        return _PAGE_QUERY
    if path.endswith("/episodes"):
        return _PAGE_QUERY | {"season"}
    if path == "health/stack":
        return {"operation"}
    return set()


def _failure(error: AccessFailure, *, status: int) -> web.Response:
    return web.json_response({"error_code": error.code, "message": str(error)}, status=status)


class DownloadsView(HomeAssistantView):
    url = "/api/downloads/v1/{path:.*}"
    name = "api:downloads:v1"
    requires_auth = True

    def __init__(self, hass: HomeAssistant) -> None:
        self._hass = hass

    async def _proxy(self, request: web.Request, path: str) -> web.Response:
        read_route = any(route.fullmatch(path) for route in _READ_ROUTES)
        if not read_route:
            raise web.HTTPNotFound
        methods = {"GET", "POST"} if path == "requests" else {"GET"}
        if request.method not in methods:
            raise web.HTTPMethodNotAllowed(request.method, methods)
        if (
            len(request.query_string) > 4096
            or request.query.keys() - (_query_keys(path) if request.method == "GET" else set())
            or len(list(request.query.items())) != len(set(request.query.keys()))
        ):
            raise web.HTTPBadRequest(reason="Unsupported query parameters")
        user = request["hass_user"]
        try:
            guard = current_guard(
                self._hass, Context(user_id=user.id), request=request.method == "POST"
            )
            await guard.check()
        except AccessFailure as err:
            return _failure(err, status=503 if err.code == "unavailable" else 403)
        size = request.content_length
        if size is not None and size > MAX_REQUEST_BYTES:
            raise web.HTTPRequestEntityTooLarge(max_size=MAX_REQUEST_BYTES, actual_size=size)
        payload = bytearray()
        async for chunk in request.content.iter_chunked(16 * 1024):
            payload.extend(chunk)
            if len(payload) > MAX_REQUEST_BYTES:
                raise web.HTTPRequestEntityTooLarge(
                    max_size=MAX_REQUEST_BYTES, actual_size=len(payload)
                )
        body = None
        key = None
        try:
            if request.method == "POST":
                key = identity(request.headers.get("Idempotency-Key"))
                body = json.dumps(request_document(json.loads(payload))).encode()
            elif payload:
                raise ValueError("GET does not accept a body")
        except ValueError, TypeError:
            raise web.HTTPBadRequest(reason="Invalid Downloads request") from None
        try:
            actor = await guard.check()
            response = await guard.coordinator.client.request(
                request.method,
                f"{API_PREFIX}/{path}"
                + ("?" + urlencode(list(request.query.items())) if request.query else ""),
                actor=actor,
                body=body,
                idempotency_key=key,
            )
            await guard.check()
        except AccessFailure as err:
            return _failure(err, status=503 if err.code == "unavailable" else 403)
        except TimeoutError:
            raise web.HTTPGatewayTimeout from None
        except ClientError, ResponseTooLarge:
            raise web.HTTPBadGateway from None
        try:
            data = response_document(response, path=path)
        except AccessFailure as err:
            # Only public API error codes and fixed local messages cross the bridge.
            status = response.status if 400 <= response.status < 600 else 502
            return _failure(err, status=status)
        headers = {
            name: value for name, value in response.headers.items() if name != "Content-Type"
        }
        if request.method == "POST" and response.status == 202:
            try:
                headers["Location"] = "/api/downloads/v1/requests/" + identity(data.get("id"))
            except ValueError, TypeError:
                raise web.HTTPBadGateway from None
        return web.json_response(data, status=response.status, headers=headers)

    get = _proxy
    post = _proxy
    put = _proxy
    patch = _proxy
    delete = _proxy


class DownloadsHealthView(HomeAssistantView):
    """Never forward HA credentials, accept destinations, or fetch app options."""

    url = "/api/downloads/health"
    name = "api:downloads:health"
    requires_auth = True

    def __init__(self, hass: HomeAssistant) -> None:
        self.hass = hass

    async def get(self, request: web.Request) -> web.Response:
        """Read cached health using the authenticated user's entity permission."""
        headers = {"Cache-Control": "no-store"}
        if request.query:
            return self.json({"error": "invalid_arguments"}, 400, headers)
        for entry in self.hass.config_entries.async_loaded_entries(DOMAIN):
            coordinator = native_health_coordinator(entry)
            if coordinator is None:
                continue
            try:
                result = await async_health_snapshot(
                    self.hass,
                    entry,
                    coordinator,
                    request["hass_user"].id,
                    None,
                )
            except HealthAccessError as error:
                code = 503 if error.code is HealthErrorCode.UNAVAILABLE else 403
                return self.json({"error": error.code.value}, code, headers)
            return self.json(result, headers=headers)
        return self.json({"error": "unavailable"}, 503, headers)
