# syntax=docker/dockerfile:1.7
#
# The Ogun container image: daemon, CLI, server, ingest/sync and operator children, running as a non-root user.
#
# Build context: the directory that holds both checkouts side by side, because Ogun path-depends on zeus-core
# (ADR 0002):
#
#   parent/
#     ogun/        this repository
#     zeus-core/   https://github.com/ArefinAlter/zeus-core
#
#   docker build -f ogun/deploy/Dockerfile -t ogun:dev parent/
#
# The binaries are built from source by default. The release workflow passes BINARIES=prebuilt and puts the static
# musl binaries it already built under prebuilt/<amd64|arm64>/ in the context, so no Rust is compiled under
# emulation. The binaries are selected by name: `ogund` comes from the crate ogun-daemon, `ogun` from ogun-cli,
# `ogun-server` from ogun-server and `ogun-ingest` from ogun-ingest.

ARG BINARIES=source
ARG RUST_VERSION=1
ARG NODE_VERSION=22

# ---- binaries from source ----
FROM rust:${RUST_VERSION}-bookworm AS binaries-source
WORKDIR /src
COPY zeus-core ./zeus-core
COPY ogun ./ogun
WORKDIR /src/ogun
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/ogun/target \
    cargo build --release --locked \
      --bin ogund --bin ogun --bin ogun-server --bin ogun-ingest \
      --bin ogun-sync --bin ogun-op-llm --bin ogun-op-shim \
 && mkdir -p /out \
 && cp target/release/ogund target/release/ogun target/release/ogun-server target/release/ogun-ingest \
       target/release/ogun-sync target/release/ogun-op-llm target/release/ogun-op-shim /out/

# ---- binaries the release workflow built ----
FROM debian:bookworm-slim AS binaries-prebuilt
ARG TARGETARCH
COPY prebuilt/${TARGETARCH}/ /out/
RUN chmod 0755 /out/*

FROM binaries-${BINARIES} AS binaries

# ---- Studio-web, when the checkout has it ----
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-bookworm-slim AS studio
RUN --mount=type=bind,source=ogun,target=/ctx/ogun \
    mkdir -p /studio-web \
 && if [ -f /ctx/ogun/apps/studio/web/package.json ]; then \
      mkdir -p /build && cp -r /ctx/ogun/apps/studio/web/. /build/ \
      && cd /build \
      && (npm ci --no-audit --no-fund || npm install --no-audit --no-fund) \
      && npm run build \
      && cp -r dist/. /studio-web/; \
    fi

# ---- runtime ----
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 ogun \
 && useradd --system --uid 10001 --gid ogun --home-dir /var/lib/ogun --no-create-home --shell /usr/sbin/nologin ogun \
 && install -d -o ogun -g ogun -m 0700 /var/lib/ogun \
 && install -d -m 0755 /etc/ogun /usr/share/ogun
COPY --from=binaries /out/ /usr/local/bin/
COPY --from=studio /studio-web/ /usr/share/ogun/studio-web/
COPY ogun/LICENSE ogun/NOTICE /usr/share/doc/ogun/

# ogund and every client find the state through OGUN_HOME; the server profile lives in profiles/server beneath it,
# with its socket in profiles/server/run. The bridge's own store is a sibling directory.
ENV OGUN_HOME=/var/lib/ogun \
    OGUN_SERVER_HOME=/var/lib/ogun/ogun-server \
    OGUN_SERVER_CONFIG=/etc/ogun/server.json \
    OGUN_LOG=info

USER 10001:10001
WORKDIR /var/lib/ogun
VOLUME ["/var/lib/ogun"]
# 8443: ogun-server with its own TLS; 8080: ogun-server behind a reverse proxy.
EXPOSE 8443 8080
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["ogund", "--profile", "server"]

LABEL org.opencontainers.image.title="Ogun" \
      org.opencontainers.image.description="Ogun daemon, CLI, server bridge and ingest bridge" \
      org.opencontainers.image.vendor="Verne Ltd." \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.source="https://github.com/ArefinAlter/ogun"
