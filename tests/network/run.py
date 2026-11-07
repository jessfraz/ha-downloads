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
BASE_TAG = "ha-downloads:network-base-" + NONCE
CONTAINERS = []
NETWORK_CREATED = False
IMAGE_CREATED = False
BASE_ID = ""
BASE_TAG_CREATED = False
WORK = ROOT / "work"
WORK.mkdir(exist_ok=True)
TEMP = Path(tempfile.mkdtemp(prefix="network-", dir=WORK))
os.chmod(TEMP, 0o700)
(TEMP / ".owner").write_text(NONCE)
(TEMP / ".owner").chmod(0o600)


def run(
    *args: str, check: bool = True, capture: bool = True, cwd: Path | None = None
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        args, check=False, capture_output=capture, text=True, timeout=300, cwd=cwd
    )
    if check and result.returncode:
        if capture:
            print(result.stdout, file=sys.stderr)
            print(result.stderr, file=sys.stderr)
        result.check_returncode()
    return result


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


def app_health(app: str) -> int:
    response = execute(
        app,
        "/opt/venv/bin/python",
        "-c",
        """import urllib.error,urllib.request
try:
 with urllib.request.urlopen('http://127.0.0.1:8099/healthz',timeout=5) as response:
  print(response.status)
except urllib.error.HTTPError as error:
 print(error.code)
""",
    )
    return int(response.stdout)


def start_vpn(server: str) -> None:
    execute(
        server,
        "/usr/sbin/openvpn2.6",
        "--config",
        "/fixture/server.conf",
        "--daemon",
        "--log",
        "/tmp/test-vpn.log",
        "--writepid",
        "/tmp/test-vpn.pid",
    )


def is_state(app: str, expected: str) -> bool:
    state = app_status(app)
    if state.get("state") == "degraded" and state.get("downloader", {}).get("state") == "stopped":
        raise RuntimeError("Production runtime stopped its protected children: " + str(state))
    return state.get("state") == expected


def main() -> dict[str, object]:
    global NETWORK_CREATED, IMAGE_CREATED, BASE_ID, BASE_TAG_CREATED
    BASE_ID = docker("image", "inspect", IMAGE, "--format", "{{.Id}}").stdout.strip()
    if docker("image", "inspect", BASE_TAG, check=False).returncode == 0:
        raise RuntimeError("Refusing an existing fixture base tag")
    docker("tag", BASE_ID, BASE_TAG)
    BASE_TAG_CREATED = True
    for name in ("fixture.py", "probe.py", "Dockerfile"):
        shutil.copyfile(ROOT / "tests/network" / name, TEMP / name)
    # Only generated test PKI goes into a disposable, unpublished test image.
    san = (
        "subjectAltName=DNS:cloudflare-dns.com,DNS:cloudflare.com,DNS:github.com,"
        "DNS:example.com,DNS:origin.test,DNS:api.ipify.org,DNS:ipinfo.io,"
        "DNS:ifconfig.co,DNS:api.ip2location.io,DNS:www.cloudflare.com,"
        "DNS:raw.githubusercontent.com,DNS:cdn.jsdelivr.net,IP:198.18.0.3"
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
        (TEMP / (role + ".ext")).write_text(
            "basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\n"
            + "extendedKeyUsage="
            + role
            + "Auth\n"
        )
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
        "build",
        "--build-arg",
        "APP_IMAGE=" + BASE_TAG,
        "--tag",
        TEST_IMAGE,
        str(TEMP),
        capture=False,
    )
    IMAGE_CREATED = True
    docker(
        "network",
        "create",
        "--label",
        LABEL,
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
push "route 198.18.0.3 255.255.255.255"
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
    data.mkdir(mode=0o700)
    (data / "openvpn").mkdir(mode=0o700)
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
ip addr add 198.18.0.3/32 dev eth0
ip -6 addr add 2001:db8:1234::3/128 dev eth0
iptables -P FORWARD DROP
iptables -P OUTPUT DROP
iptables -A OUTPUT -o lo -j ACCEPT
iptables -A OUTPUT -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
iptables -A OUTPUT -d 198.19.0.0/24 -j ACCEPT
iptables -A OUTPUT -o tun0 -j ACCEPT
iptables -t nat -A PREROUTING -i tun0 -p udp --dport 53 -j DNAT --to-destination 10.8.0.1:53
iptables -t nat -A PREROUTING -i tun0 -p tcp --dport 53 -j DNAT --to-destination 10.8.0.1:53
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
        "ip route replace default via 198.19.0.2; "
        "ip -6 route replace default via fd12:3456:789a::3; exec sleep 600",
    )
    # The IPv4 listener can be ready before the fixture's IPv6 route is usable.
    # Require every unprotected control path before starting the protected app.
    wait(
        lambda: (
            execute(
                control, "/opt/venv/bin/python", "/fixture/probe.py", "control", check=False
            ).returncode
            == 0
        ),
        "unprotected IPv4, IPv6 and DNS control paths",
        seconds=30,
    )
    # Native Linux bind mounts retain the host UID. Transfer only this fixture's
    # two generated files and their private directories to the app's root user.
    helper = docker(
        "create",
        "--label",
        LABEL,
        "--network",
        "none",
        "--user",
        "0:0",
        "-v",
        f"{TEMP}:/fixture",
        "--entrypoint",
        "/opt/venv/bin/python",
        TEST_IMAGE,
        "-c",
        """import json,os,pathlib,stat,sys
root=pathlib.Path('/fixture');marker=root/'.owner'
assert stat.S_ISREG(marker.lstat().st_mode) and marker.lstat().st_nlink==1
assert marker.read_text()==sys.argv[1]
data=root/'data'
initial_uid=(data/'openvpn/custom.conf').lstat().st_uid
expected={'openvpn','openvpn/custom.conf','options.json'}
assert {str(path.relative_to(data)) for path in data.rglob('*')}==expected
for relative in ('data','data/openvpn','data/openvpn/custom.conf','data/options.json'):
 path=root/relative;metadata=path.lstat()
 directory=relative in ('data','data/openvpn')
 assert stat.S_ISDIR(metadata.st_mode) if directory else (
  stat.S_ISREG(metadata.st_mode) and metadata.st_nlink==1)
 assert stat.S_IMODE(metadata.st_mode)==(0o700 if directory else 0o600)
 os.chown(path,0,0,follow_symlinks=False)
 assert path.lstat().st_uid==0 and path.lstat().st_gid==0
print(json.dumps({'initial_uid':initial_uid,'root_owned':True}))
""",
        NONCE,
    ).stdout.strip()
    CONTAINERS.append(helper)
    ownership = json.loads(docker("start", "--attach", helper).stdout)
    assert ownership["root_owned"] is True
    print(
        json.dumps(
            {
                "generated_data_initial_uid": ownership["initial_uid"],
                "generated_data_root_ownership_verified": True,
            }
        ),
        flush=True,
    )
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
        "ip route replace default via 198.19.0.2; "
        "ip -6 route replace default via fd12:3456:789a::3; exec /usr/local/bin/downloads serve",
    )
    wait(lambda: is_state(app, "starting"), "runtime waits for VPN")
    assert app_health(app) == 200
    sab_pids = """import json,pathlib
pids=[]
for path in pathlib.Path('/proc').glob('[0-9]*/cmdline'):
 try: arguments=path.read_bytes().split(bytes([0]))
 except (FileNotFoundError,ProcessLookupError): continue
 if b'/opt/SABnzbd.py' in arguments:pids.append(int(path.parent.name))
print(json.dumps(pids))
"""
    assert json.loads(execute(app, "/opt/venv/bin/python", "-c", sab_pids).stdout) == []
    start_vpn(server)
    wait(
        lambda: is_state(app, "ready"),
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
    assert app_health(app) == 200
    up = json.loads(execute(app, "/opt/venv/bin/python", "/fixture/probe.py", "up").stdout)
    execute(control, "/opt/venv/bin/python", "/fixture/probe.py", "proxy-up")
    # SAB owns the pause state. Do not test a lookalike process or bypass the real HTTP API.
    sab_api = """import json,urllib.parse,urllib.request
from configobj import ConfigObj
c=ConfigObj('/data/sabnzbd/sabnzbd.ini');key=c['misc']['api_key']
base='http://127.0.0.1:8081/sabnzbd/api?'
def call(mode, **params):
 query=urllib.parse.urlencode({'apikey':key,'mode':mode,'output':'json',**params})
 return json.load(urllib.request.urlopen(base+query))
"""
    execute(
        app,
        "/opt/venv/bin/python",
        "-c",
        sab_api
        + "assert call('queue')['queue']['paused'] is False, 'Default queue was forced paused'\n"
        + "call('pause');assert call('queue')['queue']['paused'] is True",
    )
    # Failed answers remain a visible DNS failure without inventing a direct resolver fallback.
    execute(server, "touch", "/tmp/dns-fail")
    wait(
        lambda: app_status(app).get("protection", {}).get("dns") == "failed", "DNS failure reported"
    )
    dns_failure = app_status(app)
    assert dns_failure["state"] == "degraded", dns_failure
    assert dns_failure["downloader"]["state"] == "running", dns_failure
    assert app_health(app) == 200
    execute(server, "rm", "/tmp/dns-fail")
    wait(lambda: is_state(app, "ready"), "DNS answers recover")
    # A stopped upstream is a real tunnel outage. No namespace or production firewall is edited.
    execute(server, "/bin/sh", "-c", "kill -TERM $(cat /tmp/test-vpn.pid)")
    wait(
        lambda: app_status(app).get("state") == "degraded",
        "runtime observes tunnel outage",
        seconds=90,
    )
    assert app_health(app) == 200
    down = json.loads(execute(app, "/opt/venv/bin/python", "/fixture/probe.py", "down").stdout)
    execute(control, "/opt/venv/bin/python", "/fixture/probe.py", "control")
    execute(control, "/opt/venv/bin/python", "/fixture/probe.py", "proxy-down")
    start_vpn(server)
    wait(lambda: is_state(app, "ready"), "VPN reconnects without resuming queue")
    assert app_health(app) == 200
    execute(
        app,
        "/opt/venv/bin/python",
        "-c",
        sab_api + "assert call('queue')['queue']['paused'] is True",
    )
    # A running queue must also remain running across an ordinary tunnel
    # reconnect. The runtime must not replace SAB or manufacture a pause.
    execute(
        app,
        "/opt/venv/bin/python",
        "-c",
        sab_api + "call('resume');assert call('queue')['queue']['paused'] is False",
    )
    unpaused_sab = json.loads(execute(app, "/opt/venv/bin/python", "-c", sab_pids).stdout)
    assert len(unpaused_sab) == 1, unpaused_sab
    print(json.dumps({"running_queue_reconnect_started": True}), flush=True)
    execute(server, "/bin/sh", "-c", "kill -TERM $(cat /tmp/test-vpn.pid)")
    wait(
        lambda: app_status(app).get("state") == "degraded",
        "runtime observes tunnel outage with running queue",
        seconds=90,
    )
    assert app_health(app) == 200
    start_vpn(server)
    wait(lambda: is_state(app, "ready"), "VPN reconnects with running queue")
    assert app_health(app) == 200
    execute(
        app,
        "/opt/venv/bin/python",
        "-c",
        sab_api + "assert call('queue')['queue']['paused'] is False",
    )
    assert json.loads(execute(app, "/opt/venv/bin/python", "-c", sab_pids).stdout) == unpaused_sab
    print(json.dumps({"running_queue_recovered": True}), flush=True)
    execute(app, "/opt/venv/bin/python", "/fixture/probe.py", "up")
    # Set the next-start policy through SAB's native API without pausing the
    # current queue. The same policy must survive terminal cleanup and restart.
    execute(
        app,
        "/opt/venv/bin/python",
        "-c",
        sab_api
        + "policy=call('set_config',section='misc',keyword='start_paused',value='1')\n"
        + "assert policy['config']['misc']['start_paused'] == 1\n"
        + "assert call('queue')['queue']['paused'] is False",
    )
    # Abrupt loss of Gluetun must stop SAB; it cannot leave a downloader behind.
    execute(
        app,
        "/opt/venv/bin/python",
        "-c",
        """import os,pathlib,signal
processes={}
for path in pathlib.Path('/proc').glob('[0-9]*/cmdline'):
 try: arguments=path.read_bytes().split(bytes([0]))
 except FileNotFoundError: continue
 if arguments and arguments[0]:
  processes.setdefault(pathlib.Path(os.fsdecode(arguments[0])).name,[]).append(int(path.parent.name))
[glt]=processes['gluetun-entrypoint'];[vpn]=processes['openvpn2.6']
assert os.getpgid(vpn)!=os.getpgid(glt)
# A stopped child cannot handle TERM, so production cleanup must escalate and reap it.
os.kill(vpn,signal.SIGSTOP);os.kill(glt,signal.SIGKILL)
""",
    )
    wait(
        # Stopping already marks the downloader stopped. Read both fields from
        # one snapshot so an intermediate transition cannot finish this wait.
        lambda: (
            (snapshot := app_status(app)).get("state") == "degraded"
            and snapshot.get("downloader", {}).get("state") == "stopped"
        ),
        "completed child-exit cleanup",
    )
    terminal = app_status(app)
    assert terminal["diagnostics"]["checked_at_unix_ms"] is None, terminal
    checks = terminal["diagnostics"]["checks"]
    assert checks and all(check["outcome"] == "not_checked" for check in checks.values()), terminal
    assert app_health(app) == 503
    remaining_sab = json.loads(execute(app, "/opt/venv/bin/python", "-c", sab_pids).stdout)
    assert remaining_sab == [], {"sab_pids": remaining_sab, "status": app_status(app)}
    children = json.loads(
        execute(
            app,
            "/opt/venv/bin/python",
            "-c",
            """import json,pathlib
children=[]
for path in pathlib.Path('/proc').glob('[0-9]*/status'):
 try: fields=dict(line.split(':',1) for line in path.read_text().splitlines() if ':' in line)
 except FileNotFoundError: continue
 if fields.get('PPid','').strip()=='1':
  children.append({key:fields[key].strip() for key in ('Name','Pid','PPid','State')})
print(json.dumps(children))
""",
        ).stdout
    )
    assert children == [], children
    # Restart the same production app with SAB's persisted startup preference.
    docker("restart", "--time", "15", app)
    wait(lambda: is_state(app, "ready"), "restart honors native startup preference")
    assert app_health(app) == 200
    execute(
        app,
        "/opt/venv/bin/python",
        "-c",
        sab_api + "assert call('queue')['queue']['paused'] is True",
    )
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
        "image_id": BASE_ID,
        "dns_failure_reported_without_direct_fallback": True,
        "local_vpn": "/usr/sbin/openvpn2.6",
        "up": up,
        "outage": down,
        "operator_pause_preserved": True,
        "running_queue_preserved_across_tunnel_recovery": True,
        "native_default_start_unpaused": True,
        "native_start_paused_preserved_after_restart": True,
        "child_exit_stopped_downloader": True,
        "terminal_failure_unhealthy": True,
        "terminal_checks_invalidated": True,
        "recoverable_states_remain_healthy": True,
        "separate_group_stopped_openvpn_killed_and_reaped": True,
        "cold_start_gated": True,
        "generated_data_root_ownership_verified": True,
        "generated_data_initial_uid": ownership["initial_uid"],
        "dns_tls_and_origin_observed_through_tunnel": True,
        "http_connect_proxy_up_and_failclosed": True,
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
    for identifier in CONTAINERS:
        docker("stop", "--time", "5", identifier, check=False)
    # Linux bind mounts retain the container's UID. Remove only generated SAB data
    # through a tracked, networkless helper before deleting its private host parent.
    if IMAGE_CREATED and (TEMP / "data").exists():
        helper = docker(
            "create",
            "--label",
            LABEL,
            "--network",
            "none",
            "-v",
            f"{TEMP}:/fixture",
            "--entrypoint",
            "/opt/venv/bin/python",
            TEST_IMAGE,
            "-c",
            "import pathlib,shutil,sys;root=pathlib.Path('/fixture');"
            "assert (root/'.owner').read_text()==sys.argv[1];"
            "shutil.rmtree(root/'data')",
            NONCE,
            check=False,
        )
        if helper.returncode == 0:
            identifier = helper.stdout.strip()
            CONTAINERS.append(identifier)
            if docker("start", "--attach", identifier, check=False).returncode != 0:
                errors.append("Generated app data cleanup failed")
        else:
            errors.append("Cannot create owned data cleanup helper")
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
    if BASE_TAG_CREATED:
        docker("image", "rm", BASE_TAG, check=False)
        if docker("image", "inspect", BASE_TAG, check=False).returncode == 0:
            errors.append("Owned base tag remains: " + BASE_TAG)
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
