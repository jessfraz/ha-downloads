"""Bounded, fixed-origin transport using a dedicated integration credential."""

from dataclasses import dataclass
from urllib.parse import urlsplit

from aiohttp import ClientSession, ClientTimeout
from homeassistant.util.json import JsonObjectType, json_loads_object

from .const import MAX_RESPONSE_BYTES, REQUEST_TIMEOUT_SECONDS


class InvalidBackendURL(ValueError):
    """The configured destination is not one HTTP(S) origin."""


class ResponseTooLarge(ValueError):
    """The response exceeded the public API boundary."""


def normalize_backend_url(value: str) -> str:
    """Do not accept credentials, ingress paths, or request-selected origins."""
    try:
        parsed = urlsplit(value)
        port = parsed.port
    except ValueError as err:
        raise InvalidBackendURL from err
    if (
        parsed.scheme not in {"http", "https"}
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.path not in {"", "/"}
        or parsed.query
        or parsed.fragment
        or any(character.isspace() for character in value)
        or "\\" in value
        or port == 0
    ):
        raise InvalidBackendURL
    return value.rstrip("/")


@dataclass(frozen=True, slots=True)
class Actor:
    """Identity and authority derived inside Home Assistant, never from JSON."""

    identity: str
    can_request: bool = False

    def headers(self) -> dict[str, str]:
        """The integration credential can delegate requests but never admin."""
        return {
            "X-Downloads-Actor-Id": self.identity,
            "X-Downloads-Actor-Can-Request": "true" if self.can_request else "false",
        }


@dataclass(frozen=True, slots=True)
class BackendResponse:
    """The bounded response and protocol headers, not framework log context."""

    status: int
    body: bytes
    headers: dict[str, str]

    def json(self) -> JsonObjectType:
        """Decode only when the owning endpoint expects JSON."""
        return json_loads_object(self.body)


class DownloadsClient:
    """Only adapter-owned routes can reach the configured service."""

    def __init__(self, session: ClientSession, backend_url: str, token: str) -> None:
        self._session = session
        self._backend_url = normalize_backend_url(backend_url)
        self._token = token

    async def request(
        self,
        method: str,
        path: str,
        *,
        actor: Actor | None = None,
        body: bytes | None = None,
        idempotency_key: str | None = None,
    ) -> BackendResponse:
        """No redirect, automatic retry, incoming headers, or credential fallback."""
        headers = {
            "Authorization": f"Bearer {self._token}",
            "Accept": "application/json",
        }
        if actor is not None:
            headers.update(actor.headers())
        if body is not None:
            headers["Content-Type"] = "application/json"
        if idempotency_key is not None:
            headers["Idempotency-Key"] = idempotency_key
        async with self._session.request(
            method,
            f"{self._backend_url}{path}",
            data=body,
            headers=headers,
            allow_redirects=False,
            timeout=ClientTimeout(total=REQUEST_TIMEOUT_SECONDS),
        ) as response:
            payload = bytearray()
            async for chunk in response.content.iter_chunked(16 * 1024):
                payload.extend(chunk)
                if len(payload) > MAX_RESPONSE_BYTES:
                    raise ResponseTooLarge
            return BackendResponse(
                response.status,
                bytes(payload),
                {
                    name: response.headers[name]
                    for name in ("Content-Type", "ETag", "Retry-After", "Cache-Control")
                    if name in response.headers
                },
            )
