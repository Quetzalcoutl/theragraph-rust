# TheraGraph Rust Engine

High-performance blockchain indexer written in Rust.

## Architecture

This service:
1. Connects to Ethereum/Sepolia RPC
2. Indexes NFT events (Mints, Transfers, Likes, etc.)
3. Publishes events to Kafka topics
4. Writes processed data to PostgreSQL

## Communication Flow

```
Blockchain → Rust Engine → Kafka → Elixir API → Frontend
                         ↓
                    PostgreSQL
```

## Setup

### Prerequisites
- Rust 1.70+
- Colima/Docker (for infrastructure services)
- **Infrastructure Services**: Kafka, Zookeeper, and PostgreSQL

> **⚠️ Important**: Before running this service, you must start the infrastructure services.
> See [INFRASTRUCTURE.md](../INFRASTRUCTURE.md) for setup instructions.

**Quick Start Infrastructure**:
```bash
cd /Users/dereck/theragraph
./scripts/start-infrastructure.sh
```

### Installation

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build the project
cargo build --release

# Run
cargo run
```

## Configuration

Create a `.env` file:

```env
# Blockchain
RPC_URL=https://ethereum-sepolia-rpc.publicnode.com
CHAIN_ID=11155111
START_BLOCK=9884750
POLL_INTERVAL_MS=2000

# Kafka
KAFKA_BROKERS=localhost:9092

# Database
DATABASE_URL=postgres://postgres:postgres@localhost:5432/theragraph_dev

# Contracts
THERA_SNAP_ADDRESS=0x8CfB9Ea2fbeFf78Ee4423267De512971FE1375EC
THERA_ART_ADDRESS=0xa57A08769C26914a6Fd4344fCD8de90fb80a79B8
THERA_MUSIC_ADDRESS=0x252eacC3AcD39790D6EDDbCE90e8EBA30F031d9c
THERA_FLIX_ADDRESS=0xf661ddE72f3dcC7b16b1Df8D7d8864619049fD75
```

## Deployment

```bash
docker build -t theragraph-rust .
docker run --env-file .env theragraph-rust

## Integration notes

- See `docs/graph-ingest.md` for guidance on partitioning, batching and idempotent ingestion to the graph DB.
- Example consumer skeleton: `examples/consumer_skeleton.rs` demonstrates grouping by partition and producing batched mutations.
```
