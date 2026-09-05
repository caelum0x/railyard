# ---- Build stage -----------------------------------------------------------
FROM rust:1-bookworm AS build
WORKDIR /app
COPY . .
# Build the consolidated supervising binary (API + worker daemon).
# Add `--features live` here to bake in the real reqwest/SMTP transports.
RUN cargo build --release --bin rago-server

# ---- Runtime stage ---------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/rago-server /usr/local/bin/rago-server
EXPOSE 3000
ENV RAGO_API_ADDR=0.0.0.0:3000
ENTRYPOINT ["rago-server"]
