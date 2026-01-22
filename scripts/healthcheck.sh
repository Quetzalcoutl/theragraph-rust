#!/usr/bin/env bash
set -eo pipefail

API_PORT=${API_PORT:-8081}
DEBUG=${DEBUG_HEALTHCHECK:-false}
LOGFILE=${HEALTHCHECK_LOG:-/tmp/healthcheck.log}

URL="http://127.0.0.1:${API_PORT}/health"

# Ensure log target is writable; fall back to stdout if not
if ! touch "$LOGFILE" 2>/dev/null; then
  LOGFILE=/dev/stdout
fi

# Small helper to write both to logfile and stderr (so Coolify/UIs capture it)
log() {
  echo "$@" | tee -a "$LOGFILE" >&2 || true
}

# Error trap to always emit diagnostics when script exits non-zero
on_error() {
  rc=$?
  if [ "$rc" -ne 0 ]; then
    log "---- healthcheck ERROR (exit $rc) at: $(date -u +"%Y-%m-%dT%H:%M:%SZ") ----"
    log "--- tail of $LOGFILE ---"
    tail -n 200 "$LOGFILE" 2>/dev/null | sed -n '1,200p' | tee -a "$LOGFILE" >&2 || true
    log "--- ps aux ---"
    ps aux | tee -a "$LOGFILE" >&2 || true
    log "--- ss -ltnp (if available) ---"
    if command -v ss >/dev/null 2>&1; then ss -ltnp | tee -a "$LOGFILE" >&2 || true; fi
  fi
  exit "$rc"
}
trap 'on_error' EXIT

log "---- healthcheck run at: $(date -u +"%Y-%m-%dT%H:%M:%SZ") ----"
log "API_PORT=${API_PORT} DEBUG=${DEBUG}"

log "healthcheck: checking ${URL}"

# Prefer explicit curl path to avoid PATH issues; fall back to wget if curl is not available
CURL_BIN="/usr/bin/curl"
WGET_BIN="/usr/bin/wget"
HTTP_CLIENT=""

if [ -x "$CURL_BIN" ]; then
  HTTP_CLIENT="curl"
elif command -v curl >/dev/null 2>&1; then
  CURL_BIN="$(command -v curl)"
  HTTP_CLIENT="curl"
elif [ -x "$WGET_BIN" ]; then
  HTTP_CLIENT="wget"
elif command -v wget >/dev/null 2>&1; then
  WGET_BIN="$(command -v wget)"
  HTTP_CLIENT="wget"
else
  log "healthcheck: neither curl nor wget found"
  if [ "$DEBUG" = "true" ]; then
    log "--- DEBUG DIAGNOSTICS ---"
    log "--- date ---"; date | tee -a "$LOGFILE" >&2 || true
    log "--- ps aux ---"; ps aux | tee -a "$LOGFILE" >&2 || true
    log "--- ls -la /app ---"; ls -la /app | tee -a "$LOGFILE" >&2 || true
  fi
  exit 1
fi

if [ "$DEBUG" = "true" ]; then
  log "healthcheck (debug): using curl=${CURL_BIN}"
  log "--- environment ---"; env | sort | tee -a "$LOGFILE" >&2 || true
  log "--- listening ports (ss/netstat) ---"
  if command -v ss >/dev/null 2>&1; then ss -ltnp | tee -a "$LOGFILE" >&2 || true
  elif command -v netstat >/dev/null 2>&1; then netstat -ltnp | tee -a "$LOGFILE" >&2 || true
  else log "no ss/netstat available"; fi
  log "--- /app contents ---"; ls -la /app | tee -a "$LOGFILE" >&2 || true
fi

# Use verbose HTTP client when debugging to capture response and write to logfile
if [ "$DEBUG" = "true" ]; then
  if [ "$HTTP_CLIENT" = "curl" ]; then
    "$CURL_BIN" -v --max-time 5 "$URL" 2>&1 | tee -a "$LOGFILE" >&2 || {
      log "healthcheck: curl failed"
      exit 1
    }
  else
    # wget verbose-ish capture
    "$WGET_BIN" -S -O - --timeout=5 "$URL" 2>&1 | tee -a "$LOGFILE" >&2 || {
      log "healthcheck: wget failed"
      exit 1
    }
  fi
  log "healthcheck: ok"
  # success -> clear EXIT trap without printing error diagnostics
  trap - EXIT
  exit 0
else
  if [ "$HTTP_CLIENT" = "curl" ]; then
    if "$CURL_BIN" -fsS --max-time 2 "$URL" >/dev/null 2>&1; then
      log "healthcheck: ok"
      trap - EXIT
      exit 0
    fi
  else
    if "$WGET_BIN" -q -O - --timeout=2 "$URL" >/dev/null 2>&1; then
      log "healthcheck: ok"
      trap - EXIT
      exit 0
    fi
  fi

  log "healthcheck: failed to reach ${URL}"
  # Dump a few diagnostics to logfile for later inspection
  log "--- quick diag ---"
  ps aux | tee -a "$LOGFILE" >&2 || true
  if command -v ss >/dev/null 2>&1; then ss -ltnp | tee -a "$LOGFILE" >&2 || true; fi
  exit 1
fi

# Prefer explicit curl path to avoid PATH issues; fall back to wget if curl is not available
CURL_BIN="/usr/bin/curl"
WGET_BIN="/usr/bin/wget"
HTTP_CLIENT=""

if [ -x "$CURL_BIN" ]; then
  HTTP_CLIENT="curl"
elif command -v curl >/dev/null 2>&1; then
  CURL_BIN="$(command -v curl)"
  HTTP_CLIENT="curl"
elif [ -x "$WGET_BIN" ]; then
  HTTP_CLIENT="wget"
elif command -v wget >/dev/null 2>&1; then
  WGET_BIN="$(command -v wget)"
  HTTP_CLIENT="wget"
else
  echo "healthcheck: neither curl nor wget found" | tee -a "$LOGFILE"
  if [ "$DEBUG" = "true" ]; then
    echo "--- DEBUG DIAGNOSTICS ---" | tee -a "$LOGFILE"
    echo "--- date ---" | tee -a "$LOGFILE"; date | tee -a "$LOGFILE"
    echo "--- ps aux ---" | tee -a "$LOGFILE"; ps aux | tee -a "$LOGFILE" || true
    echo "--- ls -la /app ---" | tee -a "$LOGFILE"; ls -la /app | tee -a "$LOGFILE" || true
  fi
  exit 1
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

# Use verbose HTTP client when debugging to capture response and write to logfile
if [ "$DEBUG" = "true" ]; then
  if [ "$HTTP_CLIENT" = "curl" ]; then
    "$CURL_BIN" -v --max-time 5 "$URL" 2>&1 | tee -a "$LOGFILE" || {
      echo "healthcheck: curl failed" | tee -a "$LOGFILE"
      exit 1
    }
  else
    # wget verbose-ish capture
    "$WGET_BIN" -S -O - --timeout=5 "$URL" 2>&1 | tee -a "$LOGFILE" || {
      echo "healthcheck: wget failed" | tee -a "$LOGFILE"
      exit 1
    }
  fi
  echo "healthcheck: ok" | tee -a "$LOGFILE"
  exit 0
else
  if [ "$HTTP_CLIENT" = "curl" ]; then
    if "$CURL_BIN" -fsS --max-time 2 "$URL" >/dev/null 2>&1; then
      echo "healthcheck: ok" | tee -a "$LOGFILE"
      exit 0
    fi
  else
    if "$WGET_BIN" -q -O - --timeout=2 "$URL" >/dev/null 2>&1; then
      echo "healthcheck: ok" | tee -a "$LOGFILE"
      exit 0
    fi
  fi

  echo "healthcheck: failed to reach ${URL}" | tee -a "$LOGFILE"
  # Dump a few diagnostics to logfile for later inspection
  echo "--- quick diag ---" >> "$LOGFILE" || true
  ps aux >> "$LOGFILE" || true
  if command -v ss >/dev/null 2>&1; then ss -ltnp >> "$LOGFILE" || true; fi
  exit 1
fi
