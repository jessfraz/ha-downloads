"""Real isolated OpenVPN, TLS DNS and kill-switch test. No provider account is used."""

import json
import os
import secrets
import shutil
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
IMAGE = sys.argv[1]
NONCE = secrets.token_hex(6)
LABEL = "downloads-network-test=" + NONCE
NETWORK = "downloads-test-" + NONCE
TEST_IMAGE = "ha-downloads:network-" + NONCE
CONTAINERS = []
NETWORK_CREATED = False
IMAGE_CREATED = False
WORK = ROOT / "work"
WORK.mkdir(exist_ok=True)
TEMP = Path(tempfile.mkdtemp(prefix="network-", dir=WORK))
os.chmod(TEMP, 0o700)


def run(
    *args: str, check: bool = True, capture: bool = True, cwd: Path | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args, check=check, capture_output=capture, text=True, timeout=300, cwd=cwd
    )


def docker(
    *args: str, check: bool = True, capture: bool = True
) -> subprocess.CompletedProcess[str]:
    return run("docker", *args, check=check, capture=capture)


def start(name: str, *args: str) -> str:
    identifier = docker("run", "--detach", "--name", name, "--label", LABEL, *args).stdout.strip()
    CONTAINERS.append(identifier)
    return identifier


def execute(identifier: str, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return docker("exec", identifier, *args, check=check)


def wait(predicate: Callable[[], bool], description: str, seconds: int = 180) -> None:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(1)
    raise RuntimeError("Timed out: " + description)


def app_status(app: str) -> dict[str, object]:
    response = execute(
        app,
        "/opt/venv/bin/python",
        "-c",
        "import urllib.request,json;"
        "token=json.load(open('/data/options.json'))['api_token'];"
        "request=urllib.request.Request('http://127.0.0.1:8080/api/v1/status',"
        "headers={'Authorization':'Bearer '+token});"
        "print(urllib.request.urlopen(request).read().decode())",
        check=False,
    )
    return json.loads(response.stdout) if response.returncode == 0 else {}


def main() -> dict[str, object]:
    global NETWORK_CREATED, IMAGE_CREATED
    docker("image", "inspect", IMAGE)
    for name in ("fixture.py", "probe.py", "Dockerfile"):
        shutil.copyfile(ROOT / "tests/network" / name, TEMP / name)
    # Only generated test PKI goes into a disposable, unpublished test image.
    san = (
        "subjectAltName=DNS:cloudflare-dns.com,DNS:cloudflare.com,DNS:github.com,"
        "DNS:example.com,DNS:origin.test,DNS:api.ipify.org,IP:198.18.0.3"
    )
    run(
        "openssl",
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        "key.pem",
        "-out",
        "cert.pem",
        "-days",
        "1",
        "-subj",
        "/CN=downloads-test",
        "-addext",
        san,
        cwd=TEMP,
    )
    run(
        "openssl",
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        "ca.key",
        "-out",
        "ca.crt",
        "-days",
        "1",
        "-subj",
        "/CN=downloads-test-vpn",
        cwd=TEMP,
    )
    for role in ("server", "client"):
        run(
            "openssl",
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            role + ".key",
            "-out",
            role + ".csr",
            "-subj",
            "/CN=" + role,
            cwd=TEMP,
        )
        (TEMP / (role + ".ext")).write_text("extendedKeyUsage=" + role + "Auth\n")
        run(
            "openssl",
            "x509",
            "-req",
            "-in",
            role + ".csr",
            "-CA",
            "ca.crt",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-out",
            role + ".crt",
            "-days",
            "1",
            "-extfile",
            role + ".ext",
            cwd=TEMP,
        )
    for key in TEMP.glob("*.key"):
        key.chmod(0o600)
    (TEMP / "key.pem").chmod(0o600)
    docker(
        "build", "--build-arg", "APP_IMAGE=" + IMAGE, "--tag", TEST_IMAGE, str(TEMP), capture=False
    )
    IMAGE_CREATED = True
    docker(
        "network",
        "create",
        "--label",
        LABEL,
        "--internal",
        "--ipv6",
        "--subnet",
        "198.19.0.0/24",
        "--subnet",
        "fd12:3456:789a::/64",
        NETWORK,
    )
    NETWORK_CREATED = True
    server_config = """port 1194
proto udp
dev tun
topology subnet
server 10.8.0.0 255.255.255.0
push "redirect-gateway def1"
ca /fixture/ca.crt
cert /fixture/server.crt
key /fixture/server.key
dh none
keepalive 1 5
persist-key
persist-tun
verb 3
"""
    (TEMP / "server.conf").write_text(server_config)
    data = TEMP / "data"
    (data / "openvpn").mkdir(parents=True, mode=0o700)
    client = (
        "client\ndev tun\nproto udp\nremote 198.19.0.2 1194\nnobind\npersist-key\n"
        "persist-tun\nremote-cert-tls server\nauth-nocache\nverb 3\n"
    )
    for tag, filename in [("ca", "ca.crt"), ("cert", "client.crt"), ("key", "client.key")]:
        client += f"<{tag}>\n{(TEMP / filename).read_text()}</{tag}>\n"
    custom = data / "openvpn/custom.conf"
    custom.write_text(client)
    custom.chmod(0o600)
    options = {
        "vpn": {"provider": "custom"},
        "network": {"local_subnets": []},
        "api_token": secrets.token_hex(32),
    }
    (data / "options.json").write_text(json.dumps(options))
    (data / "options.json").chmod(0o600)
    server_script = """set -eu
ip addr add 198.18.0.3/32 dev lo
iptables -t nat -A PREROUTING -i tun0 -p tcp --dport 853 -j DNAT --to-destination 10.8.0.1:853
iptables -t nat -A PREROUTING -i tun0 -p icmp -j DNAT --to-destination 10.8.0.1
/opt/venv/bin/python /fixture/fixture.py &
exec sleep 600
"""
    server = start(
        "downloads-vpn-" + NONCE,
        "--network",
        NETWORK,
        "--ip",
        "198.19.0.2",
        "--ip6",
        "fd12:3456:789a::3",
        "--cap-add",
        "NET_ADMIN",
        "--device",
        "/dev/net/tun",
        "-e",
        "ROLE=vpn",
        "-v",
        f"{TEMP}:/fixture:ro",
        "--entrypoint",
        "/bin/sh",
        TEST_IMAGE,
        "-c",
        server_script,
    )
    wait(
        lambda: (
            execute(
                server,
                "/opt/venv/bin/python",
                "-c",
                "import socket;s=socket.create_connection(('127.0.0.1',443),2);s.close()",
                check=False,
            ).returncode
            == 0
        ),
        "local DNS/origin server",
    )
    # Independent unprotected control proves the same WAN and IPv6 destinations are reachable.
    control = start(
        "downloads-control-" + NONCE,
        "--network",
        NETWORK,
        "--ip",
        "198.19.0.5",
        "--cap-add",
        "NET_ADMIN",
        "--entrypoint",
        "/bin/sh",
        TEST_IMAGE,
        "-c",
        "ip route replace default via 198.19.0.2; exec sleep 600",
    )
    execute(control, "/opt/venv/bin/python", "/fixture/probe.py", "control")
    app = start(
        "downloads-app-" + NONCE,
        "--network",
        NETWORK,
        "--ip",
        "198.19.0.4",
        "--cap-add",
        "NET_ADMIN",
        "--device",
        "/dev/net/tun",
        "-v",
        f"{data}:/data",
        "--entrypoint",
        "/bin/sh",
        TEST_IMAGE,
        "-c",
        "ip route replace default via 198.19.0.2; exec /usr/local/bin/downloads serve",
    )
    wait(lambda: app_status(app).get("state") == "starting", "runtime waits for VPN")
    sab_pids = (
        "import pathlib;print([int(p.parent.name) for p in "
        "pathlib.Path('/proc').glob('[0-9]*/cmdline') "
        "if b'/opt/SABnzbd.py' in p.read_bytes().split(bytes([0]))])"
    )
    assert json.loads(execute(app, "/opt/venv/bin/python", "-c", sab_pids).stdout) == []
    execute(
        server,
        "openvpn",
        "--config",
        "/fixture/server.conf",
        "--daemon",
        "--writepid",
        "/tmp/test-vpn.pid",
    )
    wait(
        lambda: app_status(app).get("state") == "ready",
        "real runtime VPN/firewall/DNS and SAB readiness",
    )
    execute(
        app,
        "/opt/venv/bin/python",
        "-c",
        """import urllib.request,urllib.error
try:
 urllib.request.urlopen('http://127.0.0.1:8000/v1/vpn/status')
except urllib.error.HTTPError as error:
 assert error.code in (401,403)
else:
 raise AssertionError('Gluetun controls accepted an unauthenticated request')
""",
    )
    initial = app_status(app)
    assert initial["protection"] == {"kill_switch": "verified", "dns": "verified"}, initial
    up = json.loads(execute(app, "/opt/venv/bin/python", "/fixture/probe.py", "up").stdout)
    # SAB owns the pause state. Do not test a lookalike process or bypass the real HTTP API.
    sab_pause = """import configparser,json,urllib.parse,urllib.request
c=configparser.ConfigParser();c.read('/data/sabnzbd/sabnzbd.ini');key=c['misc']['api_key']
base='http://127.0.0.1:8081/sabnzbd/api?'
def call(mode):
 query=urllib.parse.urlencode({'apikey':key,'mode':mode,'output':'json'})
 return json.load(urllib.request.urlopen(base+query))
call('pause');assert call('queue')['queue']['paused'] is True
"""
    execute(app, "/opt/venv/bin/python", "-c", sab_pause)
    # Failed answers remain a visible DNS failure without inventing a direct resolver fallback.
    execute(server, "touch", "/tmp/dns-fail")
    wait(
        lambda: app_status(app).get("protection", {}).get("dns") == "failed", "DNS failure reported"
    )
    dns_failure = app_status(app)
    assert dns_failure["state"] == "degraded", dns_failure
    assert dns_failure["downloader"]["state"] == "running", dns_failure
    execute(server, "rm", "/tmp/dns-fail")
    wait(lambda: app_status(app).get("state") == "ready", "DNS answers recover")
    # A stopped upstream is a real tunnel outage. No namespace or production firewall is edited.
    execute(server, "/bin/sh", "-c", "kill -TERM $(cat /tmp/test-vpn.pid)")
    wait(
        lambda: app_status(app).get("state") == "degraded",
        "runtime observes tunnel outage",
        seconds=90,
    )
    down = json.loads(execute(app, "/opt/venv/bin/python", "/fixture/probe.py", "down").stdout)
    execute(control, "/opt/venv/bin/python", "/fixture/probe.py", "control")
    execute(
        server,
        "openvpn",
        "--config",
        "/fixture/server.conf",
        "--daemon",
        "--writepid",
        "/tmp/test-vpn.pid",
    )
    wait(lambda: app_status(app).get("state") == "ready", "VPN reconnects without resuming queue")
    execute(app, "/opt/venv/bin/python", "-c", sab_pause.replace("call('pause');", ""))
    execute(app, "/opt/venv/bin/python", "/fixture/probe.py", "up")
    # Abrupt loss of Gluetun must stop SAB; it cannot leave a downloader behind.
    execute(app, "/bin/sh", "-c", "kill -KILL $(pidof gluetun-entrypoint)")
    wait(
        lambda: app_status(app).get("downloader", {}).get("state") == "stopped",
        "child-exit cleanup",
    )
    assert json.loads(execute(app, "/opt/venv/bin/python", "-c", sab_pids).stdout) == []
    assert execute(app, "pidof", "openvpn", check=False).returncode != 0
    evidence = []
    for line in docker("logs", server).stdout.splitlines():
        try:
            evidence.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    assert any(row.get("dns_tls_peer", "").startswith("10.8.") for row in evidence), evidence
    assert any(row.get("http_peer", "").startswith("10.8.") for row in evidence), evidence
    assert not any(row.get("http_peer") == "198.19.0.4" for row in evidence), evidence
    receipt = {
        "test_only": True,
        "image_id": docker("image", "inspect", IMAGE, "--format", "{{.Id}}").stdout.strip(),
        "dns_failure_reported_without_direct_fallback": True,
        "local_vpn": "openvpn",
        "up": up,
        "outage": down,
        "operator_pause_preserved": True,
        "child_exit_stopped_downloader": True,
        "cold_start_gated": True,
        "dns_tls_and_origin_observed_through_tunnel": True,
        "unauthenticated_gluetun_controls_rejected": True,
        "outage_origin_still_reachable_by_unprotected_control": True,
    }
    print(json.dumps(receipt))
    return receipt


receipt = None
try:
    receipt = main()
finally:
    primary = sys.exc_info()[1]
    errors: list[str] = []
    if primary is not None:
        for identifier in CONTAINERS:
            logs = docker("logs", "--tail", "100", identifier, check=False)
            (WORK / ("network-failure-" + NONCE + "-" + identifier[:12] + ".log")).write_text(
                logs.stdout + logs.stderr
            )
    for identifier in reversed(CONTAINERS):
        docker("rm", "--force", identifier, check=False)
        if docker("container", "inspect", identifier, check=False).returncode == 0:
            errors.append("Owned container remains: " + identifier)
    if NETWORK_CREATED:
        docker("network", "rm", NETWORK, check=False)
        if docker("network", "inspect", NETWORK, check=False).returncode == 0:
            errors.append("Owned network remains: " + NETWORK)
    if IMAGE_CREATED:
        docker("image", "rm", TEST_IMAGE, check=False)
        if docker("image", "inspect", TEST_IMAGE, check=False).returncode == 0:
            errors.append("Owned image remains: " + TEST_IMAGE)
    try:
        shutil.rmtree(TEMP)
    except OSError as error:
        errors.append("Fixture directory cleanup failed: " + str(error))
    if errors:
        print("\n".join(errors), file=sys.stderr)
        if primary is None:
            raise RuntimeError("Owned network fixture cleanup failed")
    elif receipt is not None:
        receipt["cleanup_verified"] = True
        (WORK / "network-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        print("Owned network fixture cleanup verified.")
