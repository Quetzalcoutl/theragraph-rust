#!/usr/bin/env bash
set -eo pipefail

API_PORT=${API_PORT:-8081}
DEBUG=${DEBUG_HEALTHCHECK:-false}
LOGFILE=${HEALTHCHECK_LOG:-/tmp/healthcheck.log}

URL="http://127.0.0.1:${API_PORT}/health"

# Always append timestamped header to logfile for visibility
echo "---- healthcheck run at: $(date -u +"%Y-%m-%dT%H:%M:%SZ") ----" >> "$LOGFILE" || true
echo "API_PORT=${API_PORT} DEBUG=${DEBUG}" >> "$LOGFILE" || true

echo "healthcheck: checking ${URL}" | tee -a "$LOGFILE"

# Prefer explicit curl path to avoid PATH issues
CURL_BIN="/usr/bin/curl"
if [ ! -x "$CURL_BIN" ]; then
  if command -v curl >/dev/null 2>&1; then
    CURL_BIN="$(command -v curl)"
  else
    echo "healthcheck: curl not found at /usr/bin/curl or in PATH" | tee -a "$LOGFILE"
    if [ "$DEBUG" = "true" ]; then
      echo "--- DEBUG DIAGNOSTICS ---" | tee -a "$LOGFILE"
      echo "--- date ---" | tee -a "$LOGFILE"; date | tee -a "$LOGFILE"
      echo "--- ps aux ---" | tee -a "$LOGFILE"; ps aux | tee -a "$LOGFILE" || true
      echo "--- ls -la /app ---" | tee -a "$LOGFILE"; ls -la /app | tee -a "$LOGFILE" || true
    fi
    exit 1
  fi
fi

if [ "$DEBUG" = "true" ]; then
  echo "healthcheck (debug): using curl=${CURL_BIN}" | tee -a "$LOGFILE"
  echo "--- environment ---" | tee -a "$LOGFILE"; env | sort | tee -a "$LOGFILE"
  echo "--- listening ports (ss/netstat) ---" | tee -a "$LOGFILE"
  if command -v ss >/dev/null 2>&1; then ss -ltnp | tee -a "$LOGFILE" || true
  elif command -v netstat >/dev/null 2>&1; then netstat -ltnp | tee -a "$LOGFILE" || true
  else echo "no ss/netstat available" | tee -a "$LOGFILE"; fi
  echo "--- /app contents ---" | tee -a "$LOGFILE"; ls -la /app | tee -a "$LOGFILE" || true
fi

# Use verbose curl when debugging to capture response and write to logfile
if [ "$DEBUG" = "true" ]; then
  "$CURL_BIN" -v --max-time 5 "$URL" 2>&1 | tee -a "$LOGFILE" || {
    echo "healthcheck: curl failed" | tee -a "$LOGFILE"
    exit 1
  }
  echo "healthcheck: ok" | tee -a "$LOGFILE"
  exit 0
else
  if "$CURL_BIN" -fsS --max-time 2 "$URL" >/dev/null 2>&1; then
    echo "healthcheck: ok" | tee -a "$LOGFILE"
    exit 0
  else
    echo "healthcheck: failed to reach ${URL}" | tee -a "$LOGFILE"
    # Dump a few diagnostics to logfile for later inspection
    echo "--- quick diag ---" >> "$LOGFILE" || true
    ps aux >> "$LOGFILE" || true
    if command -v ss >/dev/null 2>&1; then ss -ltnp >> "$LOGFILE" || true; fi
    exit 1
  fi
fi
