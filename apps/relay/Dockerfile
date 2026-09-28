# Production image for the Railyard RELAY gateway (Cargo binary: `gateway`).
#
# Build context is `apps/relay/` (this directory). The Rust workspace lives in
# `apps/relay/crates`, and the runtime config + default retail catalog live in
# `apps/relay/config`. This is a multi-stage build: a Rust builder compiles
# `cargo build -p gateway --release` against the workspace, then a slim Debian
# runtime carries only CA certs, the compiled binary, and the checked-in config.
#
# Build:   docker build -t railyard-relay -f apps/relay/Dockerfile apps/relay
# Run:     docker run -p 3000:3000 --env-file relay.env railyard-relay
#
# `cargo-chef` is used purely to cache the dependency compilation layer so that
# source-only edits do not rebuild every crate in the workspace.

# Pinned to the workspace toolchain (crates/rust-toolchain.toml -> 1.94.0).
ARG RUST_VERSION=1.94
ARG CARGO_BUILD_FLAGS=""

# ========== chef ==========

FROM lukemathwalker/cargo-chef:latest-rust-${RUST_VERSION}-trixie AS chef
WORKDIR /relay/crates

# ========== planner ==========

FROM chef AS planner
# Only the workspace sources are needed to compute the dependency recipe.
COPY crates/ .
RUN cargo chef prepare --recipe-path recipe.json

# ========== builder ==========

FROM chef AS builder
ARG CARGO_BUILD_FLAGS
ENV CARGO_INCREMENTAL=0
# `clang` / `libc++` are required by some native dependencies in the workspace.
RUN apt-get update \
    && apt-get install -y --no-install-recommends clang libc++-dev \
    && rm -rf /var/lib/apt/lists/*
# Cache-cook dependencies for the `gateway` crate only, then build the binary.
COPY --from=planner /relay/crates/recipe.json recipe.json
COPY crates/rust-toolchain.toml rust-toolchain.toml
RUN cargo chef cook --release -p gateway ${CARGO_BUILD_FLAGS}
COPY crates/ .
# `relay-traces` ships the trace retention job (RY-RL3, OD-RY2) in the same
# image; it reuses the gateway's dependency layer.
RUN cargo build -p gateway -p relay-traces --release ${CARGO_BUILD_FLAGS}

# ========== runtime ==========

FROM debian:trixie-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates openssl wget \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash gateway

# The compiled gateway binary.
COPY --from=builder /relay/crates/target/release/gateway /usr/local/bin/gateway
# Trace retention job, run by a host systemd timer via `docker compose exec`.
COPY --from=builder /relay/crates/target/release/relay-traces-retention /usr/local/bin/relay-traces-retention

# Checked-in runtime config (`tensorzero.toml`) and the default retail catalog
# (`catalog.json`). Secrets are never baked in — they arrive as env vars.
COPY config/ /app/config/

# Apache-2.0 section 4(d): ship the license and the NOTICE (TensorZero
# attribution) with every distributed image.
COPY LICENSE NOTICE /usr/share/doc/relay/

WORKDIR /app
USER gateway

# Fail-closed billing loads the operator price catalog from this path. The
# gateway also serves it over the discovery API. Override to point at a
# different catalog file if the operator publishes their own.
ENV RELAY_CATALOG_PATH=/app/config/catalog.json
ENV RUST_LOG=info

# The gateway binds 0.0.0.0:3000 (see config/tensorzero.toml [gateway]).
EXPOSE 3000

HEALTHCHECK --start-period=15s --start-interval=2s --interval=30s --timeout=3s \
    CMD wget --no-verbose --tries=1 --spider http://localhost:3000/health || exit 1

ENTRYPOINT ["gateway", "--config-file", "/app/config/tensorzero.toml"]
