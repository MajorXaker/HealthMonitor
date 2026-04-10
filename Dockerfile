# ── Stage 1: builder ────────────────────────────────────────────────────────
FROM rust:1.85-slim AS builder

# Install system deps required by native-tls and sqlx
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock ./

# Create a dummy main so cargo can fetch and compile dependencies
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release 2>/dev/null || true
RUN rm -rf src

# Copy real sources and migrations
COPY src ./src
COPY migrations ./migrations

# Build the real binary (dependencies are cached from above)
# SQLX_OFFLINE=true skips compile-time DB checks (no DB available at build time)
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
