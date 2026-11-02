"""Run inside the owned app namespace, using real native sockets and TLS."""

import json
import os
import socket
import ssl
import sys

mode = sys.argv[1]
context = ssl.create_default_context(cafile="/fixture/cert.pem")
result = {}
for label, host, port in [("ipv4", "198.18.0.3", 443), ("ipv6", "fd12:3456:789a::3", 8443)]:
    try:
        with socket.create_connection((host, port), timeout=2) as raw:
            with context.wrap_socket(raw, server_hostname="origin.test") as stream:
                stream.sendall(b"GET /probe HTTP/1.0\r\nHost: origin.test\r\n\r\n")
                result[label] = b"200" in stream.recv(512)
    except OSError:
        result[label] = False
# Use a unique name so an old resolver cache cannot make the outage check pass.
name = ("probe-" + os.urandom(6).hex() + ".test").split(".")
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
print(json.dumps(result), flush=True)
if mode == "up":
    assert result == {"ipv4": True, "ipv6": False, "system_dns": True, "direct_dns": True}, result
elif mode == "down":
    assert not any(result.values()), result
elif mode == "control":
    assert result["ipv4"] and result["ipv6"] and result["direct_dns"], result
else:
    raise ValueError(mode)
