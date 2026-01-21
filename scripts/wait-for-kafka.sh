#!/usr/bin/env bash
set -euo pipefail

BROKER=${1:-kafka:29092}
RETRIES=${2:-20}
DELAY=${3:-2}

echo "Waiting for Kafka broker $BROKER..."
count=0
while :; do
  if docker run --rm edenhill/kcat:1.7.1 -b "$BROKER" -L >/dev/null 2>&1; then
    echo "Kafka is available at $BROKER"
    exit 0
  fi
  count=$((count+1))
  if [ "$count" -ge "$RETRIES" ]; then
    echo "Kafka not available after $RETRIES attempts, giving up"
    exit 1
  fi
  sleep "$DELAY"
done