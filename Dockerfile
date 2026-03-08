# Build stage
FROM rust:1.77-bookworm AS builder

WORKDIR /app

# Copy manifests first for better caching
COPY Cargo.toml Cargo.lock* ./

# Create a dummy main.rs to cache dependency builds
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release 2>/dev/null || true

# Copy actual source code
COPY src/ src/

# Build the real binary
RUN cargo build --release

# Runtime stage — minimal image
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for security
RUN useradd -m -s /bin/bash gyroclaw

WORKDIR /home/gyroclaw

# Copy the compiled binary
COPY --from=builder /app/target/release/gyro-claw /usr/local/bin/gyro-claw

# Create directories for vault and memory
RUN mkdir -p /home/gyroclaw/.gyro-claw && \
    chown -R gyroclaw:gyroclaw /home/gyroclaw

USER gyroclaw

# Expose API port
EXPOSE 3000

# Default command: start the API server
CMD ["gyro-claw", "serve"]
