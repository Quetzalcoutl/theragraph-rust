#!/usr/bin/env bash
set -eo pipefail

API_PORT=${API_PORT:-8081}

if ! command -v curl >/dev/null 2>&1; then
  echo "healthcheck: curl not found"
  exit 1
fi

URL="http://127.0.0.1:${API_PORT}/health"

echo "healthcheck: checking ${URL}"
if curl -fsS --max-time 2 "$URL" >/dev/null; then
  echo "healthcheck: ok"
  exit 0
else
  echo "healthcheck: failed to reach ${URL}"
  exit 1
fi
