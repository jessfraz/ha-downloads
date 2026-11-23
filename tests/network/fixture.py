"""Disposable TLS DNS/origin fixture. Keys and certificates are generated per run."""

import base64
import ipaddress
import json
import os
import secrets
import socket
import socketserver
import ssl
import struct
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

from torrent_probe import PAYLOAD, metainfo, native_rpc

ORIGIN = os.environ.get("ORIGIN", "198.18.0.3")


def dns_answer(query: bytes) -> bytes:
    if len(query) < 12:
        return b""
    end = 12
    while end < len(query) and query[end]:
        end += query[end] + 1
    end += 5
    question = query[12:end]
    qtype = struct.unpack("!H", question[-4:-2])[0]
    if Path("/tmp/dns-fail").exists():
        return query[:2] + b"\x81\x82\x00\x01\x00\x00\x00\x00\x00\x00" + question
    answer = b""
    if qtype == 1:
        answer = (
            b"\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x00\x00\x04"
            + ipaddress.ip_address(ORIGIN).packed
        )
    print(json.dumps({"dns": True, "question_hex": question.hex()}), flush=True)
    return (
        query[:2]
        + b"\x81\x80\x00\x01"
        + struct.pack("!H", bool(answer))
        + b"\x00\x00\x00\x00"
        + question
        + answer
    )


def read_exact(sock: socket.socket, count: int) -> bytes:
    result = b""
    while len(result) < count:
        block = sock.recv(count - len(result))
        if not block:
            raise EOFError
        result += block
    return result


class DnsTcp(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        if isinstance(self.request, ssl.SSLSocket):
            print(json.dumps({"dns_tls_peer": self.client_address[0]}), flush=True)
        try:
            while True:
                length = struct.unpack("!H", read_exact(self.request, 2))[0]
                query = read_exact(self.request, length)
                answer = dns_answer(query)
                self.request.sendall(struct.pack("!H", len(answer)) + answer)
        except (EOFError, OSError):
            pass


class DnsUdp(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        query, sock = self.request
        sock.sendto(dns_answer(query), self.client_address)


class NativeSeeder:
    """An actual upstream daemon seeds only this namespace's generated data."""

    def __init__(self) -> None:
        self.directory = tempfile.TemporaryDirectory(prefix="transmission-fixture-")
        self.root = Path(self.directory.name)
        self.files = self.root / "files"
        self.files.mkdir(mode=0o700)
        self.password = secrets.token_hex(32)
        self.lock = threading.Lock()
        self.ready: set[str] = set()
        settings = {
            "rpc-bind-address": "127.0.0.1",
            "rpc-port": 8084,
            "rpc-authentication-required": True,
            "rpc-username": "fixture",
            "rpc-password": self.password,
            "rpc-whitelist-enabled": False,
            "rpc-host-whitelist-enabled": False,
            "peer-port": 51415,
            "peer-port-random-on-start": False,
            "download-dir": str(self.files),
            "dht-enabled": False,
            "pex-enabled": False,
            "lpd-enabled": False,
            "port-forwarding-enabled": False,
            "ratio-limit-enabled": False,
        }
        config = self.root / "settings.json"
        config.write_text(json.dumps(settings))
        config.chmod(0o600)
        self.process = subprocess.Popen(
            ["/usr/bin/transmission-daemon", "--foreground", "--config-dir", str(self.root)],
            env={"PATH": "/usr/bin:/bin", "HOME": str(self.root), "TR_SAVE_VERSION_FORMAT": "4"},
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        deadline = time.monotonic() + 10
        while True:
            try:
                self.call("session-get")
                break
            except OSError:
                if time.monotonic() > deadline:
                    self.close()
                    raise
                time.sleep(0.1)

    def call(self, method: str, arguments: dict[str, object] | None = None) -> dict[str, object]:
        return native_rpc(
            "http://127.0.0.1:8084/transmission/rpc", "fixture", self.password, method, arguments
        )

    def ensure(self, name: str) -> None:
        if not name or len(name) > 100 or any(not (c.isalnum() or c in "-_") for c in name):
            raise ValueError("Invalid generated torrent name")
        with self.lock:
            if name in self.ready:
                return
            (self.files / name).write_bytes(PAYLOAD)
            result = self.call(
                "torrent-add",
                {"metainfo": base64.b64encode(metainfo(name, "http://127.0.0.1:8088")).decode()},
            )
            added = result.get("torrent-added")
            assert isinstance(added, dict), result
            deadline = time.monotonic() + 10
            while True:
                torrents = self.call(
                    "torrent-get", {"ids": [added["id"]], "fields": ["percentDone", "status"]}
                )["torrents"]
                if torrents and torrents[0]["percentDone"] == 1 and torrents[0]["status"] == 6:
                    break
                if time.monotonic() > deadline:
                    raise AssertionError("Native fixture seeder did not verify its generated file")
                time.sleep(0.1)
            self.ready.add(name)

    def close(self) -> None:
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.directory.cleanup()


SEEDER: NativeSeeder | None = None


class Origin(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        print(json.dumps({"http_peer": self.client_address[0], "path": self.path}), flush=True)
        path = urlsplit(self.path)
        if path.path == "/torrent/announce":
            name = parse_qs(path.query).get("case", [""])[0]
            assert SEEDER is not None
            if self.client_address[0] != "127.0.0.1":
                SEEDER.ensure(name)
            print(
                json.dumps(
                    {
                        "torrent_tracker_peer": self.client_address[0],
                        "case": name,
                        "agent": self.headers.get("User-Agent", ""),
                    }
                ),
                flush=True,
            )
            peer = socket.inet_aton("10.8.0.1") + struct.pack("!H", 51415)
            self.reply(b"d8:intervali1e12:min intervali1e5:peers6:" + peer + b"e")
            return
        # Empty blocklists are valid test data; public-IP lookup remains native and typed.
        body = (
            b'{"ip":"198.19.0.2","public_ip":"198.19.0.2"}'
            if "ipinfo.io" in self.headers.get("Host", "")
            else b""
        )
        self.reply(body)

    def reply(self, body: bytes) -> None:
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args: object) -> None:
        pass


class ReusableTcp(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


class TlsMixIn:
    def get_request(
        self,
    ) -> tuple[ssl.SSLSocket, tuple[str, int] | tuple[str, int, int, int]]:
        request, address = super().get_request()
        try:
            # A listening SSLSocket performs the handshake in accept(), before
            # ThreadingMixIn can dispatch. A peer lost during a tunnel cut can
            # then block every subsequent TLS client. Handshake in the worker.
            return context.wrap_socket(
                request, server_side=True, do_handshake_on_connect=False
            ), address
        except OSError:
            request.close()
            raise

    def process_request_thread(
        self, request: ssl.SSLSocket, address: tuple[str, int] | tuple[str, int, int, int]
    ) -> None:
        try:
            request.settimeout(5)
            request.do_handshake()
            request.settimeout(None)
        except OSError:
            self.shutdown_request(request)
            return
        super().process_request_thread(request, address)


class TlsTcp(TlsMixIn, ReusableTcp):
    pass


class TlsHttp(TlsMixIn, ThreadingHTTPServer):
    pass


class V6Http(TlsHttp):
    address_family = socket.AF_INET6


class V6PlainHttp(ThreadingHTTPServer):
    address_family = socket.AF_INET6


context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain("/fixture/cert.pem", "/fixture/key.pem")
servers = []
if os.environ.get("ROLE") == "vpn":
    SEEDER = NativeSeeder()
    server = TlsTcp(("0.0.0.0", 853), DnsTcp)
    servers.append(server)
for address, cls, port in [("0.0.0.0", TlsHttp, 443), ("::", V6Http, 8443)]:
    server = cls((address, port), Origin)
    servers.append(server)
servers.append(ThreadingHTTPServer(("0.0.0.0", 8088), Origin))
servers.append(V6PlainHttp(("::", 8089), Origin))
servers.append(socketserver.ThreadingUDPServer(("0.0.0.0", 53), DnsUdp))
servers.append(ReusableTcp(("0.0.0.0", 53), DnsTcp))
for server in servers:
    threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    threading.Event().wait()
finally:
    if SEEDER is not None:
        SEEDER.close()
