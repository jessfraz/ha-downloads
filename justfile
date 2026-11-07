set shell := ["bash", "-euo", "pipefail", "-c"]

default: check

rust:
    cargo fmt --check
    cargo clippy --locked --all --benches --tests --examples --all-features -- -D warnings
    cargo test --locked --all-features

packaging:
    uv sync --frozen --only-group dev
    uv run --frozen --only-group dev ruff check scripts/check-public-tree scripts/check-release-version
    uv run --frozen --only-group dev ruff format --check scripts/check-public-tree scripts/check-release-version
    uv run --frozen --only-group dev python scripts/check-public-tree
    uv run --frozen --only-group dev python scripts/check-release-version
    actionlint
    shellcheck scripts/container-smoke.sh scripts/network-test.sh

home-assistant:
    cargo build --locked --bin ha-downloads
    uv sync --project integration --frozen --only-group dev
    uv run --project integration --frozen --only-group dev ruff check --config integration/pyproject.toml custom_components tests_ha
    uv run --project integration --frozen --only-group dev ruff format --check --config integration/pyproject.toml custom_components tests_ha
    PYTHONPATH=. uv run --project integration --frozen --only-group dev pytest -c integration/pyproject.toml tests_ha

check: rust packaging home-assistant

test: check

web:
    node --check web/app.js
    node --test tests_web/panel.mjs

security:
    cargo deny --config .github/deny.toml check advisories licenses sources
    uv audit --locked --python-version 3.12 --python-platform linux

openapi:
    mkdir -p work
    cargo run --locked -- openapi > work/openapi.json

container:
    docker build --tag ha-downloads:smoke .
    ./scripts/container-smoke.sh ha-downloads:smoke "$(docker image inspect ha-downloads:smoke --format '{{{{.Architecture}}')"

network image="ha-downloads:smoke":
    ./scripts/network-test.sh "{{image}}"
