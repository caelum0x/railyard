# syntax=docker/dockerfile:1.7
#
# apps/ledgerline: Ledgerline margin-lens server. Standalone single-crate
# package (not a cargo workspace member of the repo root), so the build
# context is this directory:
#
#   docker build -t railyard/ledgerline apps/ledgerline
#
# Runtime is distroless (CA bundle only); sqlx and reqwest use rustls, so
# there is no OpenSSL. Serves on :8093 by default (LEDGERLINE_BIND_ADDR).

FROM rust:1.94-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
COPY migrations ./migrations
RUN cargo build --release --bin ledgerline-server \
 && strip target/release/ledgerline-server

FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app
COPY --from=builder /src/target/release/ledgerline-server /app/ledgerline-server
ENV LEDGERLINE_BIND_ADDR=0.0.0.0:8093
EXPOSE 8093
USER nonroot
ENTRYPOINT ["/app/ledgerline-server"]
