# syntax=docker/dockerfile:1

# --- Build stage -------------------------------------------------------
FROM rust:1.85-slim-bookworm AS builder
WORKDIR /app

# aws-lc-rs (pulled in transitively via rustls) compiles C/assembly at build
# time and needs a toolchain; the rest are for TLS crates that fall back to
# source builds.
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential cmake perl pkg-config ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release --locked

# --- Runtime stage -------------------------------------------------------
FROM debian:bookworm-slim AS runtime
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /app --shell /usr/sbin/nologin bridge

COPY --from=builder /app/target/release/a2a-mcp-server /usr/local/bin/a2a-mcp-server

USER bridge
EXPOSE 8000
ENTRYPOINT ["a2a-mcp-server"]
