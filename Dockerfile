# Each base is pinned to an upstream multiarch index (amd64 and arm64).
FROM ghcr.io/astral-sh/uv:0.13.6@sha256:af22a52f5eafef3d78e551599d405c02df61919d18028f0cea8eda93bc9986d0 AS uv
FROM rust:1.99.1-alpine3.22@sha256:0fd356c1b3a83a6a6a2a4113ede8166eac6697a663fa822c04f0c202e63fcc01 AS native-build

RUN apk add --no-cache build-base pkgconf python3 python3-dev libffi-dev openssl-dev curl
FROM native-build AS python-build
COPY --from=uv /uv /uvx /usr/local/bin/
WORKDIR /build/python
COPY pyproject.toml uv.lock ./
ENV UV_PROJECT_ENVIRONMENT=/opt/venv UV_PYTHON_DOWNLOADS=never UV_LINK_MODE=copy
RUN uv sync --frozen --no-dev --no-install-project --python /usr/bin/python3

ARG SABNZBD_VERSION=5.1.3
ARG SABNZBD_SHA256=20f2d0dfbefe05164235a61617693643e61b743ec8a6201a09a8659ef7b554ad
RUN curl --fail --location --retry 3 --output /tmp/sabnzbd.tar.gz \
      "https://codeload.github.com/sabnzbd/sabnzbd/tar.gz/refs/tags/${SABNZBD_VERSION}" \
    && echo "${SABNZBD_SHA256}  /tmp/sabnzbd.tar.gz" | sha256sum -c - \
    && mkdir /opt/sabnzbd \
    && tar -xzf /tmp/sabnzbd.tar.gz --strip-components=1 -C /opt/sabnzbd \
    && rm /tmp/sabnzbd.tar.gz \
    && /opt/venv/bin/python /opt/sabnzbd/SABnzbd.py --version \
    && PYTHONPATH=/opt/sabnzbd /opt/venv/bin/python -c \
      "import sabnzbd.decoder; assert sabnzbd.decoder.SABCTOOLS_ENABLED"

# Alpine does not package RARLAB's UnRAR. Build the unmodified official source
# with the same musl toolchain, and retain its redistribution license.
ARG UNRAR_VERSION=7.3.2
ARG UNRAR_SHA256=f61a1160d6e2ebbda2ad353c8e82f62ab4312f97fde23c6c2c982d25e0014f99
RUN curl --fail --location --retry 3 --output /tmp/unrar.tar.gz \
      "https://www.rarlab.com/rar/unrarsrc-${UNRAR_VERSION}.tar.gz" \
    && echo "${UNRAR_SHA256}  /tmp/unrar.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/unrar.tar.gz -C /tmp \
    && make -C /tmp/unrar -j2 \
    && install -D /tmp/unrar/unrar /opt/bin/unrar \
    && install -Dm644 /tmp/unrar/license.txt /opt/licenses/unrar.txt \
    && rm -rf /tmp/unrar /tmp/unrar.tar.gz
RUN curl --fail --location --retry 3 --output /opt/licenses/gluetun.txt \
      https://raw.githubusercontent.com/passteque/gluetun/v3.41.3/LICENSE \
    && echo "081820bd14b0013f5b7868ea217caf2c3186d9af67bfd49ab02639c258384b45  /opt/licenses/gluetun.txt" | sha256sum -c -

FROM native-build AS build
WORKDIR /build/app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
# The Alpine Rust toolchain builds a native musl binary for each target runner.
RUN cargo build --locked --release --bin downloads

FROM qmcgaw/gluetun:v3.41.3@sha256:fa19cc76b2af13d57a8d3dc3066f2ada061b1c761b8aecf989b3877c0486e027
ARG BUILD_VERSION=dev
LABEL org.opencontainers.image.title="Downloads" \
      org.opencontainers.image.description="SABnzbd behind Gluetun in one private network namespace" \
      org.opencontainers.image.source="https://github.com/jessfraz/ha-downloads" \
      org.opencontainers.image.licenses="MIT AND GPL-2.0-or-later" \
      io.hass.name="Downloads" io.hass.version="${BUILD_VERSION}" \
      io.hass.type="app" io.hass.arch="aarch64|amd64"
RUN apk add --no-cache python3 par2cmdline 7zip libstdc++ \
    && mkdir -p /data /run/downloads /usr/share/licenses/downloads \
    && chmod 0700 /run/downloads
COPY --from=python-build /opt/venv /opt/venv
COPY --from=python-build /opt/sabnzbd /opt
COPY --from=python-build /opt/bin/unrar /usr/local/bin/unrar
COPY --from=python-build /opt/licenses/ /usr/share/licenses/downloads/
COPY --from=build /build/app/target/release/downloads /usr/local/bin/downloads
COPY LICENSE NOTICE /usr/share/licenses/downloads/
ENV PATH="/opt/venv/bin:${PATH}" PYTHONUNBUFFERED=1 PYTHONDONTWRITEBYTECODE=1
WORKDIR /data
EXPOSE 8099/tcp 8080/tcp 8888/tcp
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget --quiet --output-document=/dev/null http://127.0.0.1:8099/healthz || exit 1
ENTRYPOINT ["/usr/local/bin/downloads"]
CMD ["serve"]
