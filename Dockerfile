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

# Build for release and build example consumer into release artifacts
RUN touch src/main.rs && \
    cargo build --release && \
    # Build the consumer example so we can run it as a standalone binary
    cargo build --release --example consumer_nebula

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy main engine binary from builder
COPY --from=builder /app/target/release/theragraph-engine /app/
# Copy built consumer example binary
COPY --from=builder /app/target/release/examples/consumer_nebula /app/consumer_nebula
# Copy example assets (sample events) so the consumer can run without source mounts
COPY --from=builder /app/examples /app/examples

# Make sure consumer binary is executable
RUN chmod +x /app/consumer_nebula || true

# Default CMD runs the engine; consumer is run by overriding the command in compose or Kubernetes
CMD ["./theragraph-engine"]
