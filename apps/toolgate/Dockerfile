# syntax=docker/dockerfile:1.7
#
# apps/toolgate: Switchyard server + CLI. Standalone workspace with no path
# dependencies outside apps/toolgate, so the build context is this directory:
#
#   docker buildx build --platform linux/arm64,linux/amd64 -t railyard/switchyard apps/toolgate
#
# Runtime is distroless (CA bundle only); reqwest and sqlx use rustls, so
# there is no OpenSSL. Serves on :8130.

FROM rust:1.94-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
COPY catalog ./catalog
RUN cargo build --release -p switchyard-server -p switchyard-cli \
 && strip target/release/switchyard-server target/release/sy

FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app
COPY --from=builder /src/target/release/switchyard-server /app/switchyard-server
COPY --from=builder /src/target/release/sy /app/sy
COPY --from=builder /src/catalog /app/catalog
ENV SWITCHYARD_CATALOG_DIR=/app/catalog \
    SWITCHYARD_BIND=0.0.0.0:8130 \
    SWITCHYARD_CACHE_MB=32
EXPOSE 8130
USER nonroot
ENTRYPOINT ["/app/switchyard-server"]
CMD ["serve"]
