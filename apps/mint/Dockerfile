# syntax=docker/dockerfile:1
#
# apps/mint — production image for the `mint-server` binary (AIT settlement layer).
#
# Multi-stage:
#   1) builder  — rust:1.94-bookworm compiles `cargo build -p mint-server --release`.
#                 The Postgres migrations under crates/mint-store-postgres/migrations
#                 are embedded into the binary at compile time via `sqlx::migrate!`,
#                 so no migration files are needed in the runtime image.
#   2) runtime  — debian:bookworm-slim; ships only the static-ish binary + CA certs.
#                 sqlx (tls-rustls) and reqwest (rustls-tls) need no OpenSSL, only
#                 the system CA bundle to verify remote TLS (Postgres, providers).
#
# The container serves on :8080 and runs migrations at startup (PgStore path when
# DATABASE_URL is set).

# ---- Stage 1: builder ------------------------------------------------------
FROM rust:1.94-bookworm AS builder

WORKDIR /build

# Copy the whole standalone workspace. Path deps (crates/*) and the embedded
# migrations dir must all be present for a correct compile.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY contracts ./contracts

# Build only the server binary in release mode. A cache mount keeps the cargo
# registry and target dir warm across builds when BuildKit is available.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build -p mint-server --release \
    && cp /build/target/release/mint-server /usr/local/bin/mint-server

# ---- Stage 2: runtime ------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# CA certificates for outbound TLS (Postgres SSL, payment providers).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Run as a non-root user.
RUN useradd --system --uid 10001 --user-group --home-dir /app --shell /usr/sbin/nologin mint
WORKDIR /app

COPY --from=builder /usr/local/bin/mint-server /usr/local/bin/mint-server

USER mint

ENV MINT_BIND_ADDR=0.0.0.0:8080
EXPOSE 8080

# The binary runs embedded migrations at startup (when DATABASE_URL is set) and
# then serves. No shell wrapper needed.
ENTRYPOINT ["/usr/local/bin/mint-server"]
