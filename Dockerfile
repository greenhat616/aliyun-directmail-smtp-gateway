# syntax=docker/dockerfile:1

# ---- builder ---------------------------------------------------------------
# Full (non-slim) rust image: ships gcc/make/perl so C-backed deps (ring via
# rustls) compile cleanly. The "1" tag always resolves to the latest stable
# 1.x (>= 1.85, required by edition 2024). Override with --build-arg to pin.
ARG RUST_VERSION=1
FROM rust:${RUST_VERSION}-bookworm AS builder

WORKDIR /app
COPY . .

# Cache mounts keep the cargo registry and target dir across builds; the binary
# is copied out of the cache-mounted target dir within the same RUN so it lands
# in a persisted layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked --bin aliyun-directmail-smtp-gateway \
 && cp target/release/aliyun-directmail-smtp-gateway /usr/local/bin/gateway

# ---- runtime ---------------------------------------------------------------
# distroless/cc provides glibc + libgcc for the dynamically-linked binary, no
# shell or package manager. The :nonroot tag runs as uid 65532.
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

COPY --from=builder /usr/local/bin/gateway /usr/local/bin/gateway

# Bind on all interfaces inside the container (the app defaults to loopback).
# Sender defaults to in-memory (safe); set GATEWAY_SENDER=directmail plus the
# DIRECTMAIL_* credentials to deliver for real.
ENV GATEWAY_BIND_ADDR=0.0.0.0:2525
EXPOSE 2525

USER nonroot
ENTRYPOINT ["/usr/local/bin/gateway"]
