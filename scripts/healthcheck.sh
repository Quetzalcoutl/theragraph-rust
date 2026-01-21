#!/usr/bin/env bash
set -eo pipefail

API_PORT=${API_PORT:-8081}
DEBUG=${DEBUG_HEALTHCHECK:-false}

URL="http://127.0.0.1:${API_PORT}/health"

# Ensure curl exists
if ! command -v curl >/dev/null 2>&1; then
  echo "healthcheck: curl not found"
  # If debugging enabled, print some diagnostics anyway
  if [ "$DEBUG" = "true" ]; then
    echo "healthcheck: DEBUG=true — printing diagnostics despite missing curl"
    echo "--- date ---"; date
    echo "--- ps aux ---"; ps aux || true
    echo "--- ls -la /app ---"; ls -la /app || true
  fi
  exit 1
fi

# Verbose diagnostics when DEBUG_HEALTHCHECK=true
if [ "$DEBUG" = "true" ]; then
  echo "healthcheck (debug): starting diagnostics"
  echo "--- date ---"; date
  echo "--- environment ---"; env | sort
  echo "--- process list ---"; ps aux || true
  echo "--- listening ports (ss/netstat) ---"
  if command -v ss >/dev/null 2>&1; then ss -ltnp || true
  elif command -v netstat >/dev/null 2>&1; then netstat -ltnp || true
  else echo "no ss/netstat available"; fi
  echo "--- /app contents ---"; ls -la /app || true
  echo "--- last 200 lines of any /app/*.log ---"; for f in /app/*.log; do [ -f "$f" ] && echo "--- $f ---" && tail -n 200 "$f"; done || true
fi

echo "healthcheck: checking ${URL}"

# Use verbose curl when debugging to capture response
if [ "$DEBUG" = "true" ]; then
  curl -v --max-time 5 "$URL" || {
    echo "healthcheck: curl failed"
    exit 1
  }
  echo "healthcheck: ok"
  exit 0
else
  if curl -fsS --max-time 2 "$URL" >/dev/null; then
    echo "healthcheck: ok"
    exit 0
  else
    echo "healthcheck: failed to reach ${URL}"
    exit 1
  fi
fi
