# syntax=docker/dockerfile:1.7
#
# apps/game-economy — production image for the `game-economy-server` binary:
# the game storefront (packs, cosmetics, battle pass, IAP credits) that settles
# every purchase as a hold -> capture on Mint.
#
# BUILD CONTEXT = the railyard repo ROOT, NOT apps/game-economy. game-economy is
# its own cargo workspace but path-depends on `apps/mint/crates/mint-protocol`,
# which inherits `workspace = true` settings from apps/mint/Cargo.toml. The build
# therefore recreates both workspaces with their sibling layout (same approach
# as apps/turnstile). Dockerfile.dockerignore next to this file keeps the root
# context down to the two workspaces.
#
#   docker build -f apps/game-economy/Dockerfile -t railyard/game-economy-server .
#
# Multi-stage:
#   1) builder  — rust:1.94-bookworm compiles `cargo build -p game-economy-server
#                 --release`. The Postgres migrations are embedded at compile
#                 time via `sqlx::migrate!` (runtime query API only, so no live
#                 DATABASE_URL is needed to build).
#   2) runtime  — debian:bookworm-slim; only the binary + CA certs + curl for the
#                 healthcheck. sqlx (tls-rustls) and reqwest (rustls-tls) need no
#                 OpenSSL. Runs as uid 10001.
#
# Arch-agnostic: both base images are multi-arch (amd64 + arm64); nothing here
# downloads an arch-specific artifact, so it builds natively on either.
#
# Runtime configuration (see crates/game-economy-server/src/main.rs):
#   RAILYARD_MINT_URL, MINT_API_KEY, GAME_REVENUE_ACCOUNT, GAME_AUTH_SECRET  (required)
#   GAME_DATABASE_URL        — Postgres state (recommended; migrations run at boot)
#   GAME_CATALOG_PATH        — seed catalog JSON (mount it, e.g. /etc/game-economy/catalog.json)
#   GAME_SERVER_SECRET, GAME_ADMIN_TOKEN, GAME_FULFILLMENT_*, GAME_IAP_*, GAME_REVENUECAT_WEBHOOK_AUTH
#   GAME_BIND_ADDR           — default 0.0.0.0:8102
# Without GAME_DATABASE_URL the single-instance JSON state files land in /app
# (writable by the runtime user); mount a volume there if you rely on them.

# ---- Stage 1: builder ------------------------------------------------------
FROM rust:1.94-bookworm AS builder

# Sibling workspace providing the mint-protocol path dependency. Only manifests
# and sources are needed; mint's own lockfile is irrelevant (game-economy's
# Cargo.lock pins everything).
WORKDIR /src/apps/mint
COPY apps/mint/Cargo.toml ./
COPY apps/mint/crates ./crates
COPY apps/mint/contracts ./contracts

WORKDIR /src/apps/game-economy
COPY apps/game-economy/Cargo.toml apps/game-economy/Cargo.lock ./
COPY apps/game-economy/crates ./crates

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/apps/game-economy/target \
    cargo build --locked --release -p game-economy-server \
    && cp /src/apps/game-economy/target/release/game-economy-server /usr/local/bin/game-economy-server

# ---- Stage 2: runtime ------------------------------------------------------
FROM debian:bookworm-slim AS runtime

ARG VCS_REF=unknown
ARG BUILD_DATE=unknown
LABEL org.opencontainers.image.title="Game Economy" \
      org.opencontainers.image.description="Game storefront (packs, cosmetics, battle pass, IAP) settling on Mint" \
      org.opencontainers.image.revision="$VCS_REF" \
      org.opencontainers.image.created="$BUILD_DATE"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && find /var/log -type f -delete

RUN useradd --system --uid 10001 --user-group --home-dir /app --shell /usr/sbin/nologin gameeco \
    && install -d -o 10001 -g 10001 -m 750 /app
WORKDIR /app

COPY --from=builder --chown=10001:10001 /usr/local/bin/game-economy-server /usr/local/bin/game-economy-server

USER 10001:10001

ENV GAME_BIND_ADDR=0.0.0.0:8102 \
    RUST_LOG=info \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
EXPOSE 8102

HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=6 \
    CMD ["curl", "--fail", "--silent", "--show-error", "--max-time", "2", "http://127.0.0.1:8102/healthz"]

STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/game-economy-server"]
