# syntax=docker/dockerfile:1.7
#
# apps/context-passport — production image for the `identity-lock-server`
# binary (package `passport-server`): the Identity Lock / Context Passport
# API (create/edit/lock/version a brand or character identity, resolve it
# per-provider, generate through Relay with signed Mint settlement evidence
# and C2PA-aligned provenance).
#
# BUILD CONTEXT = the railyard repo ROOT, NOT apps/context-passport.
# context-passport is its own cargo workspace but path-depends on
# `apps/mint/crates/mint-protocol` (and mint-protocol's own siblings
# mint-core/mint-store/mint-receipts/mint-ledger), which live OUTSIDE
# apps/context-passport. The build therefore recreates both workspaces with
# their sibling layout, same pattern as apps/turnstile's Dockerfile.
#
#   docker build -f apps/context-passport/Dockerfile -t railyard/identity-lock-server .
#
# Runtime is distroless (CA bundle only); sqlx and reqwest use rustls, so
# there is no OpenSSL. Serves on :8095 by default (PASSPORT_BIND_ADDR).

FROM rust:1.94-bookworm AS builder

WORKDIR /src/apps/mint
COPY apps/mint/Cargo.toml apps/mint/Cargo.lock ./
COPY apps/mint/crates/mint-core ./crates/mint-core
COPY apps/mint/crates/mint-store ./crates/mint-store
COPY apps/mint/crates/mint-receipts ./crates/mint-receipts
COPY apps/mint/crates/mint-ledger ./crates/mint-ledger
COPY apps/mint/crates/mint-protocol ./crates/mint-protocol

WORKDIR /src/apps/context-passport
COPY apps/context-passport/Cargo.toml apps/context-passport/Cargo.lock ./
COPY apps/context-passport/crates ./crates

RUN cargo build --release -p passport-server --bin identity-lock-server \
 && strip target/release/identity-lock-server

FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app
COPY --from=builder /src/apps/context-passport/target/release/identity-lock-server /app/identity-lock-server
ENV PASSPORT_BIND_ADDR=0.0.0.0:8095
EXPOSE 8095
USER nonroot
ENTRYPOINT ["/app/identity-lock-server"]
