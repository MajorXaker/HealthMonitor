# ── Stage 1: builder ────────────────────────────────────────────────────────
FROM rust:1.94-slim AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./

RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release || true

RUN rm -rf src \
    target/release/healthmon \
    target/release/deps/healthmon*

COPY src ./src
COPY migrations ./migrations

ENV SQLX_OFFLINE=true
RUN cargo build --release

# ── Stage 2: runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the compiled binary
COPY --from=builder /app/target/release/healthmon /app/healthmon

# Copy migrations (needed at runtime for sqlx::migrate!)
COPY --from=builder /app/migrations /app/migrations

# config.json is NOT baked into the image — mount it at runtime.
# Example: docker run -v $(pwd)/config.json:/app/config.json healthmon

EXPOSE 8080

CMD ["/app/healthmon"]
