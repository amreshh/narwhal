# syntax=docker/dockerfile:1.7
#
# Multi-stage build:
#   1. build  – compiles the Leptos frontend to WASM with Trunk and the Axum server
#   2. runtime – distroless image with just the binary and static assets (~30 MB)
#
#   docker build -t narwhal:latest .
#   docker run --rm -p 8080:8080 -v "$PWD/data:/data" narwhal:latest

ARG RUST_VERSION=1.97
ARG TRUNK_VERSION=0.22.1

FROM rust:${RUST_VERSION}-bookworm AS build
ARG TRUNK_VERSION
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
 && trunk --version

COPY Cargo.toml Cargo.lock ./
COPY shared shared
COPY office office
COPY server server
COPY frontend frontend

# Cache mounts keep crates.io downloads, Trunk's tool cache (wasm-bindgen,
# wasm-opt) and incremental build output between builds.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/root/.cache/trunk \
    --mount=type=cache,target=/src/target \
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
