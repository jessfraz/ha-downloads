#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
exec python3 tests/network/run.py "${1:-ha-downloads:smoke}"
