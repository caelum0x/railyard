# syntax=docker/dockerfile:1.7
#
# apps/turnstile — production image for the `turnstile-server` binary: the
# AITP-402 paid-tool gateway (HTTP proxy + MCP paywall + operator console)
# that settles every paid call on Mint.
#
# BUILD CONTEXT = the railyard repo ROOT, NOT apps/turnstile. turnstile is its
# own cargo workspace but path-depends on `apps/agent-runtime/crates/aitp-client`
# (the real AITP wire types + signer codec), which lives OUTSIDE apps/turnstile.
# The build therefore recreates both workspaces with their sibling layout.
#
#   docker build -f apps/turnstile/Dockerfile -t railyard/turnstile-server .
#
# Multi-stage:
#   1) builder  — rust:1.94-bookworm compiles `cargo build -p turnstile-server
#                 --release` (agent-runtime pins rustc 1.94.0).
#   2) runtime  — debian:bookworm-slim; only the binary + CA certs. sqlx
#                 (tls-rustls) and reqwest (rustls-tls) need no OpenSSL.
#
# Runtime configuration (see apps/turnstile/README.md):
#   MINT_SERVER_URL, TURNSTILE_API_KEY, TURNSTILE_PAYEE     — Mint rail (required)
#   TURNSTILE_DATABASE_URL                                   — Postgres nonce/settlement journal
#   TURNSTILE_TOOLS_PATH                                     — paid-tool registry JSON (mount it, e.g. /etc/turnstile/tools.json)
#   TURNSTILE_ADMIN_TOKEN                                    — operator console (/admin)
#   BIND_ADDR                                                — default 0.0.0.0:8101

# ---- Stage 1: builder ------------------------------------------------------
FROM rust:1.94-bookworm AS builder

WORKDIR /src/apps/agent-runtime
COPY apps/agent-runtime/Cargo.toml apps/agent-runtime/Cargo.lock ./
COPY apps/agent-runtime/rust-toolchain.toml ./
COPY apps/agent-runtime/crates ./crates

WORKDIR /src/apps/turnstile
COPY apps/turnstile/Cargo.toml apps/turnstile/Cargo.lock ./
COPY apps/turnstile/crates ./crates

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/apps/turnstile/target \
    cargo build --locked --release -p turnstile-server \
    && cp /src/apps/turnstile/target/release/turnstile-server /usr/local/bin/turnstile-server

# ---- Stage 2: runtime ------------------------------------------------------
FROM debian:bookworm-slim AS runtime

ARG VCS_REF=unknown
ARG BUILD_DATE=unknown
LABEL org.opencontainers.image.title="Turnstile" \
      org.opencontainers.image.description="AITP-402 paid-tool gateway (HTTP + MCP) settling on Mint" \
      org.opencontainers.image.revision="$VCS_REF" \
      org.opencontainers.image.created="$BUILD_DATE"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && find /var/log -type f -delete

RUN useradd --system --uid 10001 --user-group --home-dir /app --shell /usr/sbin/nologin turnstile
WORKDIR /app

COPY --from=builder --chown=10001:10001 /usr/local/bin/turnstile-server /usr/local/bin/turnstile-server

USER 10001:10001

ENV BIND_ADDR=0.0.0.0:8101 \
    RUST_LOG=info \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
EXPOSE 8101

HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=6 \
    CMD ["curl", "--fail", "--silent", "--show-error", "--max-time", "2", "http://127.0.0.1:8101/healthz"]

STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/turnstile-server"]
