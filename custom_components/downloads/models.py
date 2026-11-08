"""Typed public-contract projections; the Rust service owns domain decisions."""

from dataclasses import dataclass
from datetime import datetime
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


class IssueSeverity(StrEnum):
    WARNING = "warning"
    ERROR = "error"


class HealthIssueCode(StrEnum):
    UPDATE_AVAILABLE = "update_available"
    UPSTREAM_HEALTH = "upstream_health"
    INDEXER_UNAVAILABLE = "indexer_unavailable"
    DOWNLOAD_CLIENT_UNAVAILABLE = "download_client_unavailable"
    STORAGE_UNAVAILABLE = "storage_unavailable"
    STORAGE_LOW = "storage_low"
    PROTECTION_UNVERIFIED = "protection_unverified"
    CHECKS_MISSING = "checks_missing"
    CHECKS_STALE = "checks_stale"
    NOT_CONFIGURED = "not_configured"
    UPSTREAM_UNAVAILABLE = "upstream_unavailable"


class UnavailableReason(StrEnum):
    TIMEOUT = "timeout"
    CONNECTION = "connection"
    AUTHENTICATION = "authentication"
    INVALID_RESPONSE = "invalid_response"
    NOT_RUNNING = "not_running"


class ErrorCode(StrEnum):
    AUTHENTICATION_REQUIRED = "authentication_required"
    INVALID_ARGUMENTS = "invalid_arguments"
    PERMISSION_DENIED = "permission_denied"
    NOT_FOUND = "not_found"
    NOT_CONFIGURED = "not_configured"
    UPSTREAM_UNAVAILABLE = "upstream_unavailable"
    INVALID_UPSTREAM_RESPONSE = "invalid_upstream_response"
    NATIVE_SETTINGS_REQUIRED = "native_settings_required"
    EXISTING_SETTINGS_CONFLICT = "existing_settings_conflict"
    IDEMPOTENCY_CONFLICT = "idempotency_conflict"
    INVALID_SCOPE = "invalid_scope"
    PRECONDITION_REQUIRED = "precondition_required"
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
    exact(body, {"kind", id_field, "intent"})
    media_id = integer(body[id_field], minimum=1, maximum=2**31 - 1)
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
        "intent": intent,
    }


class RequestAvailability(StrEnum):
    READY = "ready"
    PARENT_SETUP_REQUIRED = "parent_setup_required"

    @classmethod
    def parse(cls, value: object) -> Self:
        data = exact(value, {"state"})
        return cls(text(data["state"]))


def validate_media_item(value: object) -> None:
    """Check the owned media boundary without exposing native configuration."""
    data = record(value)
    kind = MediaKind(text(data.get("kind")))
    fields = {"kind", "title", "year", "overview", "poster", "request", "library", "acquisition"}
    id_field = "tmdb_id" if kind is MediaKind.MOVIE else "tvdb_id"
    fields.add(id_field)
    if kind is MediaKind.SERIES:
        fields.add("seasons")
    if "metadata" in data:
        fields.add("metadata")
    exact(data, fields)
    integer(data[id_field], minimum=1, maximum=2**31 - 1)
    RequestAvailability.parse(data["request"])
    library = record(data["library"])
    if library.get("state") == "not_added":
        exact(library, {"state"})
    elif library.get("state") == "managed":
        exact(library, {"state", "upstream_id"})
        integer(library["upstream_id"], minimum=1, maximum=2**32 - 1)
    else:
        raise ValueError("Invalid library state")
    if data.get("metadata") is not None:
        validate_media_metadata(kind, data["metadata"])


def validate_quality(value: object) -> None:
    """Validate observed release quality, never native profile settings."""
    data = exact(value, {"name", "resolution", "source", "modifier", "revision"})
    text(data["name"])
    if data["resolution"] is not None:
        integer(data["resolution"], maximum=65535)
    for field in ("source", "modifier"):
        if data[field] is not None:
            text(data[field])
    if data["revision"] is not None:
        revision = exact(data["revision"], {"version", "real", "is_repack"})
        integer(revision["version"], maximum=2**32 - 1)
        integer(revision["real"], maximum=2**32 - 1)
        boolean(revision["is_repack"])


def validate_media_metadata(kind: MediaKind, value: object) -> None:
    fields = {"runtime_minutes", "genres", "certification"}
    fields |= {"studio", "file"} if kind is MediaKind.MOVIE else {"network", "next_airing"}
    data = exact(value, fields)
    if data["runtime_minutes"] is not None:
        integer(data["runtime_minutes"], maximum=2**32 - 1)
    genres = data["genres"]
    if not isinstance(genres, list) or len(genres) > 100:
        raise ValueError("Invalid genres")
    for genre in genres:
        text(genre)
    for field in ("certification", "studio" if kind is MediaKind.MOVIE else "network"):
        if data[field] is not None:
            text(data[field])
    if kind is MediaKind.SERIES and data["next_airing"] is not None:
        timestamp = text(data["next_airing"], maximum=40)
        if not timestamp.endswith("Z"):
            raise ValueError("Invalid UTC timestamp")
        datetime.fromisoformat(timestamp)
    if kind is MediaKind.MOVIE and data["file"] is not None:
        file = exact(
            data["file"],
            {"quality", "size_bytes", "video_codec", "audio_codec", "audio_channels"},
        )
        if file["quality"] is not None:
            validate_quality(file["quality"])
        if file["size_bytes"] is not None:
            integer(file["size_bytes"])
        for field in ("video_codec", "audio_codec", "audio_channels"):
            if file[field] is not None:
                text(file[field])


def validate_media_response(path: str, value: object) -> None:
    """The bridge and tools share the same public media contract."""
    route = path.split("?", 1)[0]
    if route in {"search", "movies", "series"}:
        data = record(value)
        items = data.get("items")
        if not isinstance(items, list) or len(items) > 100:
            raise ValueError("Invalid media page")
        for item in items:
            validate_media_item(item)
    elif len(parts := route.split("/")) == 2 and parts[0] in {"movies", "series"}:
        validate_media_item(value)


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
class HealthIssue:
    """Keep only closed codes; native service messages are not Repair content."""

    code: HealthIssueCode
    severity: IssueSeverity

    @classmethod
    def parse(cls, value: object) -> Self:
        data = record(value)
        return cls(
            HealthIssueCode(text(data.get("code"))), IssueSeverity(text(data.get("severity")))
        )


@dataclass(frozen=True, slots=True)
class ComponentHealth:
    component: Component
    outcome: Outcome | None
    observed_at: int | None
    expires_at: int | None
    stale: bool
    issues: tuple[HealthIssue, ...] = ()
    unavailable_reason: UnavailableReason | None = None

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
        outcome_data = record(observation.get("outcome"))
        outcome = Outcome(text(outcome_data.get("state")))
        issues: tuple[HealthIssue, ...] = ()
        reason = None
        if outcome is Outcome.DEGRADED:
            incoming = outcome_data.get("issues")
            if not isinstance(incoming, list) or not 1 <= len(incoming) <= 100:
                raise ValueError("Invalid health issues")
            issues = tuple(HealthIssue.parse(item) for item in incoming)
        elif outcome is Outcome.UNAVAILABLE:
            reason = UnavailableReason(text(outcome_data.get("reason")))
        freshness = observation.get("freshness")
        if freshness not in {"fresh", "stale"}:
            raise ValueError("Invalid freshness")
        observed = integer(observation.get("observed_at_unix_ms"))
        expires = integer(observation.get("expires_at_unix_ms"))
        if expires < observed:
            raise ValueError("Invalid expiry")
        return cls(component, outcome, observed, expires, freshness == "stale", issues, reason)


@dataclass(frozen=True, slots=True)
class Readiness:
    operation: Operation
    ready: bool
    expires_at: int

    @classmethod
    def parse(cls, value: object) -> Self:
        data = exact(value, {"operation", "state", "reasons", "expires_at_unix_ms"})
        state = data.get("state")
        if state not in {"ready", "blocked"}:
            raise ValueError("Invalid readiness")
        return cls(
            Operation(text(data.get("operation"))),
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
        if not isinstance(readiness, list) or len(readiness) > len(Operation):
            raise ValueError("Invalid operation readiness")
        parsed_components = tuple(ComponentHealth.parse(item) for item in components)
        if len({item.component for item in parsed_components}) != len(parsed_components):
            raise ValueError("Duplicate component")
        parsed_readiness = tuple(Readiness.parse(item) for item in readiness)
        if len({item.operation for item in parsed_readiness}) != len(parsed_readiness):
            raise ValueError("Duplicate operation readiness")
        return cls(
            parsed_components,
            parsed_readiness,
            integer(data.get("generated_at_unix_ms")),
        )

    def ready_at(self, operation: Operation, now_ms: int) -> bool | None:
        """Expire backend decisions without choosing native settings in HA."""
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
