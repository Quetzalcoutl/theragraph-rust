#!/usr/bin/env sh
set -euo pipefail

# Debug wrapper: collect env and run the engine while capturing stdout/stderr
LOG=/tmp/startup.log
echo "=== STARTUP DIAGNOSTICS ===" > "$LOG"
echo "Timestamp: $(date -u +'%Y-%m-%dT%H:%M:%SZ')" >> "$LOG"

# Dump selected env vars useful for debugging
echo "Environment:" >> "$LOG"
env | sort >> "$LOG" 2>&1

# Log pwd and ls
echo "\nWorking dir: $(pwd)" >> "$LOG"
ls -la >> "$LOG" 2>&1 || true

# Run the engine and capture output
echo "\n--- RUN /app/theragraph-engine ---" >> "$LOG"
/app/theragraph-engine >> "$LOG" 2>&1 || RC=$?
RC=${RC:-0}
echo "--- EXIT CODE: $RC ---" >> "$LOG"

# Print log to stdout for immediate visibility
cat "$LOG" || true

# Keep container alive long enough for inspection
sleep 3600
exit "$RC"
