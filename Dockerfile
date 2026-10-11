# syntax=docker/dockerfile:1
# Multi-stage build for the KSI Rust backend (multi-crate workspace).

FROM rust:1.89-slim-bookworm AS builder
WORKDIR /app

# Cache dependencies first (only re-runs when manifests change).
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ksi-models/Cargo.toml crates/ksi-models/
COPY crates/ksi-db/Cargo.toml crates/ksi-db/
COPY crates/ksi-auth/Cargo.toml crates/ksi-auth/
COPY crates/ksi-sandbox/Cargo.toml crates/ksi-sandbox/
COPY crates/ksi-api/Cargo.toml crates/ksi-api/
COPY crates/ksi-backend/Cargo.toml crates/ksi-backend/
RUN for c in ksi-models ksi-db ksi-auth ksi-sandbox ksi-api; do \
      mkdir -p crates/$c/src && touch crates/$c/src/lib.rs; \
    done && \
    mkdir -p crates/ksi-backend/src && \
    echo 'fn main() {}' > crates/ksi-backend/src/main.rs && \
    cargo build --release && \
    rm -rf crates/*/src

# Real build.
COPY crates ./crates
RUN cargo build --release

FROM debian:bookworm-slim
# NOTE: install the `isolate` sandbox binary here if this image should
# evaluate programming modules (see README "Code execution sandbox").
# It needs privileges for namespaces/cgroups, so a plain apt install is
# usually not enough on its own — see https://github.com/ioi/isolate.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/ksi-backend /usr/local/bin/ksi-backend

ENV LISTEN_ADDR=0.0.0.0:3030
ENV DATABASE_URL=sqlite:///var/ksi-be/db.sqlite
EXPOSE 3030
CMD ["ksi-backend"]
