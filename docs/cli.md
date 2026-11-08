# Downloads CLI

`ha-downloads` preserves the published read-only `status`/`doctor` interface and
adds one-request media commands. The `downloads` binary still owns `serve`,
`doctor`, and `openapi`; the client never starts the app or a daemon. Build it
with `cargo build --locked --bin
ha-downloads`, or use the flake's `ha-downloads` package/app on supported systems.

## Existing status and doctor commands

`ha-downloads status` and its `doctor` alias keep the existing command, output,
credential, and readiness behavior. With no arguments, the CLI prints help
offline. Status defaults to native access, regardless of HA environment values:
it reads `/run/downloads/status.json` locally, or accepts `--status-file PATH`.

```sh
ha-downloads status --status-file /run/downloads/status.json
ha-downloads doctor --url https://downloads.example/ --token-file /path/to/token
fetch-home-assistant -- ha-downloads --transport home-assistant status
```

Native remote status uses `--url`/`DOWNLOADS_URL` and
`--token-file`/`DOWNLOADS_TOKEN_FILE`. It does not fall back to the new media
token environment. Native status URLs retain support for a base path. Native
URL and local-file options cannot be combined.

HA status uses only `HOME_ASSISTANT_URL` and `HOME_ASSISTANT_TOKEN`, with an
explicit `--transport home-assistant`; native status options are rejected.
It reads the integration's cached safe `/api/downloads/health` projection, not
the new media API. HA requires HTTPS, except on loopback. Remote readiness uses
the server's check age, independent of the CLI host's clock.

Native status output preserves the status fields and adds `transport` and
`health`. HA status output contains only `transport` and the safe `health`
projection. Exit 0 means ready, 2 means not ready (including stale checks), and
1 means failure. Errors retain the published `{error,message,http_status?}`
shape. Status requests retain their 10-second deadline and 128 KiB response
limit. Existing `downloads doctor` behavior remains unchanged.

## Media connection and credentials

| Transport | Origin | Credential |
| --- | --- | --- |
| `native` | `DOWNLOADS_URL` | `DOWNLOADS_API_TOKEN` or `DOWNLOADS_TOKEN_FILE` |
| `home-assistant` | `HOME_ASSISTANT_URL` | `HOME_ASSISTANT_TOKEN` or `HOME_ASSISTANT_TOKEN_FILE` |

For media commands, one configured origin selects its transport automatically.
Both origins require `--transport`. `--url` overrides the selected origin and
requires an explicit transport. Origins must be HTTP(S), without credentials,
paths, query, or fragment. HA requires HTTPS, except on loopback. Do not use an
ingress URL. Native calls use `/api/v1`; HA calls use `/api/downloads/v1` through
the installed Downloads companion integration.

`--token-file PATH` overrides the selected transport's credential source. A
configured credential file takes precedence over that transport's token
environment; an unreadable file is an error, never permission to fall back.
Trailing newlines in credential files are accepted. There is no token argument,
credential discovery, actor impersonation flag, or cross-transport fallback.
Keep credentials in the existing secret-management wrapper, not shell history.

```sh
ha-downloads --transport native system
fetch-home-assistant -- ha-downloads --transport home-assistant health
```

The native credential determines read/request/admin capabilities. `system`
reports those capabilities and its `credential_role`; the client does not
preflight credentials or invent delegated actor headers. Profile writes are
native-only and still require the server's administrator credential. HA derives
its own user context and checks permissions in the companion integration.

## Commands

| Command | Result |
| --- | --- |
| `system` | Version and credential capabilities |
| `status` / `doctor` | Published read-only status report and readiness exit behavior |
| `health` | Complete health/readiness snapshot |
| `health component COMPONENT` | One component observation |
| `health check --operation OP [--profile-id UUID]` | Scoped readiness snapshot and readiness exit code |
| `search QUERY [--kind all\|movie\|series]` | One search page |
| `managers list` | Configured media managers |
| `managers options radarr\|sonarr` | Current profiles, safe root labels, tags, series types |
| `profiles list [--kind movie\|series]` | Saved profiles |
| `profiles get UUID` | Profile with its exact ETag |
| `profiles create --input FILE --idempotency-key KEY` | Native administrator profile creation |
| `profiles replace UUID --input FILE --if-match '"7"'` | Native administrator optimistic replacement |
| `movies list` / `movies get TMDB_ID` | Movie library page/detail |
| `series list` / `series get TVDB_ID` | Series library page/detail |
| `series episodes TVDB_ID [--season N]` | Episode page |
| `requests list` / `requests get UUID` | Actor-scoped request page/detail |
| `requests lookup --idempotency-key KEY` | Recover the original submission after an uncertain response |
| `requests create --input FILE --idempotency-key KEY` | Submit a typed movie/series request |
| `queue list` | One acquisition queue page |

All paginated commands accept `--limit 1..100` and `--page-token TOKEN`. Each
invocation fetches exactly one page. Preserve `next_page` and `coverage`; an
unavailable/partial source is not an empty library. Search is passed to the
server as data, not interpreted as a URL.

Health component names are `radarr`, `sonarr`, `prowlarr`, `sabnzbd`, `vpn`,
`firewall`, `dns`, `storage`, and `plex`. Operations are `request_movie`,
`request_series`, `transfer`, `import_movie`, and `import_series`. Profile-scoped
operations need an unambiguous server-resolved target. The server owns the
dependency policy; optional Plex/Prowlarr are not blanket prerequisites. A
readiness check requires exactly one matching, unexpired server verdict.
Aggregate `health` and `health check` use `/health/stack`; component reads use
`/health/components/COMPONENT`. The legacy native `/health` route is unchanged.

## Requesting

`--input` defaults to `-` for stdin. Inputs use the server's strict Rust request
models: unknown fields, wrong media IDs, invalid scopes, and invalid revisions
are rejected before an HTTP write. Input is limited to 1 MiB. Profile create and
replace accept `ProfileInput`; request creation accepts exactly `CreateRequest`.
See the [media contract](media-contract.md) for all shapes.

For example, a movie request file contains:

```json
{
  "kind": "movie",
  "tmdb_id": 14337,
  "profile": {
    "id": "276319ec-c797-460c-a49e-6b90fc1f6ae8",
    "revision": 7
  },
  "intent": {"mode": "search_now"}
}
```

Use the actual profile ID/revision returned by this installation:

```sh
ha-downloads requests create --input request.json \
  --idempotency-key movie-request-001
ha-downloads requests lookup --idempotency-key movie-request-001
```

Keys are caller-owned, stable, and 1..128 ASCII letters, digits, hyphens, or
underscores. The CLI never generates or replaces them. Preserve the original
key/body after an uncertain response, then use `lookup` or explicitly replay the
same body/key. No retry, lookup, profile refresh, or second write is automatic.
Profile replacement sends the exact strong `--if-match` supplied by the caller,
including its quotes. A conflict never fetches a newer revision and retries.

An accepted/applied request is not proof that media finished downloading or
importing. Inspect the request and acquisition state for the actual outcome.

## Media output and failure behavior

Successful media-command stdout is one typed JSON object, for example
`managers list` with no configured managers:

```json
{"status":200,"etag":null,"body":{"items":[]}}
```

`body` contains the command's real response model, not an untyped passthrough;
`etag` is the server's exact header or `null`. Media request errors have empty
stdout and safe JSON stderr containing `status` and a closed `error_code`.
Argument errors preserve the published `{error,message,http_status?}` shape
and exit 1. Upstream messages, raw error bodies, URLs, credentials, and request
IDs are not printed. Legacy `status`/`doctor` output is described separately
above and does not use the media envelope.

| Exit | Meaning |
| --- | --- |
| `0` | Command succeeded; a readiness check is ready |
| `1` | Invalid usage, configuration, input, HTTP, decoding, or output failure |
| `2` | Legacy status/doctor health is not ready |
| `3` | Readiness unmet, stale, or ambiguous; valid snapshot remains on stdout |

For media commands, `--timeout SECONDS` sets a total 1..300 second deadline
(default 30), including the response body. Media responses are limited to 8 MiB.
Redirects and protocol/HTTP retries are disabled. Errors ending in
`_outcome_unknown` do not prove a write was unapplied; reconcile using the
preserved caller key before another action.
