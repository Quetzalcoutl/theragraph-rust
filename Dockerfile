# Dockerfile: smaller runtime, clearer stages, reproducible builds
# - Builder: caches deps, supports SQLX_OFFLINE by including .sqlx and migrations
# - Runtime: minimal Debian image, explicit runtime packages, healthcheck

# The Rust version must be the one the project pins: Cargo.toml says rust-version = "1.95" and rust-toolchain.toml says 1.95.0. A
# builder older than that stops at the very first `cargo build` (exit 101: "requires rustc 1.95 or newer"). This was rust:1.79-slim
# until 2026-11-06, when the first Coolify build of the current code failed there.
#
# The Debian release is bookworm (GCC 12, OpenSSL 3.0) ON PURPOSE: the same build on trixie failed twice, at the same file, compiling
# librdkafka's rdkafka_conf.c (2026-11-06), and bookworm is the environment this project has always built in. The runtime stage below is
# NEWER (trixie): a binary built against an older glibc runs on a newer one, never the other way round, and the ldd check in the runtime
# stage proves it at build time.
FROM rust:1.95.0-slim-bookworm AS builder

LABEL org.opencontainers.image.source="https://github.com/Quetzalcoutl/theragraph-rust"

ARG CHAIN_ID=11155111
ARG API_PORT=8081
ARG RPC_URL
ARG KAFKA_BROKERS=kafka:29092
# (No DATABASE_URL / ELIXIR_DATABASE_URL build args: Coolify feeds every env var in as a build arg, and a build arg that a RUN step reads is
# written into that layer's history on the build host. The build needs no database: SQLX_OFFLINE=1 and the committed .sqlx data.)
ARG COOLIFY_URL
ARG COOLIFY_FQDN
ARG COOLIFY_BRANCH
ARG COOLIFY_RESOURCE_UUID

ENV DEBIAN_FRONTEND=noninteractive
ENV SQLX_OFFLINE=1
ENV CARGO_BUILD_JOBS=1

# pipefail: the two cargo steps below pipe through `tee` so their output still streams live AND a failure prints the first errors at the
# END of the log (BuildKit shows only the last lines of a failed step: the compiler's own message used to be cut off).
SHELL ["/bin/bash", "-o", "pipefail", "-c"]

WORKDIR /app

# Copy dependency manifests first (keeps layer cache effective)
COPY Cargo.toml Cargo.lock ./

# Ensure SQLx migrations and prepared query cache are available for offline builds
COPY migrations/ ./migrations/
COPY .sqlx/ .sqlx/

# Install build tools needed for native deps and warm the cargo target cache using a tiny dummy crate.
# libcurl4-openssl-dev is needed for its HEADER only: librdkafka 2.12.1's rdkafka_conf.c does `#ifdef WITH_OAUTHBEARER_OIDC #include <curl/curl.h>`
# and the build defines that macro as 0 (curl support is off), which `#ifdef` still counts as defined. Nothing links against curl.
# (A trixie builder and then a bookworm one failed on exactly this line, 2026-11-06: the compiler was never the problem.)
RUN apt-get update -qq && apt-get install -y -qq \
    build-essential \
    cmake \
    pkg-config \
    libsasl2-dev \
    libssl-dev \
    libcurl4-openssl-dev \
    ca-certificates \
  && rm -rf /var/lib/apt/lists/* \
  && mkdir -p src \
  && echo "fn main() {}" > src/main.rs \
  && echo "pub fn __dummy_lib() {}" > src/lib.rs \
  && echo "--- build host: $(df -h / | tail -1) | $(free -m | sed -n 2p)" \
  && ( cargo build --release --locked -j 1 2>&1 | tee /tmp/deps-build.log \
       || { echo "=== dependency build FAILED: the first errors ==="; grep -nE '^error|error\[|error:|Killed|No space|fatal' /tmp/deps-build.log | head -12; exit 1; } ) \
  && rm -rf src

# Copy source and build the release binary
COPY src ./src

# (The queries are checked offline against the committed .sqlx data: SQLX_OFFLINE=1 above. Run `cargo sqlx prepare` on a developer machine
# and commit .sqlx when a query changes.)

# Real build: produce release binary and optional example
RUN touch src/main.rs \
  && ( cargo build --release --locked -j 1 2>&1 | tee /tmp/app-build.log \
       || { echo "=== release build FAILED: the first errors ==="; grep -nE '^error|error\[|error:|Killed|No space|fatal' /tmp/app-build.log | head -12; exit 1; } ) \
  && if [ -d examples ]; then cargo build --release --example consumer_nebula || true; fi \
  && mkdir -p /app/target/release/examples /app/examples \
  && [ -f /app/target/release/examples/consumer_nebula ] || touch /app/target/release/examples/consumer_nebula

# nebula-console binary — extracted from the official Vesoft image
FROM vesoft/nebula-console:v3.8.0 AS nebula-console

# Runtime image: minimal and predictable
FROM debian:trixie-slim AS runtime

LABEL maintainer="Theragraph <ops@thera.example>"

ENV DEBIAN_FRONTEND=noninteractive \
    KAFKA_BROKERS=kafka:29092 \
    API_PORT=8000 \
    DEBUG_RUN_WRAPPER=false

# Install only what's necessary for runtime + debugging/health checks
RUN apt-get update -qq && apt-get install -y -qq \
    ca-certificates \
    libssl3t64 \
    libsasl2-2 \
    procps \
    netcat-openbsd \
    curl \
    wget \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# nebula-console binary — needed by graph_client.rs to run nGQL queries
COPY --from=nebula-console /usr/local/bin/nebula-console /usr/local/bin/nebula-console
RUN chmod +x /usr/local/bin/nebula-console

# Copy helper scripts (healthcheck, kafka waiter, debug wrapper)
COPY scripts/wait-for-kafka.sh /app/wait-for-kafka.sh
COPY scripts/healthcheck.sh /app/healthcheck.sh
COPY scripts/run-debug.sh /app/run-debug.sh
RUN chmod +x /app/*.sh || true

# Copy the compiled artifacts from builder
COPY --from=builder /app/target/release/theragraph /app/theragraph
COPY --from=builder /app/target/release/examples/consumer_nebula /app/consumer_nebula
COPY --from=builder /app/examples/ /app/examples/
RUN ["/bin/sh","-c","[ -f /app/consumer_nebula ] && chmod +x /app/consumer_nebula || true; [ -f /app/theragraph ] && chmod +x /app/theragraph || true"]

# Fail the BUILD, not the first start, if the binary needs a library (or a newer glibc) that this image does not have.
RUN if ldd /app/theragraph 2>&1 | grep -q "not found"; then \
      ldd /app/theragraph >&2; \
      echo "FATAL: the binary needs libraries this image does not have: build and run on the same Debian release" >&2; \
      exit 1; \
    fi

# Expose API port
EXPOSE ${API_PORT}

# Healthcheck: run the scripted probe directly (avoids inline-shell quoting issues); script will emit diagnostics to /tmp/healthcheck.log
HEALTHCHECK --interval=10s --timeout=3s --start-period=60s --retries=10 \
  CMD ["/bin/bash","/app/healthcheck.sh"]

# Entrypoint: run kafka waiter in background then either the debug wrapper or the engine
# - Debug wrapper captures env + stdout/stderr into /tmp/startup.log and sleeps so admins can inspect the container
# - Use `exec` so signals are forwarded to the engine process
ENTRYPOINT ["sh","-c","/app/wait-for-kafka.sh \"${KAFKA_BROKERS:-}\" & if [ \"${DEBUG_RUN_WRAPPER:-}\" = \"true\" ]; then /app/run-debug.sh; else exec /app/theragraph; fi"]
CMD ["/app/theragraph"]

# Notes:
# - To capture startup logs without rebuilding, set environment variable `DEBUG_RUN_WRAPPER=true` in your compose / platform.
# - Revert DEBUG_RUN_WRAPPER to "false" once the underlying issue is fixed to restore normal behavior.
