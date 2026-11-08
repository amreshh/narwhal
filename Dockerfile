# syntax=docker/dockerfile:1.7
#
# Multi-stage build:
#   1. chef    – Rust toolchain + wasm and musl targets, musl-gcc, Trunk and cargo-chef
#   2. planner – turns the Cargo manifests into a dependency "recipe"
#   3. build   – compiles dependencies from the recipe (a cached layer that only
#                changes when dependencies do), then the Leptos frontend to WASM
#                with Trunk and the Axum server as a static (musl) binary
#   4. runtime – empty (scratch) image with just the binary and static assets
#
#   docker build -t narwhal:latest .
#   docker run --rm -p 8080:8080 -v "$PWD/data:/data" narwhal:latest
#
# Base images are pinned by digest and written out in full (not through ARGs)
# so Dependabot can update them.

ARG TRUNK_VERSION=0.22.1
ARG CHEF_VERSION=0.1.78

FROM rust:1.99-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c AS chef
ARG TRUNK_VERSION
ARG CHEF_VERSION
ARG TARGETARCH
# A piped command (curl | tar) fails when any part of it fails.
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
WORKDIR /src

# musl-gcc compiles the C code among the server's dependencies (mimalloc) for
# musl. Not pinned to a version: Debian drops a package's old version when it
# ships an update, so a pinned one would stop installing.
# hadolint ignore=DL3008
RUN apt-get update \
 && apt-get install -y --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/*
ENV CC_x86_64_unknown_linux_musl=musl-gcc \
    CC_aarch64_unknown_linux_musl=musl-gcc

# The server's target (x86_64-unknown-linux-musl, …) goes in /rust-target for
# the build stage.
RUN case "${TARGETARCH:-amd64}" in \
      amd64) arch=x86_64 ;; \
      arm64) arch=aarch64 ;; \
      *) echo "unsupported arch ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
 && echo "${arch}-unknown-linux-musl" > /rust-target \
 && rustup target add wasm32-unknown-unknown "${arch}-unknown-linux-musl" \
 && curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-${arch}-unknown-linux-gnu.tar.gz" \
    | tar -xz -C /usr/local/bin trunk \
 && trunk --version \
 && cargo install cargo-chef --locked --version "${CHEF_VERSION}" \
 && rm -rf /usr/local/cargo/registry

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY shared shared
COPY office office
COPY server server
COPY frontend frontend
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
# Dependencies only. The recipe is identical until a Cargo.toml or Cargo.lock
# changes, so in CI this layer comes straight from the registry cache. The
# commands must match the real builds below (profile, target, packages) or the
# cooked dependencies won't be reused.
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json -p narwhal-server --target "$(cat /rust-target)" \
 && cargo chef cook --release --locked --recipe-path recipe.json -p narwhal-frontend \
      --target wasm32-unknown-unknown

COPY Cargo.toml Cargo.lock ./
COPY shared shared
COPY office office
COPY server server
COPY frontend frontend

# The Trunk cache mount keeps wasm-bindgen and wasm-opt between local builds.
WORKDIR /src/frontend
RUN --mount=type=cache,target=/root/.cache/trunk trunk build --release
WORKDIR /src
RUN target=$(cat /rust-target) \
 && cargo build --release --locked -p narwhal-server --target "$target" \
 && mkdir -p /out/data \
 && cp "target/$target/release/narwhal" /out/narwhal \
 && cp -r frontend/dist /out/static

# Empty base: just the server (a static binary) and the frontend. No shell, C
# library, CA certificates, time zone data or /etc/passwd, and the server needs
# none of them: it makes no outbound TLS connections, logs in UTC, has MIME
# types built in and runs as a numeric user. If it ever does need one (e.g.
# CA certificates for an outgoing HTTPS call), copy it in from the build stage.
FROM scratch AS runtime
WORKDIR /app
COPY --from=build /out/narwhal /app/narwhal
COPY --from=build /out/static /app/static
# Default (empty) data dir so the image also runs without a volume.
COPY --from=build --chown=65532:65532 /out/data /data

ENV NARWHAL_ROOT=/data \
    NARWHAL_STATIC_DIR=/app/static \
    RUST_LOG=info,tower_http=info

USER 65532:65532
EXPOSE 8080
# For Docker (Kubernetes uses the probes in deploy/). With no shell or curl in
# the image, the binary asks the running server's /healthz itself.
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 CMD ["/app/narwhal", "healthcheck"]
ENTRYPOINT ["/app/narwhal"]
