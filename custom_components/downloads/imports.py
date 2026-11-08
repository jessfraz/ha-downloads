"""Native import observations and entry-owned completion notifications."""

import logging
import re
from dataclasses import dataclass
from datetime import datetime, timedelta
from urllib.parse import urlencode

from aiohttp import ClientError
from homeassistant.components import persistent_notification
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.storage import Store
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator, UpdateFailed
from homeassistant.util import dt as dt_util
from homeassistant.util.json import JsonObjectType

from .client import Actor, DownloadsClient, ResponseTooLarge
from .const import API_PREFIX, DOMAIN
from .models import MediaKind, exact, integer, record, text, validate_quality

_LOGGER = logging.getLogger(__name__)
_IMPORT_ID = re.compile(r"(radarr|sonarr):([1-9][0-9]{0,9})")
_UNAVAILABLE = {"timeout", "connection", "authentication", "invalid_response", "not_running"}
_UNKNOWN_PLEX = {
    "identity_unavailable",
    "file_evidence_missing",
    "not_matched",
    "ambiguous_match",
    "older_version",
}


def _timestamp(value: object) -> datetime:
    timestamp = text(value, maximum=40)
    if not timestamp.endswith("Z"):
        raise ValueError("Expected UTC timestamp")
    return datetime.fromisoformat(timestamp)


@dataclass(frozen=True, slots=True)
class ImportedItem:
    """Closed projection; the Rust backend owns file and playback proof."""

    id: str
    manager: str
    media: JsonObjectType
    title: str
    episode: str | None
    imported_at: datetime
    plex_verified: bool
    subtitles_verified: bool
    plex_message: str
    subtitle_message: str
    subtitle_state: str

    @classmethod
    def parse(cls, value: object) -> ImportedItem:
        data = exact(
            value,
            {
                "id",
                "manager",
                "media",
                "title",
                "episode",
                "imported_at",
                "quality",
                "plex",
                "subtitles",
                "observed_at_unix_ms",
            },
        )
        import_id = text(data["id"], maximum=18)
        match = _IMPORT_ID.fullmatch(import_id)
        if match is None or int(match[2]) > 2**31 - 1 or data["manager"] != match[1]:
            raise ValueError("Invalid import identity")
        manager = match[1]
        kind = MediaKind.MOVIE if manager == "radarr" else MediaKind.SERIES
        key = "tmdb_id" if kind is MediaKind.MOVIE else "tvdb_id"
        media = exact(data["media"], {"kind", key})
        if media["kind"] != kind.value:
            raise ValueError("Import manager mismatch")
        media_id = integer(media[key], minimum=1, maximum=2**31 - 1)
        episode = None
        if data["episode"] is not None:
            if kind is MediaKind.MOVIE:
                raise ValueError("Movie has an episode")
            number = exact(data["episode"], {"season", "number"})
            season = integer(number["season"], maximum=65535)
            index = integer(number["number"], maximum=65535)
            episode = f"S{season:02}E{index:02}"
        if data["quality"] is not None:
            validate_quality(data["quality"])
        imported_at = _timestamp(data["imported_at"])
        observed = integer(data["observed_at_unix_ms"])
        if int(imported_at.timestamp() * 1000) > observed + 60_000:
            raise ValueError("Import is newer than its evidence")
        plex = record(data["plex"])
        state = text(plex.get("state"))
        if state == "verified":
            exact(plex, {"state", "observed_at_unix_ms"})
            integer(plex["observed_at_unix_ms"])
            plex_message = "Plex: verified available."
        elif state == "not_configured":
            exact(plex, {"state"})
            plex_message = "Plex: not configured."
        elif state in {"unknown", "unavailable"}:
            exact(plex, {"state", "reason"})
            if text(plex["reason"]) not in (_UNKNOWN_PLEX if state == "unknown" else _UNAVAILABLE):
                raise ValueError("Invalid Plex evidence")
            plex_message = "Plex: not yet verified."
        else:
            raise ValueError("Invalid Plex state")
        subtitles = record(data["subtitles"])
        subtitle_state = text(subtitles.get("state"))
        if subtitle_state == "verified":
            if state != "verified":
                raise ValueError("Subtitles require the verified imported file")
            exact(subtitles, {"state", "languages", "observed_at_unix_ms"})
            integer(subtitles["observed_at_unix_ms"])
            languages = subtitles["languages"]
            if not isinstance(languages, list) or len(languages) > 100:
                raise ValueError("Invalid subtitle languages")
            names = sorted({text(language, maximum=80) for language in languages})
            subtitle_message = (
                "Subtitles: verified" + (f" ({', '.join(names)})" if names else "") + "."
            )
        elif subtitle_state == "missing":
            if state != "verified":
                raise ValueError("Missing subtitles require verified file evidence")
            exact(subtitles, {"state", "observed_at_unix_ms"})
            integer(subtitles["observed_at_unix_ms"])
            subtitle_message = "Subtitles: none reported for this file."
        elif subtitle_state == "unknown":
            exact(subtitles, {"state", "reason"})
            if text(subtitles["reason"]) not in {"plex_unverified", "streams_unavailable"}:
                raise ValueError("Invalid subtitle evidence")
            subtitle_message = "Subtitles: not yet verified."
        else:
            raise ValueError("Invalid subtitle state")
        return cls(
            import_id,
            manager,
            {"kind": kind.value, key: media_id},
            text(data["title"]),
            episode,
            imported_at,
            state == "verified",
            subtitle_state == "verified",
            plex_message,
            subtitle_message,
            subtitle_state,
        )


class ImportLedger:
    """Compact durable native IDs without assuming history arrives in date order."""

    def __init__(self, value: object) -> None:
        self.ranges: dict[str, list[tuple[int, int]]] = {}
        for source, ranges in record(value).items():
            if (
                source not in {"radarr", "sonarr"}
                or not isinstance(ranges, list)
                or len(ranges) > 65536
            ):
                raise ValueError("Invalid import ledger")
            parsed = []
            previous = -1
            for pair in ranges:
                if not isinstance(pair, list) or len(pair) != 2:
                    raise ValueError("Invalid ID range")
                start = integer(pair[0], minimum=1, maximum=2**31 - 1)
                end = integer(pair[1], minimum=start, maximum=2**31 - 1)
                if start <= previous + 1:
                    raise ValueError("Unordered ID ranges")
                parsed.append((start, end))
                previous = end
            self.ranges[source] = parsed

    def contains(self, import_id: str) -> bool:
        source, number = import_id.split(":")
        return any(start <= int(number) <= end for start, end in self.ranges.get(source, []))

    def add(self, import_id: str) -> None:
        source, number = import_id.split(":")
        ranges = sorted([*self.ranges.get(source, []), (int(number), int(number))])
        merged: list[tuple[int, int]] = []
        for start, end in ranges:
            if merged and start <= merged[-1][1] + 1:
                merged[-1] = (merged[-1][0], max(end, merged[-1][1]))
            else:
                merged.append((start, end))
        if len(merged) > 65536:
            raise ValueError("Import ledger exceeds its bound")
        self.ranges[source] = merged

    def document(self) -> JsonObjectType:
        return {
            source: [[start, end] for start, end in ranges]
            for source, ranges in self.ranges.items()
        }


class ImportNotifications(DataUpdateCoordinator[None]):
    """Poll independently of health; stable IDs make notification retries upserts."""

    def __init__(self, hass: HomeAssistant, entry: ConfigEntry, client: DownloadsClient) -> None:
        super().__init__(
            hass,
            _LOGGER,
            name="Downloads imports",
            config_entry=entry,
            update_interval=timedelta(seconds=30),
        )
        self.client = client
        self.entry_id = entry.entry_id
        self.store: Store[JsonObjectType] = Store(hass, 1, f"{DOMAIN}.{entry.entry_id}.imports")
        self._baseline: int | None = None
        self._started_at = int(dt_util.utcnow().timestamp() * 1000)
        self._seen = ImportLedger({})
        self._ready = ImportLedger({})
        self._notified: dict[str, str] = {}
        self._dirty = False

    async def _load(self) -> None:
        if self._baseline is not None:
            if self._dirty:
                await self._save()
            return
        saved = await self.store.async_load()
        if saved is None:
            self._baseline = self._started_at
            self._dirty = True
            await self._save()
            return
        data = exact(saved, {"baseline", "seen", "ready", "notified"})
        baseline = integer(data["baseline"])
        seen, ready = ImportLedger(data["seen"]), ImportLedger(data["ready"])
        notified = record(data["notified"])
        if len(notified) > 2000:
            raise ValueError("Invalid stored notification count")
        signatures = {
            text(key, maximum=18): text(value, maximum=64) for key, value in notified.items()
        }
        if any(_IMPORT_ID.fullmatch(key) is None for key in signatures):
            raise ValueError("Invalid stored notification identity")
        self._baseline, self._seen, self._ready, self._notified = baseline, seen, ready, signatures

    async def _save(self) -> None:
        notified: JsonObjectType = dict(self._notified)
        document: JsonObjectType = {
            "baseline": self._baseline,
            "seen": self._seen.document(),
            "ready": self._ready.document(),
            "notified": notified,
        }
        await self.store.async_save(document)
        # Store logs write failures instead of raising. A fresh native readback
        # confirms durability before another observation can advance this state.
        readback = await Store[JsonObjectType](self.hass, 1, self.store.key).async_load()
        if readback != document:
            raise OSError("Import state was not persisted")
        self._dirty = False

    async def _read(self) -> list[ImportedItem]:
        items: list[ImportedItem] = []
        tokens: set[str] = set()
        token = None
        for _ in range(20):
            query = {"limit": "100"}
            query["since"] = (
                dt_util.utc_from_timestamp(self._baseline / 1000).isoformat().replace("+00:00", "Z")
            )
            if token is not None:
                query["page_token"] = token
            response = await self.client.request(
                "GET", f"{API_PREFIX}/imports?{urlencode(query)}", actor=Actor("integration")
            )
            if response.status == 404:  # An older paired backend can still provide health.
                return []
            if response.status != 200:
                raise ValueError("Import history unavailable")
            page = exact(response.json(), {"items", "coverage", "next_page"})
            records, coverage = page["items"], page["coverage"]
            if (
                not isinstance(records, list)
                or len(records) > 100
                or not isinstance(coverage, list)
                or len(coverage) > 2
            ):
                raise ValueError("Invalid imports page")
            sources = {}
            for value in coverage:
                source = exact(value, {"source", "state", "observed_at_unix_ms"})
                name, state = text(source["source"]), text(source["state"])
                integer(source["observed_at_unix_ms"])
                if (
                    name not in {"radarr", "sonarr"}
                    or name in sources
                    or state not in {"available", "partial", "unavailable", "unsupported"}
                ):
                    raise ValueError("Invalid import coverage")
                sources[name] = state
            parsed = [ImportedItem.parse(item) for item in records]
            # One unavailable manager must not hold up the other's proven imports.
            # Last-seen records remain useful to Activity, but cannot notify here.
            items.extend(
                item for item in parsed if sources.get(item.manager) in {"available", "partial"}
            )
            if page["next_page"] is None:
                return items
            token = text(page["next_page"], maximum=4096)
            if token in tokens:
                raise ValueError("Repeated import page")
            tokens.add(token)
        raise ValueError("Import history exceeds bounded window")

    async def _async_update_data(self) -> None:
        try:
            await self._load()
            items = await self._read()
            for item in sorted(items, key=lambda value: (value.imported_at, value.id)):
                at = int(item.imported_at.timestamp() * 1000)
                previous = self._notified.get(item.id)
                seen = self._seen.contains(item.id)
                if at < self._baseline or (seen and previous is None):
                    continue
                label = item.title + (f" {item.episode}" if item.episode else "")
                title = (
                    f"Available in Plex: {label}" if item.plex_verified else f"Imported: {label}"
                )
                message = (
                    f"{label} was imported into your library.\n\n"
                    f"{item.plex_message}\n\n{item.subtitle_message}"
                )
                # A corrected title or reordered subtitle languages is not a new completion.
                signature = f"{item.plex_verified}:{item.subtitle_state}"
                if signature != previous:
                    # Notification Markdown is data; native titles/languages cannot create links.
                    safe_message = re.sub(r"([\\`*_{}\[\]()#+.!|<>])", r"\\\1", message)
                    persistent_notification.async_create(
                        self.hass,
                        safe_message,
                        title,
                        notification_id=f"downloads_{self.entry_id}_{item.id.replace(':', '_')}",
                    )
                    payload = {
                        "import_id": item.id,
                        "media": item.media,
                        "imported_at": item.imported_at.isoformat(),
                        "plex_verified": item.plex_verified,
                        "subtitles_verified": item.subtitles_verified,
                    }
                    if not seen:
                        self.hass.bus.async_fire("downloads_media_imported", payload)
                    if (
                        item.plex_verified
                        and item.subtitles_verified
                        and not self._ready.contains(item.id)
                    ):
                        self.hass.bus.async_fire("downloads_media_ready", payload)
                        self._ready.add(item.id)
                    self._notified[item.id] = signature
                    self._seen.add(item.id)
                    self._dirty = True
            if len(self._notified) > 2000:
                live = {item.id for item in items}
                self._notified = {
                    key: value for key, value in self._notified.items() if key in live
                }
            if self._dirty:
                await self._save()
        except ClientError, TimeoutError, ResponseTooLarge, ValueError, OSError:
            raise UpdateFailed("Cannot verify Downloads import history") from None


@callback
def _updated() -> None:
    """Keep the entry-owned observer polling without creating another entity."""


async def async_setup_import_notifications(
    hass: HomeAssistant, entry: ConfigEntry, client: DownloadsClient
) -> ImportNotifications:
    coordinator = ImportNotifications(hass, entry, client)
    entry.async_on_unload(coordinator.async_add_listener(_updated))
    # Optional history/Plex latency must not hold up the existing health entry.
    entry.async_create_background_task(
        hass, coordinator.async_request_refresh(), name="Downloads import notifications"
    )
    return coordinator
