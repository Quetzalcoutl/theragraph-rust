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

RETRIES=${2:-60}
DELAY=${3:-1}

IFS=',' read -r -a BROKERS <<< "$BROKER_LIST"

echo "Waiting for Kafka brokers: ${BROKERS[*]}"
count=0
while :; do
  for b in "${BROKERS[@]}"; do
    # split host:port
    host=$(echo "$b" | awk -F: '{print $1}')
    port=$(echo "$b" | awk -F: '{print $2}')
    echo "  Trying broker: $host:$port"

    # Prefer nc if available
    if command -v nc >/dev/null 2>&1; then
      if nc -z -w 3 "$host" "$port" >/dev/null 2>&1; then
        echo "Kafka is available at $host:$port (nc)"
        exit 0
      fi
    else
      # Fallback to bash /dev/tcp
      if (timeout 3 bash -c "cat < /dev/tcp/$host/$port" ) >/dev/null 2>&1; then
        echo "Kafka is available at $host:$port (/dev/tcp)"
        exit 0
      fi
    fi

    echo "    No response from $host:$port"
  done

  count=$((count+1))
  if [ "$count" -ge "$RETRIES" ]; then
    echo "Kafka not available after $RETRIES attempts, giving up"
    exit 1
  fi

  # exponential backoff with cap
  backoff=$((DELAY * count))
  if [ "$backoff" -gt 30 ]; then
    backoff=30
  fi
  echo "Waiting ${backoff}s before next attempt..."
  sleep "$backoff"
done