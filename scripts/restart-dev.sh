#!/bin/bash
# Restart Rust service with shared INTERNAL_API_KEY (matches Elixir + Next.js)
SHARED_KEY=$(grep '^INTERNAL_API_KEY=' /Users/dereck/thera/theragraph/theragraph-indexer/.env | cut -d= -f2)
cd /Users/dereck/thera/theragraph/theragraph-rust
pkill -TERM -f "target/release/theragraph" 2>/dev/null || true
sleep 2
INTERNAL_API_KEY="$SHARED_KEY" nohup ./target/release/theragraph > /tmp/theragraph-rust.log 2>&1 &
echo "PID $!"
