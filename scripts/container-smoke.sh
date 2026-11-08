#!/usr/bin/env bash
set -euo pipefail

image=${1:?Usage: container-smoke.sh IMAGE EXPECTED_ARCH}
expected_arch=${2:?Usage: container-smoke.sh IMAGE EXPECTED_ARCH}
scratch=$(mktemp -d)
container="downloads-smoke-$(basename "$scratch" | tr '[:upper:]' '[:lower:]')"
volume="$container-data"
cleanup() {
    docker rm --force "$container" >/dev/null 2>&1 || true
    docker volume rm "$volume" >/dev/null 2>&1 || true
    rm -rf "$scratch"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

test "$(docker image inspect "$image" --format '{{.Architecture}}')" = "$expected_arch"
# Execute every packaged runtime, with WAN networking disabled. These commands
# cannot start a provider tunnel or submit a download.
docker run --rm --network none --cap-drop ALL --entrypoint /bin/sh "$image" -ec '
    /gluetun-entrypoint genkey >/dev/null
    python /opt/SABnzbd.py --version | grep -F "5.1.3"
    unrar > /dev/null
    par2 -V
    7zz i > /dev/null
    mkdir /tmp/archive-smoke
    cd /tmp/archive-smoke
    printf "Downloads archive check\n" > payload
    par2 create -q payload.par2 payload > /dev/null
    par2 verify -q payload.par2 > /dev/null
    7zz a payload.7z payload > /dev/null
    7zz t payload.7z > /dev/null
    PYTHONPATH=/opt python -c "import sabnzbd.decoder, cherrypy, cryptography; import ssl; assert ssl.OPENSSL_VERSION; assert sabnzbd.decoder.SABCTOOLS_ENABLED, \"SAB native decoder is unavailable\""
'
docker run --rm --network none --cap-drop ALL "$image" --version
docker run --rm --interactive --network none --cap-drop ALL --entrypoint python "$image" - <<'PY'
import certifi
import jwt
from oauthlib.oauth2 import WebApplicationClient
from requests_oauthlib import OAuth2Session
import secrets
import ssl
import urllib3

ssl.create_default_context(cafile=certifi.where())
pool = urllib3.PoolManager(cert_reqs="CERT_REQUIRED", ca_certs=certifi.where())
assert pool.connection_from_url("https://downloads.example/api").host == "downloads.example"
token = {"access_token": secrets.token_hex(16), "token_type": "Bearer"}
client = WebApplicationClient("downloads-fixture", token=token)
assert client.add_token("https://downloads.example/api")[1]["Authorization"] == "Bearer " + token["access_token"]
assert OAuth2Session("downloads-fixture", token=token).authorized
key = secrets.token_bytes(32)
encoded = jwt.encode({"sub": "downloads-fixture"}, key, algorithm="HS256")
assert jwt.decode(encoded, key, algorithms=["HS256"])["sub"] == "downloads-fixture"
PY
docker run --rm --network none --cap-drop ALL "$image" openapi > "$scratch/openapi.json"
python3 - "$scratch" <<'PY'
import json
from pathlib import Path
import secrets
import sys

root = Path(sys.argv[1])
schema = json.loads((root / "openapi.json").read_text())
assert schema["openapi"].startswith("3.")
assert "/api/v1/status" in schema["paths"]
options = {
    "vpn": {"provider": "", "protocol": "openvpn", "username": "", "password": "",
            "wireguard_private_key": "", "wireguard_addresses": "", "countries": ""},
    "network": {"local_subnets": ["172.30.32.0/23"]},
    "api_token": secrets.token_hex(32),
}
(root / "options.json").write_text(json.dumps(options))
(root / "options.json").chmod(0o600)
PY
docker volume create "$volume" >/dev/null
docker run --rm --interactive --network none --cap-drop ALL --entrypoint /bin/sh \
    --mount "type=volume,source=$volume,target=/data" "$image" \
    -ec 'umask 077; cat > /data/options.json' < "$scratch/options.json"
docker run --detach --name "$container" --cap-drop ALL \
    --env SUPERVISOR_TOKEN=fixture-supervisor-token \
    --mount "type=volume,source=$volume,target=/data" \
    --publish 127.0.0.1::8080 "$image" >/dev/null
address="http://$(docker port "$container" 8080/tcp)"
python3 - "$address" "$scratch/options.json" "$container" <<'PY'
import json
from pathlib import Path
import subprocess
import sys
import time
import urllib.error
import urllib.request

address = sys.argv[1]
token = json.loads(Path(sys.argv[2]).read_text())["api_token"]
deadline = time.monotonic() + 30
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
while True:
    try:
        request = urllib.request.Request(address + "/api/v1/status", headers={"Authorization": "Bearer " + token})
        with opener.open(request, timeout=2) as response:
            result = json.load(response)
        break
    except (OSError, urllib.error.URLError):
        if time.monotonic() >= deadline:
            raise
        time.sleep(0.25)
assert result["state"] == "needs_configuration"
assert result["downloader"]["state"] == "stopped"
assert result["protection"]["kill_switch"] != "verified"
try:
    opener.open(address + "/api/v1/status", timeout=2)
except urllib.error.HTTPError as error:
    assert error.code == 401
else:
    raise AssertionError("Direct status accepted an unauthenticated client")

container = sys.argv[3]
credential_path = "/data/media/integration-token"
integration = subprocess.check_output(
    ["docker", "exec", container, "cat", credential_path], text=True
).strip()
assert len(integration) == 64 and all(c in "0123456789abcdef" for c in integration)
assert subprocess.check_output(
    ["docker", "exec", container, "stat", "-c", "%a:%u", credential_path], text=True
).strip() == "600:0"
assert subprocess.check_output(
    ["docker", "exec", container, "stat", "-c", "%a:%u", "/data/media"], text=True
).strip() == "700:0"

def integration_system():
    request = urllib.request.Request(
        address + "/api/v1/system", headers={"Authorization": "Bearer " + integration}
    )
    with opener.open(request, timeout=2) as response:
        system = json.load(response)
    assert system["credential_role"] == "integration"
    assert system["capabilities"] == {"read": True, "request": False, "admin": False}

integration_system()
request = urllib.request.Request(
    address + "/api/v1/profiles",
    data=json.dumps({"kind": "movie", "name": "Fixture", "is_default": True,
                     "settings": {"quality_profile_id": 1, "root_folder_id": 1,
                                  "tags": [], "minimum_availability": "released"}}).encode(),
    headers={"Authorization": "Bearer " + integration,
             "X-Downloads-Actor-Id": "smoke", "X-Downloads-Actor-Can-Request": "true",
             "Content-Type": "application/json", "Idempotency-Key": "smoke-profile"},
    method="POST",
)
try:
    opener.open(request, timeout=2)
except urllib.error.HTTPError as error:
    assert error.code == 403
else:
    raise AssertionError("Generated integration credential gained administration")

subprocess.check_output(["docker", "stop", "--time", "15", container])
assert subprocess.check_output(
    ["docker", "inspect", container, "--format", "{{.State.ExitCode}}"], text=True
).strip() == "0"
subprocess.check_output(["docker", "start", container])
address = "http://" + subprocess.check_output(
    ["docker", "port", container, "8080/tcp"], text=True
).strip()
deadline = time.monotonic() + 30
while True:
    try:
        integration_system()
        break
    except (OSError, urllib.error.URLError):
        if time.monotonic() >= deadline:
            raise
        time.sleep(0.25)
assert subprocess.check_output(
    ["docker", "exec", container, "cat", credential_path], text=True
).strip() == integration, "App restart replaced its pairing credential"
logs = subprocess.check_output(["docker", "logs", container], stderr=subprocess.STDOUT, text=True)
assert integration not in logs, "Generated pairing credential escaped into logs"
PY
docker exec "$container" wget --quiet --output-document=/dev/null http://127.0.0.1:8099/healthz
docker exec "$container" /bin/sh -ec '! ps | grep -E "[g]luetun-entrypoint|[S]ABnzbd.py"'
docker stop --time 15 "$container" >/dev/null
test "$(docker inspect "$container" --format '{{.State.ExitCode}}')" = 0
docker logs "$container" > "$scratch/logs" 2>&1
python3 - "$scratch" <<'PY'
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
token = json.loads((root / "options.json").read_text())["api_token"]
assert token not in (root / "logs").read_text(), "API token escaped into container logs"
PY
echo "Container smoke passed ($expected_arch): packaged binaries, upstream Python imports, private pairing and restart, API authentication and graceful stop."
