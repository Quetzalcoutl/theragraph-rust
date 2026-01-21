#!/usr/bin/env bash
set -euo pipefail

# Wait-for-kafka: tries to resolve and connect to broker list.
# Usage: KAFKA_BROKERS can be set as env (comma separated) or pass as first arg.

INPUT=${1:-}
BROKERS_ENV=${KAFKA_BROKERS:-}
if [ -n "$INPUT" ]; then
  BROKER_LIST="$INPUT"
elif [ -n "$BROKERS_ENV" ]; then
  BROKER_LIST="$BROKERS_ENV"
else
  BROKER_LIST="kafka:29092"
fi

RETRIES=${2:-20}
DELAY=${3:-2}

IFS=',' read -r -a BROKERS <<< "$BROKER_LIST"

echo "Waiting for Kafka brokers: ${BROKERS[*]}"
count=0
while :; do
  for b in "${BROKERS[@]}"; do
    echo "  Trying broker: $b"
    if docker run --rm edenhill/kcat:1.7.1 -b "$b" -L >/dev/null 2>&1; then
      echo "Kafka is available at $b"
      exit 0
    else
      echo "    No response from $b"
    fi
  done

  count=$((count+1))
  if [ "$count" -ge "$RETRIES" ]; then
    echo "Kafka not available after $RETRIES attempts, giving up"
    exit 1
  fi
  sleep "$DELAY"
done