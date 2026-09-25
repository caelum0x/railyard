# syntax=docker/dockerfile:1.7
#
# BUILD CONTEXT = the railyard repo ROOT, NOT apps/vend:
#   docker build -f apps/vend/Dockerfile -t railyard/vend-server .
# vend-mint-client path-depends on apps/mint/crates/{mint-core,mint-protocol}
# (`../../../mint/crates/...`), so the mint workspace is recreated as a sibling
# under /src/apps. Dockerfile.dockerignore trims the context to these two
# workspaces.
FROM rust:1.94-bookworm AS builder

WORKDIR /src/apps/mint
COPY apps/mint/Cargo.toml ./
COPY apps/mint/crates ./crates
COPY apps/mint/contracts ./contracts

WORKDIR /src/apps/vend
COPY apps/vend/Cargo.toml apps/vend/Cargo.lock ./
COPY apps/vend/crates ./crates
RUN cargo build --locked --release -p vend-server \
    --bin vend-server --bin vend-bootstrap --bin vend-reconcile

FROM debian:bookworm-slim AS runtime
ARG VCS_REF=unknown
ARG BUILD_DATE=unknown
LABEL org.opencontainers.image.title="Vend" \
      org.opencontainers.image.description="Metered inference and settlement gateway" \
      org.opencontainers.image.revision="$VCS_REF" \
      org.opencontainers.image.created="$BUILD_DATE" \
      org.opencontainers.image.source="https://github.com/replace/reelforge"
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && find /var/log -type f -delete

COPY --from=builder --chown=65532:65532 /src/apps/vend/target/release/vend-server /usr/local/bin/vend-server
COPY --from=builder --chown=65532:65532 /src/apps/vend/target/release/vend-bootstrap /usr/local/bin/vend-bootstrap
COPY --from=builder --chown=65532:65532 /src/apps/vend/target/release/vend-reconcile /usr/local/bin/vend-reconcile

ENV VEND_BIND_ADDR=0.0.0.0:8090 \
    RUST_LOG=info \
    VEND_LOG_FORMAT=json \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
EXPOSE 8090
USER 65532:65532
HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=6 \
    CMD ["curl", "--fail", "--silent", "--show-error", "--max-time", "2", "http://127.0.0.1:8090/readyz"]
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/vend-server"]
