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
    python -c "import sabctools, cherrypy, cryptography; import ssl; assert ssl.OPENSSL_VERSION"
'
docker run --rm --network none --cap-drop ALL "$image" --version
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
    --mount "type=volume,source=$volume,target=/data" \
    --publish 127.0.0.1::8080 "$image" >/dev/null
address="http://$(docker port "$container" 8080/tcp)"
python3 - "$address" "$scratch/options.json" <<'PY'
import json
from pathlib import Path
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
echo "Container smoke passed ($expected_arch): packaged binaries, upstream Python imports, missing-config boundary, API authentication and graceful stop."
