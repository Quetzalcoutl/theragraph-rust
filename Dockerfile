FROM rust:1.75 as builder

WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Create dummy main to cache dependencies
RUN mkdir src && \
    echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Copy source code
COPY src ./src

# Build for release and build consumer example into release artifacts
RUN touch src/main.rs && \
    cargo build --release && \
    cargo build --release --example consumer_nebula || true

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    procps \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy main engine binary from builder
COPY --from=builder /app/target/release/theragraph-engine /app/theragraph-engine
# Copy built consumer example binary (if present)
COPY --from=builder /app/target/release/examples/consumer_nebula /app/consumer_nebula
# Copy example assets so consumer can run without source mounts
COPY --from=builder /app/examples /app/examples || true

# Make sure consumer binary is executable
RUN ["/bin/sh", "-c", "[ -f /app/consumer_nebula ] && chmod +x /app/consumer_nebula || true"]

# Default CMD runs the engine; override to run consumer
CMD ["/app/theragraph-engine"]
