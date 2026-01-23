# Dockerfile (refactor): smaller runtime, clearer stages, reproducible builds
# - Builder: caches deps, supports SQLX_OFFLINE by including .sqlx and migrations
# - Runtime: minimal Debian image, explicit runtime packages, healthcheck + debug wrapper
# Keep `DEBUG_RUN_WRAPPER=true` for now to capture startup logs during active debugging — flip to "false" when done.

FROM rustlang/rust:nightly AS builder

LABEL org.opencontainers.image.source="https://github.com/Quetzalcoutl/theragraph-rust"

ARG CHAIN_ID=11155111
ARG API_PORT=8081
ARG DATABASE_URL
ARG RPC_URL
ARG KAFKA_BROKERS=kafka:29092
ARG ELIXIR_DATABASE_URL
ARG COOLIFY_URL
ARG COOLIFY_FQDN
ARG COOLIFY_BRANCH
ARG COOLIFY_RESOURCE_UUID

ENV DEBIAN_FRONTEND=noninteractive
ENV SQLX_OFFLINE=1
ENV CARGO_BUILD_JOBS=1

WORKDIR /app

# Copy dependency manifests first (keeps layer cache effective)
COPY Cargo.toml Cargo.lock ./

# Ensure SQLx migrations and prepared query cache are available for offline builds
COPY migrations/ ./migrations/
COPY .sqlx/ .sqlx/

# Install build tools needed for native deps and warm the cargo target cache using a tiny dummy crate
RUN apt-get update -qq && apt-get install -y -qq \
    build-essential \
    cmake \
    pkg-config \
    libsasl2-dev \
    libssl-dev \
    ca-certificates \
  && rm -rf /var/lib/apt/lists/* \
  && mkdir -p src \
  && echo "fn main() {}" > src/main.rs \
  && echo "pub fn __dummy_lib() {}" > src/lib.rs \
  && cargo build --release -j 1 \
  && rm -rf src

# Copy source and build the release binary
COPY src ./src

# If DATABASE_URL build-arg is provided and cargo-sqlx exists in builder, attempt prepare (non-fatal)
RUN if [ -n "$DATABASE_URL" ]; then \
      echo "DATABASE_URL provided; attempting cargo sqlx prepare if sqlx-cli is installed"; \
      if command -v cargo-sqlx >/dev/null 2>&1; then \
        export DATABASE_URL="$DATABASE_URL" && cargo sqlx prepare -- -q || echo "cargo sqlx prepare failed"; \
      else \
        echo "sqlx-cli not found, skipping prepare"; \
      fi; \
    else \
      echo "No DATABASE_URL build-arg; relying on committed .sqlx and SQLX_OFFLINE=1"; \
    fi

# Real build: produce release binary and optional example
RUN touch src/main.rs \
  && cargo build --release -j 1 \
  && if [ -d examples ]; then cargo build --release --example consumer_nebula || true; fi \
  && mkdir -p /app/target/release/examples /app/examples \
  && [ -f /app/target/release/examples/consumer_nebula ] || touch /app/target/release/examples/consumer_nebula

# Runtime image: minimal and predictable
FROM debian:trixie-slim AS runtime

LABEL maintainer="Theragraph <ops@thera.example>"

ENV DEBIAN_FRONTEND=noninteractive \
    KAFKA_BROKERS=kafka:29092 \
    API_PORT=8081 \
    # Keep enabled to capture startup logs during active debugging; set to "false" after fix
    DEBUG_RUN_WRAPPER=true

# Install only what's necessary for runtime + debugging/health checks
RUN apt-get update -qq && apt-get install -y -qq \
    ca-certificates \
    libssl3 \
    libsasl2-2 \
    procps \
    netcat-openbsd \
    curl \
    wget \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy helper scripts (healthcheck, kafka waiter, debug wrapper)
COPY scripts/wait-for-kafka.sh /app/wait-for-kafka.sh
COPY scripts/healthcheck.sh /app/healthcheck.sh
COPY scripts/run-debug.sh /app/run-debug.sh
RUN chmod +x /app/*.sh || true

# Copy the compiled artifacts from builder
COPY --from=builder /app/target/release/theragraph-engine /app/theragraph-engine
COPY --from=builder /app/target/release/examples/consumer_nebula /app/consumer_nebula
COPY --from=builder /app/examples/ /app/examples/
RUN ["/bin/sh","-c","[ -f /app/consumer_nebula ] && chmod +x /app/consumer_nebula || true; [ -f /app/theragraph-engine ] && chmod +x /app/theragraph-engine || true"]

# Expose API port
EXPOSE ${API_PORT}

# Healthcheck: run the scripted probe directly (avoids inline-shell quoting issues); script will emit diagnostics to /tmp/healthcheck.log
HEALTHCHECK --interval=10s --timeout=3s --start-period=60s --retries=10 \
  CMD ["/bin/bash","/app/healthcheck.sh"]

# Entrypoint: run kafka waiter in background then either the debug wrapper or the engine
# - Debug wrapper captures env + stdout/stderr into /tmp/startup.log and sleeps so admins can inspect the container
# - Use `exec` so signals are forwarded to the engine process
ENTRYPOINT ["sh","-c","/app/wait-for-kafka.sh \"${KAFKA_BROKERS:-}\" & if [ \"${DEBUG_RUN_WRAPPER:-}\" = \"true\" ]; then /app/run-debug.sh; else exec \"$@\"; fi"]
CMD ["/app/theragraph-engine"]

# Notes:
# - To capture startup logs without rebuilding, set environment variable `DEBUG_RUN_WRAPPER=true` in your compose / platform.
# - Revert DEBUG_RUN_WRAPPER to "false" once the underlying issue is fixed to restore normal behavior.
