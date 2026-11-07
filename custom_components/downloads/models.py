"""Typed public-contract projections; the Rust service owns domain decisions."""

from dataclasses import dataclass
from enum import StrEnum
from typing import Self
from uuid import UUID

from homeassistant.util.json import JsonArrayType, JsonObjectType


class MediaKind(StrEnum):
    MOVIE = "movie"
    SERIES = "series"


class SearchKind(StrEnum):
    ALL = "all"
    MOVIE = "movie"
    SERIES = "series"


class Component(StrEnum):
    RADARR = "radarr"
    SONARR = "sonarr"
    PROWLARR = "prowlarr"
    SABNZBD = "sabnzbd"
    VPN = "vpn"
    FIREWALL = "firewall"
    DNS = "dns"
    STORAGE = "storage"
    PLEX = "plex"


class Operation(StrEnum):
    REQUEST_MOVIE = "request_movie"
    REQUEST_SERIES = "request_series"
    TRANSFER = "transfer"
    IMPORT_MOVIE = "import_movie"
    IMPORT_SERIES = "import_series"


class Outcome(StrEnum):
    HEALTHY = "healthy"
    DEGRADED = "degraded"
    UNAVAILABLE = "unavailable"
    NOT_CONFIGURED = "not_configured"


class HealthState(StrEnum):
    HEALTHY = "healthy"
    DEGRADED = "degraded"
    UNAVAILABLE = "unavailable"
    NOT_CONFIGURED = "not_configured"
    STALE = "stale"
    UNKNOWN = "unknown"


class ErrorCode(StrEnum):
    AUTHENTICATION_REQUIRED = "authentication_required"
    INVALID_ARGUMENTS = "invalid_arguments"
    PERMISSION_DENIED = "permission_denied"
    NOT_FOUND = "not_found"
    NOT_CONFIGURED = "not_configured"
    UPSTREAM_UNAVAILABLE = "upstream_unavailable"
    INVALID_UPSTREAM_RESPONSE = "invalid_upstream_response"
    PROFILE_REVISION_CONFLICT = "profile_revision_conflict"
    EXISTING_SETTINGS_CONFLICT = "existing_settings_conflict"
    IDEMPOTENCY_CONFLICT = "idempotency_conflict"
    INVALID_SCOPE = "invalid_scope"
    AMBIGUOUS_OPERATION_TARGET = "ambiguous_operation_target"
    PRECONDITION_REQUIRED = "precondition_required"
    REVISION_CONFLICT = "revision_conflict"
    UNSUPPORTED_TRANSPORT_OPERATION = "unsupported_transport_operation"


def record(value: object) -> dict[str, object]:
    """Narrow the dynamic JSON boundary before reading owned fields."""
    if not isinstance(value, dict) or any(not isinstance(key, str) for key in value):
        raise ValueError("Expected an object")
    return value


def exact(value: object, keys: set[str]) -> dict[str, object]:
    result = record(value)
    if result.keys() != keys:
        raise ValueError("Invalid object fields")
    return result


def text(value: object, *, maximum: int = 200) -> str:
    if (
        not isinstance(value, str)
        or not value.strip()
        or len(value) > maximum
        or any(ord(char) < 32 or ord(char) == 127 for char in value)
    ):
        raise ValueError("Invalid text")
    return value.strip()


def integer(value: object, *, minimum: int = 0, maximum: int = 2**64 - 1) -> int:
    if type(value) is not int or not minimum <= value <= maximum:
        raise ValueError("Invalid integer")
    return value


def identity(value: object) -> str:
    result = UUID(text(value, maximum=36))
    if result.int == 0:
        raise ValueError("Invalid UUID")
    return str(result)


def boolean(value: object) -> bool:
    if type(value) is not bool:
        raise ValueError("Invalid boolean")
    return value


def request_document(value: object) -> JsonObjectType:
    """Reject cross-kind fields and invalid enum shapes before an HTTP write."""
    body = record(value)
    kind = MediaKind(text(body.get("kind")))
    id_field = "tmdb_id" if kind is MediaKind.MOVIE else "tvdb_id"
    exact(body, {"kind", id_field, "profile", "intent"})
    media_id = integer(body[id_field], minimum=1, maximum=2**31 - 1)
    profile = exact(body["profile"], {"id", "revision"})
    profile_id = identity(profile["id"])
    revision = integer(profile["revision"], minimum=1)
    incoming_intent = record(body["intent"])
    mode = text(incoming_intent.get("mode"))
    intent: JsonObjectType
    if mode == "monitor_only" or (mode == "search_now" and kind is MediaKind.MOVIE):
        exact(incoming_intent, {"mode"})
        intent = {"mode": mode}
    elif mode == "search_now" and kind is MediaKind.SERIES:
        exact(incoming_intent, {"mode", "scope"})
        scope = record(incoming_intent["scope"])
        if scope.get("kind") == "all_aired":
            exact(scope, {"kind"})
            intent = {"mode": mode, "scope": {"kind": "all_aired"}}
        elif scope.get("kind") == "seasons":
            exact(scope, {"kind", "numbers"})
            numbers = scope["numbers"]
            if not isinstance(numbers, list) or not 1 <= len(numbers) <= 100:
                raise ValueError("Choose 1 to 100 seasons")
            seasons = sorted(integer(number, maximum=65535) for number in numbers)
            if len(set(seasons)) != len(seasons):
                raise ValueError("Duplicate season")
            season_values: JsonArrayType = [number for number in seasons]
            intent = {
                "mode": mode,
                "scope": {"kind": "seasons", "numbers": season_values},
            }
        else:
            raise ValueError("Invalid scope")
    else:
        raise ValueError("Invalid intent")
    return {
        "kind": kind.value,
        id_field: media_id,
        "profile": {"id": profile_id, "revision": revision},
        "intent": intent,
    }


@dataclass(frozen=True, slots=True)
class System:
    version: str
    read: bool
    request: bool
    admin: bool
    role: str

    @classmethod
    def parse(cls, value: object) -> Self:
        data = record(value)
        capabilities = record(data.get("capabilities"))
        role = text(data.get("credential_role"))
        if role not in {"read_only", "requester", "administrator", "integration"}:
            raise ValueError("Invalid credential role")
        return cls(
            text(data.get("version"), maximum=80),
            boolean(capabilities.get("read")),
            boolean(capabilities.get("request")),
            boolean(capabilities.get("admin")),
            role,
        )


@dataclass(frozen=True, slots=True)
class ComponentHealth:
    component: Component
    outcome: Outcome | None
    observed_at: int | None
    expires_at: int | None
    stale: bool

    def state_at(self, now_ms: int) -> HealthState:
        if self.outcome is None:
            return HealthState.UNKNOWN
        if self.observed_at is None or now_ms < self.observed_at:
            return HealthState.UNKNOWN
        if self.stale or self.expires_at is None or now_ms >= self.expires_at:
            return HealthState.STALE
        return HealthState(self.outcome.value)

    @classmethod
    def parse(cls, value: object) -> Self:
        data = record(value)
        component = Component(text(data.get("component")))
        observation = record(data.get("observation"))
        if observation.get("state") == "never_checked":
            return cls(component, None, None, None, False)
        if observation.get("state") != "observed":
            raise ValueError("Invalid observation")
        outcome = Outcome(text(record(observation.get("outcome")).get("state")))
        freshness = observation.get("freshness")
        if freshness not in {"fresh", "stale"}:
            raise ValueError("Invalid freshness")
        observed = integer(observation.get("observed_at_unix_ms"))
        expires = integer(observation.get("expires_at_unix_ms"))
        if expires < observed:
            raise ValueError("Invalid expiry")
        return cls(component, outcome, observed, expires, freshness == "stale")


@dataclass(frozen=True, slots=True)
class Readiness:
    operation: Operation
    profile_id: str | None
    ready: bool
    expires_at: int

    @classmethod
    def parse(cls, value: object) -> Self:
        data = record(value)
        state = data.get("state")
        if state not in {"ready", "blocked"}:
            raise ValueError("Invalid readiness")
        profile_id = data.get("profile_id")
        return cls(
            Operation(text(data.get("operation"))),
            None if profile_id is None else identity(profile_id),
            state == "ready",
            integer(data.get("expires_at_unix_ms")),
        )


@dataclass(frozen=True, slots=True)
class HealthSnapshot:
    components: tuple[ComponentHealth, ...]
    readiness: tuple[Readiness, ...]
    generated_at: int

    @classmethod
    def parse(cls, value: object) -> Self:
        data = record(value)
        components, readiness = data.get("components"), data.get("readiness")
        if not isinstance(components, list) or len(components) > len(Component):
            raise ValueError("Invalid health components")
        if not isinstance(readiness, list) or len(readiness) > 500:
            raise ValueError("Invalid operation readiness")
        parsed_components = tuple(ComponentHealth.parse(item) for item in components)
        if len({item.component for item in parsed_components}) != len(parsed_components):
            raise ValueError("Duplicate component")
        parsed_readiness = tuple(Readiness.parse(item) for item in readiness)
        if len({(item.operation, item.profile_id) for item in parsed_readiness}) != len(
            parsed_readiness
        ):
            raise ValueError("Duplicate operation readiness")
        return cls(
            parsed_components,
            parsed_readiness,
            integer(data.get("generated_at_unix_ms")),
        )

    def ready_at(self, operation: Operation, now_ms: int) -> bool | None:
        """Never guess a profile target or retain expired success."""
        matches = [item for item in self.readiness if item.operation is operation]
        if now_ms < self.generated_at or len(matches) != 1 or matches[0].expires_at <= now_ms:
            return None
        return matches[0].ready

    def stack_state_at(self, now_ms: int) -> HealthState:
        if {item.component for item in self.components} != set(Component):
            return HealthState.UNKNOWN
        states = [item.state_at(now_ms) for item in self.components]
        configured = [state for state in states if state is not HealthState.NOT_CONFIGURED]
        if not configured:
            return HealthState.NOT_CONFIGURED if states else HealthState.UNKNOWN
        for state in (
            HealthState.STALE,
            HealthState.UNKNOWN,
            HealthState.UNAVAILABLE,
            HealthState.DEGRADED,
        ):
            if state in configured:
                return state
        return HealthState.HEALTHY
