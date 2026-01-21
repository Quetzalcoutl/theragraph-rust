FROM rustlang/rust:nightly AS builder

WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock ./
# Ensure SQLx migrations are available at build-time for `sqlx::migrate!` macro
COPY migrations/ ./migrations/

# Create dummy main to cache dependencies
RUN apt-get update && apt-get install -y \
    build-essential \
    cmake \
    pkg-config \
    libsasl2-dev \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/* && \
    mkdir src && \
    echo "fn main() {}" > src/main.rs && \
    # Also add a minimal lib placeholder so Cargo can resolve a [lib] target during dependency caching
    echo "pub fn __dummy_lib() {}" > src/lib.rs && \
    cargo build --release && \
    rm -rf src

# Copy source code
COPY src ./src

# Build for release and build consumer example into release artifacts
RUN touch src/main.rs && \
    cargo build --release && \
    # Only attempt to build the example if the examples directory exists (prevents build failures when examples are absent)
    if [ -d examples ]; then \
      cargo build --release --example consumer_nebula || true; \
    fi && \
    # Ensure target/example and examples directories exist so runtime COPYs don't fail
    mkdir -p /app/target/release/examples /app/examples && \
    [ -f /app/target/release/examples/consumer_nebula ] || touch /app/target/release/examples/consumer_nebula

# Runtime stage
FROM debian:trixie-slim AS runtime

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    libsasl2-2 \
    procps \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy wait script and make it executable
COPY scripts/wait-for-kafka.sh /app/wait-for-kafka.sh
RUN chmod +x /app/wait-for-kafka.sh

# Copy main engine binary from builder
COPY --from=builder /app/target/release/theragraph-engine /app/theragraph-engine
# Copy built consumer example binary (if present)
COPY --from=builder /app/target/release/examples/consumer_nebula /app/consumer_nebula
# Copy example assets so consumer can run without source mounts
# Ensure we copy the directory (trailing slashes) and remove the invalid shell `|| true` suffix
COPY --from=builder /app/examples/ /app/examples/

# Make sure consumer binary is executable
RUN ["/bin/sh", "-c", "[ -f /app/consumer_nebula ] && chmod +x /app/consumer_nebula || true"]

# Entrypoint will wait for internal Kafka before starting the engine
ENTRYPOINT ["/app/wait-for-kafka.sh", "kafka:29092", "20", "2"]
CMD ["/app/theragraph-engine"]
