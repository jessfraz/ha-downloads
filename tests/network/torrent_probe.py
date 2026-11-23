"""Exercise the packaged Transmission daemon, never an external torrent swarm."""

import base64
import hashlib
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

PAYLOAD = b"Downloads torrent VPN fixture\n" * 64
STATE = Path("/data/torrent-network-test.json")
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))
RPC_URL = "http://127.0.0.1:8080/transmission/rpc"


def rpc(method: str, arguments: dict[str, object] | None = None) -> dict[str, object]:
    options = json.loads(Path("/data/options.json").read_text())["torrents"]
    return native_rpc(RPC_URL, options["username"], options["password"], method, arguments)


def native_rpc(
    url: str,
    username: str,
    password: str,
    method: str,
    arguments: dict[str, object] | None = None,
) -> dict[str, object]:
    body = json.dumps({"method": method, "arguments": arguments or {}}).encode()
    authorization = "Basic " + base64.b64encode((username + ":" + password).encode()).decode()
    headers = {"Authorization": authorization, "Content-Type": "application/json"}
    # A 409 explicitly rejects the first request before execution. It is the
    # native CSRF handshake, not permission to replay a timed-out mutation.
    for attempt in range(2):
        request = urllib.request.Request(url, data=body, headers=headers)
        try:
            with OPENER.open(request, timeout=10) as response:
                result = json.load(response)
            assert result["result"] == "success", result
            return result["arguments"]
        except urllib.error.HTTPError as error:
            if error.code != 409 or attempt:
                raise
            session = error.headers.get("X-Transmission-Session-Id")
            assert session, "Transmission omitted its native session header"
            headers["X-Transmission-Session-Id"] = session
    raise AssertionError("Transmission did not complete its native handshake")


def no_seeding() -> None:
    session = rpc("session-get")
    assert session["seedRatioLimited"] is True, session
    assert session["seedRatioLimit"] == 0, session
    assert session["speed-limit-up-enabled"] is True, session
    assert session["speed-limit-up"] == 1, session


def manager_handshake(stale_session: str | None = None) -> str:
    options = json.loads(Path("/data/options.json").read_text())["torrents"]
    authorization = (
        "Basic "
        + base64.b64encode((options["username"] + ":" + options["password"]).encode()).decode()
    )
    if stale_session is not None:
        request = urllib.request.Request(
            RPC_URL,
            data=b'{"method":"session-get"}',
            headers={
                "Authorization": authorization,
                "X-Transmission-Session-Id": stale_session,
                "Content-Type": "application/json",
            },
        )
        try:
            OPENER.open(request, timeout=5)
        except urllib.error.HTTPError as error:
            with error:
                assert error.code == 409, "Restart did not reject the previous native session"
                assert error.headers.get("X-Transmission-Session-Id") != stale_session, (
                    "Restart reused the previous native session"
                )
        else:
            raise AssertionError("Restart accepted the previous native session")
    session = None
    for port in (8082, 8080):
        url = f"http://127.0.0.1:{port}/transmission/rpc"
        request = urllib.request.Request(url, headers={"Authorization": authorization})
        try:
            OPENER.open(request, timeout=5)
        except urllib.error.HTTPError as error:
            with error:
                assert error.code == 409, (
                    f"GET RPC session challenge returned {error.code}, expected 409 on port {port}"
                )
                session = error.headers.get("X-Transmission-Session-Id")
                assert session, "Native GET handshake omitted its session header"
        else:
            raise AssertionError("Native GET handshake accepted a missing session header")
    assert session
    headers = {
        "Authorization": authorization,
        "X-Transmission-Session-Id": session,
        "Content-Type": "application/json",
    }
    # Radarr/Sonarr obtain the native challenge using GET, then reuse it for
    # their first authenticated POST. Exercise that exact client sequence.
    request = urllib.request.Request(RPC_URL, headers=headers, data=b'{"method":"session-get"}')
    with OPENER.open(request, timeout=5) as response:
        result = json.load(response)
    assert result["result"] == "success", "Native GET challenge did not authorize the POST"
    assert result["arguments"]["version"], "Native POST omitted its version"
    before = result["arguments"]["speed-limit-up"]
    for port in (8080, 8082):
        for method in ("GET", "POST"):
            request = urllib.request.Request(
                f"http://127.0.0.1:{port}/transmission/rpc",
                method=method,
                data=b'{"method":"session-get"}' if method == "POST" else None,
                headers={"Content-Type": "application/json"},
            )
            try:
                OPENER.open(request, timeout=5)
            except urllib.error.HTTPError as error:
                with error:
                    assert error.code == 401, error.code
                    challenge = error.headers.get("WWW-Authenticate", "")
                    assert challenge.split(" ", 1)[0].lower() == "basic", (
                        "Anonymous RPC omitted its native Basic authentication challenge"
                    )
            else:
                raise AssertionError("Native torrent RPC accepted anonymous access")
    mutation = {"method": "session-set", "arguments": {"speed-limit-up": 33}}
    query = urllib.parse.urlencode(
        {"method": mutation["method"], "arguments": json.dumps(mutation["arguments"])}
    )
    for suffix, body in (("?" + query, None), ("", json.dumps(mutation).encode())):
        request = urllib.request.Request(RPC_URL + suffix, method="GET", headers=headers, data=body)
        try:
            OPENER.open(request, timeout=5)
        except urllib.error.HTTPError as error:
            with error:
                assert error.code == 400, "GET RPC accepted a query or body"
        else:
            raise AssertionError("GET RPC accepted mutation parameters")
    assert rpc("session-get")["speed-limit-up"] == before, "Rejected GET mutated native settings"
    return session


def string(value: bytes) -> bytes:
    return str(len(value)).encode() + b":" + value


def metainfo(name: str, origin: str) -> bytes:
    # One real v1 piece. Both native daemons verify it against the same bytes.
    info = (
        b"d6:lengthi"
        + str(len(PAYLOAD)).encode()
        + b"e4:name"
        + string(name.encode())
        + b"12:piece lengthi16384e6:pieces"
        + string(hashlib.sha1(PAYLOAD).digest())
        + b"7:privatei1ee"
    )
    announce = origin + "/torrent/announce?case=" + urllib.parse.quote(name)
    return b"d8:announce" + string(announce.encode()) + b"4:info" + info + b"e"


def add_torrent(name: str, origin: str) -> dict[str, object]:
    result = rpc(
        "torrent-add",
        {
            "metainfo": base64.b64encode(metainfo(name, origin)).decode(),
            "paused": True,
        },
    )
    added = result.get("torrent-added")
    assert isinstance(added, dict), result
    # A short bounded transfer keeps its actual peer visible long enough to
    # observe the VPN address, rather than racing a tiny in-memory payload.
    rpc("torrent-set", {"ids": [added["id"]], "downloadLimit": 1, "downloadLimited": True})
    rpc("torrent-start", {"ids": [added["id"]]})
    return {"name": name, "hash": added["hashString"]}


def torrents() -> list[dict[str, object]]:
    return rpc("torrent-get", {"fields": ["hashString", "name", "percentDone", "status", "peers"]})[
        "torrents"
    ]


def completed_and_stopped(item: dict[str, object]) -> bool:
    current = next((row for row in torrents() if row["hashString"] == item["hash"]), None)
    return current is not None and current["percentDone"] == 1 and current["status"] == 0


def wait_completed(item: dict[str, object], require_peer: bool = False) -> str | None:
    deadline = time.monotonic() + 90
    peer_address = None
    while True:
        current = next(row for row in torrents() if row["hashString"] == item["hash"])
        for peer in current["peers"]:
            if peer["address"] == "10.8.0.1" and "transmission" in peer["clientName"].lower():
                peer_address = peer["address"]
        if current["percentDone"] == 1 and current["status"] == 0:
            break
        if time.monotonic() > deadline:
            raise AssertionError("Native torrent failed to download and stop: " + str(torrents()))
        time.sleep(0.5)
    target = Path("/media/downloads/torrents") / item["name"]
    assert target.read_bytes() == PAYLOAD, "Transmission imported corrupt fixture payload"
    if require_peer:
        assert peer_address == "10.8.0.1", "Native BitTorrent peer was not observed through the VPN"
    return peer_address


def save(state: dict[str, object]) -> None:
    STATE.write_text(json.dumps(state))
    STATE.chmod(0o600)


def setup(nonce: str) -> None:
    session = manager_handshake()
    no_seeding()
    name = "torrent-vpn-" + nonce
    item = add_torrent(name, "http://" + name + ".example.com:8088")
    peer = wait_completed(item, require_peer=True)
    # This is only the generated fixture's CSRF nonce, never authentication.
    # Keep it private and unprinted to exercise the real stale-session recovery.
    save({"initial": item, "rpc_session": session})
    print(
        json.dumps({"native_torrent_downloaded_and_stopped": True, "case": name, "peer": peer}),
        flush=True,
    )


def outage(nonce: str) -> None:
    state = json.loads(STATE.read_text())
    ipv4_name = "torrent-outage-v4-" + nonce
    ipv6_name = "torrent-outage-v6-" + nonce
    state["outage_ipv4"] = add_torrent(ipv4_name, "http://" + ipv4_name + ".example.com:8088")
    state["outage_ipv6"] = add_torrent(ipv6_name, "http://[2001:db8:1234::3]:8089")
    save(state)
    # Both are real running transfers. The origin stays reachable by an
    # unprotected control; none of the protected daemon's requests may arrive.
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        for key in ("outage_ipv4", "outage_ipv6"):
            current = next(row for row in torrents() if row["hashString"] == state[key]["hash"])
            assert current["percentDone"] == 0, current
        time.sleep(0.5)
    print(json.dumps({"native_torrent_outage_pending": True}), flush=True)


def verify(restarted: bool) -> None:
    state = json.loads(STATE.read_text())
    no_seeding()
    wait_completed(state["outage_ipv4"])
    assert completed_and_stopped(state["initial"]), torrents()
    if restarted:
        manager_handshake(stale_session=state["rpc_session"])
        current = {row["hashString"] for row in torrents()}
        assert all(
            state[key]["hash"] in current for key in ("initial", "outage_ipv4", "outage_ipv6")
        ), current
    print(json.dumps({"native_torrent_state_preserved": True, "restarted": restarted}), flush=True)


if __name__ == "__main__":
    action = sys.argv[1]
    if action == "setup":
        setup(sys.argv[2])
    elif action == "outage":
        outage(sys.argv[2])
    elif action == "handshake":
        manager_handshake()
        print(json.dumps({"native_manager_get_handshake": True, "get_mutations_rejected": True}))
    elif action in ("recover", "restart"):
        verify(action == "restart")
    else:
        raise ValueError(action)
