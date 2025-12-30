# Graph ingestion & Kafka (theragraph-rust)

Purpose

This document describes how `theragraph-rust` will format events, batch/deduplicate them and write to the graph database service.

Event schema & partitioning

- Use canonical events from `Theragraph-Database/docs/kafka-event-schemas.md`.
- Compute `partition_id` (e.g., hash(actor_id) % N) and attach to events so downstream writes can be coalesced by partition.

Idempotency & batching

- Each event should have `event_id` (uuid). Consumers maintain a small in-memory window of seen `event_id`s per partition to deduplicate, with longer-term dedupe stored in DB or RocksDB if needed.
- Batch events per partition (e.g., 256 or time-windowed) and transform them into a single mutation request for the graph DB.

Backpressure & metrics

- Expose per-partition metrics (lag, pending batch size, error rates).
- Use backpressure (pause poll) when DB leader is overloaded.

Local skeleton (example)

- Add `examples/consumer_skeleton.rs` demonstrating how to consume a small local queue and flush batches to an HTTP GraphQL endpoint (Dgraph) or Bolt (Neo4j).

Testing

- Add integration tests that run a local Dgraph/Neo4j via docker-compose and assert eventual consistency of writes.

Next steps

- Implement `examples/consumer_skeleton.rs` and a scripted local run to demonstrate batched idempotent writes.

- Example Nebula consumer: `examples/consumer_nebula.rs` shows a simple local PoC that groups events by partition, deduplicates and builds NGQL statements, then executes them through the `nebula-console` via `docker compose exec`.

Notes on idempotency

- The example demonstrates a simple idempotency pattern for `follow` events by deleting any existing edge with the same `event_id` and then inserting the edge with `event_id` as a property; this avoids duplicate edges for retried events.
- For production, prefer transactional / upsert APIs or use a dedupe store (RocksDB, Redis) and stronger guarantees.
