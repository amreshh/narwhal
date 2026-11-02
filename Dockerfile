# syntax=docker/dockerfile:1.7
#
# Multi-stage build:
#   1. chef    – Rust toolchain + wasm target, Trunk and cargo-chef
#   2. planner – turns the Cargo manifests into a dependency "recipe"
#   3. build   – compiles dependencies from the recipe (a cached layer that only
#                changes when dependencies do), then the Leptos frontend to WASM
#                with Trunk and the Axum server
#   4. runtime – distroless image with just the binary and static assets (~30 MB)
#
#   docker build -t narwhal:latest .
#   docker run --rm -p 8080:8080 -v "$PWD/data:/data" narwhal:latest

ARG RUST_VERSION=1.97
ARG TRUNK_VERSION=0.22.1
ARG CHEF_VERSION=0.1.78

FROM rust:${RUST_VERSION}-bookworm AS chef
ARG TRUNK_VERSION
ARG CHEF_VERSION
ARG TARGETARCH
WORKDIR /src

RUN rustup target add wasm32-unknown-unknown \
 && case "${TARGETARCH:-amd64}" in \
      amd64) arch=x86_64 ;; \
      arm64) arch=aarch64 ;; \
      *) echo "unsupported arch ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
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
RUN cargo chef cook --release --locked --recipe-path recipe.json -p narwhal-server \
 && cargo chef cook --release --locked --recipe-path recipe.json -p narwhal-frontend \
      --target wasm32-unknown-unknown

COPY Cargo.toml Cargo.lock ./
COPY shared shared
COPY office office
COPY server server
COPY frontend frontend

# The Trunk cache mount keeps wasm-bindgen and wasm-opt between local builds.
RUN --mount=type=cache,target=/root/.cache/trunk \
    cd frontend && trunk build --release && cd .. \
 && cargo build --release --locked -p narwhal-server \
 && mkdir -p /out/data \
 && cp target/release/narwhal /out/narwhal \
 && cp -r frontend/dist /out/static

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
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
ENTRYPOINT ["/app/narwhal"]
