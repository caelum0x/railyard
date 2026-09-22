# syntax=docker/dockerfile:1.7
#
# apps/abacus — production image for the `abacus-proxy` binary: the inline
# reverse proxy that enforces a hard, Mint-receipted per-agent spend budget.
#
# BUILD CONTEXT = the railyard repo ROOT (like apps/vend), NOT apps/abacus.
# abacus is its own standalone cargo workspace, but it path-depends on
# `apps/agent-runtime/crates/aitp-client` (via abacus-mint-client), which lives
# OUTSIDE apps/abacus. The build therefore needs BOTH workspaces on disk with
# their sibling layout (../agent-runtime) preserved.
#
#   docker build -f apps/abacus/Dockerfile -t railyard/abacus-proxy .
#
# Multi-stage:
#   1) builder  — rust:1.94-bookworm compiles `cargo build -p abacus-proxy
#                 --release`. agent-runtime pins rustc 1.94.0 (edition-2024
#                 vendored rig), so the base image tag matches deliberately.
#   2) runtime  — debian:bookworm-slim; ships only the binary + CA certs.
#                 sqlx (tls-rustls) and reqwest (rustls-tls) need no OpenSSL,
#                 only the system CA bundle to verify remote TLS (Postgres, Mint,
#                 upstream providers).
#
# The container serves on :8100 (ABACUS_BIND_ADDR default 0.0.0.0:8100).

# ---- Stage 1: builder ------------------------------------------------------
FROM rust:1.94-bookworm AS builder

# Recreate the two sibling workspaces under /src/apps so the abacus manifest's
# `../agent-runtime/crates/aitp-client` path dep resolves exactly as on disk.
# Copy only the manifests + sources (never target/) so the huge local build dir
# never enters the context layer regardless of .dockerignore placement.
WORKDIR /src/apps/agent-runtime
COPY apps/agent-runtime/Cargo.toml apps/agent-runtime/Cargo.lock ./
COPY apps/agent-runtime/rust-toolchain.toml ./
COPY apps/agent-runtime/crates ./crates

WORKDIR /src/apps/abacus
COPY apps/abacus/Cargo.toml apps/abacus/Cargo.lock ./
COPY apps/abacus/crates ./crates

# Build only the proxy binary in release mode. Cache mounts keep the cargo
# registry and target dir warm across builds when BuildKit is available.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/apps/abacus/target \
    cargo build --locked --release -p abacus-proxy \
    && cp /src/apps/abacus/target/release/abacus-proxy /usr/local/bin/abacus-proxy

# ---- Stage 2: runtime ------------------------------------------------------
FROM debian:bookworm-slim AS runtime

ARG VCS_REF=unknown
ARG BUILD_DATE=unknown
LABEL org.opencontainers.image.title="Abacus" \
      org.opencontainers.image.description="Inline reverse proxy enforcing hard, Mint-receipted per-agent spend budgets" \
      org.opencontainers.image.revision="$VCS_REF" \
      org.opencontainers.image.created="$BUILD_DATE"

# CA certificates for outbound TLS (Mint server, Postgres SSL, upstream
# providers); curl for the container healthcheck.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && find /var/log -type f -delete

# Run as a non-root system user.
RUN useradd --system --uid 10001 --user-group --home-dir /app --shell /usr/sbin/nologin abacus
WORKDIR /app

COPY --from=builder --chown=10001:10001 /usr/local/bin/abacus-proxy /usr/local/bin/abacus-proxy

USER 10001:10001

ENV ABACUS_BIND_ADDR=0.0.0.0:8100 \
    RUST_LOG=info \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
EXPOSE 8100

# /healthz is a static liveness endpoint on the proxy router.
HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=6 \
    CMD ["curl", "--fail", "--silent", "--show-error", "--max-time", "2", "http://127.0.0.1:8100/healthz"]

STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/abacus-proxy"]
