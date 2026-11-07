"""Run inside the owned app namespace, using real native sockets and TLS."""

import json
import os
import socket
import ssl
import struct
import sys
import urllib.request

mode = sys.argv[1]
context = ssl.create_default_context(cafile="/fixture/cert.pem")
if mode == "tls-listeners":
    # A connection interrupted before its TLS handshake must not prevent the
    # fixture from accepting another client, as happens during a tunnel cut.
    for port in (443, 853):
        with socket.create_connection(("127.0.0.1", port), timeout=2):
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=2) as raw:
                    with context.wrap_socket(raw, server_hostname="origin.test") as stream:
                        if port == 443:
                            stream.sendall(
                                b"GET /handshake-probe HTTP/1.0\r\nHost: origin.test\r\n\r\n"
                            )
                            assert b"200" in stream.recv(512)
                        else:
                            query = b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00"
                            query += b"\x07example\x03com\x00\x00\x01\x00\x01"
                            stream.sendall(struct.pack("!H", len(query)) + query)
                            assert stream.recv(4096)[2:4] == b"\x12\x34"
            except OSError as error:
                raise AssertionError(
                    f"TLS listener {port} blocked by an incomplete handshake"
                ) from error
    print(json.dumps({"incomplete_tls_handshakes_do_not_block_listeners": True}), flush=True)
    raise SystemExit(0)
if mode in ("proxy-up", "proxy-down"):
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({"https": "http://198.19.0.4:8888"}),
        urllib.request.HTTPSHandler(context=context),
    )
    try:
        with opener.open("https://example.com/proxy-probe", timeout=6) as response:
            connected = response.status == 200
    except OSError:
        connected = False
    print(json.dumps({"proxy_connected": connected}), flush=True)
    assert connected == (mode == "proxy-up")
    raise SystemExit(0)
result = {}
for label, host, port in [("ipv4", "198.18.0.3", 443), ("ipv6", "2001:db8:1234::3", 8443)]:
    try:
        with socket.create_connection((host, port), timeout=2) as raw:
            with context.wrap_socket(raw, server_hostname="origin.test") as stream:
                stream.sendall(b"GET /probe HTTP/1.0\r\nHost: origin.test\r\n\r\n")
                result[label] = b"200" in stream.recv(512)
    except OSError:
        result[label] = False
# Use a unique name so an old resolver cache cannot make the outage check pass.
name = ("probe-" + os.urandom(6).hex() + ".example.com").split(".")
question = b"".join(bytes([len(x)]) + x.encode() for x in name) + b"\0\0\1\0\1"
query = b"\x12\x34\x01\0\0\1\0\0\0\0\0\0" + question
for label, host in [("system_dns", "127.0.0.1"), ("direct_dns", "198.18.0.3")]:
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.settimeout(2)
            sock.sendto(query, (host, 53))
            answer = sock.recv(4096)
            result[label] = answer[:2] == query[:2] and answer[3] & 15 == 0
    except OSError:
        result[label] = False
# Pin an additional attempt to the physical interface, so a stale tunnel route
# cannot masquerade as kill-switch enforcement. The unprotected control must pass.
with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as raw:
    raw.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, b"eth0\0")
    raw.settimeout(2)
    try:
        raw.connect(("198.18.0.3", 443))
        result["forced_direct_ipv4"] = True
    except OSError:
        result["forced_direct_ipv4"] = False
with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as raw:
    raw.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, b"eth0\0")
    raw.settimeout(2)
    try:
        raw.sendto(query, ("198.18.0.3", 53))
        result["forced_direct_dns"] = raw.recv(4096)[:2] == query[:2]
    except OSError:
        result["forced_direct_dns"] = False
print(json.dumps(result), flush=True)
if mode == "up":
    assert result == {
        "ipv4": True,
        "ipv6": False,
        "system_dns": True,
        "direct_dns": True,
        "forced_direct_ipv4": False,
        "forced_direct_dns": False,
    }, result
elif mode == "down":
    assert not any(result.values()), result
elif mode == "control":
    assert all(
        result[key]
        for key in ("ipv4", "ipv6", "direct_dns", "forced_direct_ipv4", "forced_direct_dns")
    ), result
else:
    raise ValueError(mode)
