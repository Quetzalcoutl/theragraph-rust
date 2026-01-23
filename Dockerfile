FROM rustlang/rust:nightly AS builder

WORKDIR /app

# Ensure examples dir exists even if the build fails (prevents later COPY failures)
RUN mkdir -p /app/examples

ENV DEBIAN_FRONTEND=noninteractive
ARG DATABASE_URL
ENV SQLX_OFFLINE=1
ENV CARGO_BUILD_JOBS=1

# Copy manifests
COPY Cargo.toml Cargo.lock ./
# Ensure SQLx migrations are available at build-time for `sqlx::migrate!` macro
COPY migrations/ ./migrations/
# Also include prepared SQLx query cache for offline compilation (generated via `cargo sqlx prepare`)
# This allows `SQLX_OFFLINE=1` to succeed during builder compile and avoids needing sqlx-cli in CI
COPY .sqlx/ .sqlx/

# Create dummy main to cache dependencies
RUN apt-get update -qq && apt-get install -y -qq \
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
    cargo build --release -j 1 && \
    rm -rf src

# Copy source code
COPY src ./src

# If a DATABASE_URL build-arg is supplied and sqlx-cli is available, attempt to prepare SQLx query cache.
# Otherwise rely on committed sqlx-data.json and SQLX_OFFLINE=1
RUN if [ -n "$DATABASE_URL" ]; then \
      echo "DATABASE_URL provided; attempting cargo sqlx prepare if sqlx-cli is installed"; \
      if command -v cargo-sqlx >/dev/null 2>&1; then \
        export DATABASE_URL="$DATABASE_URL" && cargo sqlx prepare -- -q || echo "cargo sqlx prepare failed"; \
      else \
        echo "sqlx-cli not found (cargo-sqlx), skipping prepare"; \
      fi; \
    else \
      echo "No DATABASE_URL build-arg; assuming sqlx-data.json is present and using SQLX_OFFLINE"; \
    fi

# Build for release and build consumer example into release artifacts
RUN touch src/main.rs && \
    cargo build --release -j 1 && \
    # Only attempt to build the example if the examples directory exists (prevents build failures when examples are absent)
    if [ -d examples ]; then \
      cargo build --release --example consumer_nebula || true; \
    fi && \
    # Ensure target/example and examples directories exist so runtime COPYs don't fail
    mkdir -p /app/target/release/examples /app/examples && \
    [ -f /app/target/release/examples/consumer_nebula ] || touch /app/target/release/examples/consumer_nebula

# Runtime stage
FROM debian:trixie-slim AS runtime

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update -qq && apt-get install -y -qq \
    bash \
    ca-certificates \
    libssl3 \
    libsasl2-2 \
    procps \
    netcat-openbsd \
    iproute2 \
    iputils-ping \
    dnsutils \
    curl \
    wget \
    && rm -rf /var/lib/apt/lists/*

# Fail fast if curl is not available after install (fail build early and surface issue)
RUN if ! command -v curl >/dev/null 2>&1; then echo "curl not found in runtime image" >&2; exit 1; fi

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

# Make sure consumer and engine binaries are executable
RUN ["/bin/sh", "-c", "[ -f /app/consumer_nebula ] && chmod +x /app/consumer_nebula || true; [ -f /app/theragraph-engine ] && chmod +x /app/theragraph-engine || true"]

# Default Kafka bootstrap when running in the same Docker network as Kafka service
# Supports file-based env via FFOLDER (each file name => env var name). Mount your secret folder and set FFOLDER to point to it.
ENV KAFKA_BROKERS=kafka:29092
# API port (configurable) — keep in sync with compose / platform routing
ENV API_PORT=8081

# Expose API port and use a scripted healthcheck (longer start-period to allow app init)
EXPOSE ${API_PORT}

# Copy and make healthcheck script executable
COPY scripts/healthcheck.sh /app/healthcheck.sh
RUN chmod +x /app/healthcheck.sh

# Primary healthcheck: check that the API port is listening using `ss` (no curl dependency) and fall back to the scripted probe
# Make start period generous and allow more retries to avoid false negatives during startup
HEALTHCHECK --interval=10s --timeout=3s --start-period=60s --retries=10 \
  CMD ["bash","-lc","if ss -ltn | grep -q \":${API_PORT}\b"; then echo 'healthcheck: port listening'; exit 0; else echo 'healthcheck: port not listening, running scripted probe and dumping diagnostics'; /app/healthcheck.sh || true; echo '--- ps ---'; ps aux; echo '--- ss -ltnp ---'; ss -ltnp 2>/dev/null || true; echo '--- /tmp/healthcheck.log ---'; cat /tmp/healthcheck.log 2>/dev/null || true; exit 1; fi"]

# Entrypoint will start the Kafka waiter in background so the app can start and serve health checks
# Use shell form to pass env and then conditionally run debug wrapper if DEBUG_RUN_WRAPPER=true
ENTRYPOINT ["sh","-c","/app/wait-for-kafka.sh \"${KAFKA_BROKERS:-}\" & if [ \"${DEBUG_RUN_WRAPPER:-}\" = \"true\" ]; then /app/run-debug.sh; else exec \"$@\"; fi" ]
CMD ["/app/theragraph-engine"]
