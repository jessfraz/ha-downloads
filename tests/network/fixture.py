"""Disposable TLS DNS/origin fixture. Keys and certificates are generated per run."""

import ipaddress
import json
import os
import socket
import socketserver
import ssl
import struct
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

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


class Origin(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        print(json.dumps({"http_peer": self.client_address[0], "path": self.path}), flush=True)
        # Empty blocklists are valid test data; public-IP lookup remains native and typed.
        body = (
            b'{"ip":"198.19.0.2","public_ip":"198.19.0.2"}'
            if "ipinfo.io" in self.headers.get("Host", "")
            else b""
        )
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args: object) -> None:
        pass


class V6Http(ThreadingHTTPServer):
    address_family = socket.AF_INET6


class ReusableTcp(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain("/fixture/cert.pem", "/fixture/key.pem")
servers = []
if os.environ.get("ROLE") == "vpn":
    server = ReusableTcp(("0.0.0.0", 853), DnsTcp)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    servers.append(server)
for address, cls, port in [("0.0.0.0", ThreadingHTTPServer, 443), ("::", V6Http, 8443)]:
    server = cls((address, port), Origin)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    servers.append(server)
servers.append(socketserver.ThreadingUDPServer(("0.0.0.0", 53), DnsUdp))
servers.append(ReusableTcp(("0.0.0.0", 53), DnsTcp))
for server in servers:
    threading.Thread(target=server.serve_forever, daemon=True).start()
threading.Event().wait()
