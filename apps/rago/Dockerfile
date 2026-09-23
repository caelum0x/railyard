# ---- Build stage -----------------------------------------------------------
FROM rust:1-bookworm AS build
WORKDIR /app
COPY . .
# Build the consolidated supervising binary (API + worker daemon) with the
# live transports: real HTTP for payment providers, integrations and outbound
# webhooks, and real SMTP. Without `live` every outbound call fails loudly.
RUN cargo build --release --features live --bin rago-server

# ---- Runtime stage ---------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/rago-server /usr/local/bin/rago-server
EXPOSE 3000
ENV RAGO_API_ADDR=0.0.0.0:3000
ENTRYPOINT ["rago-server"]
