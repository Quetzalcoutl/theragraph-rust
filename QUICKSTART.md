# Quick Start Guide - TheraGraph Rust Engine

## Prerequisites
- Rust 1.70+ installed
- Docker & Docker Compose
- PostgreSQL access
- Sepolia RPC URL

## Setup Steps

### 1. Install Rust (if needed)
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### 2. Configure Environment
```bash
cd theragraph-rust
cp .env.example .env
```

Edit `.env` and add your TheraFriend contract address:
```env
THERA_FRIEND_ADDRESS=0xYOUR_ACTUAL_ADDRESS_HERE
```

### 3. Start Infrastructure
```bash
docker-compose up -d
```

This starts:
- Kafka (localhost:9092)
- Zookeeper (localhost:2181)
- Kafka UI (http://localhost:8080)
- PostgreSQL (localhost:5432)

### 4. Build & Run
```bash
# Development
cargo run

# Production (optimized)
cargo build --release
./target/release/theragraph-engine
```

## Monitoring

### Kafka UI
Visit http://localhost:8080 to see:
- Topics: `blockchain.events`, `user.actions`
- Messages flowing in real-time
- Consumer lag

### Logs
The Rust engine outputs structured logs:
```
🚀 TheraGraph Rust Engine starting...
✅ Configuration loaded
✅ Kafka producer initialized
✅ Database connection established
🔍 Starting blockchain indexers...
📸 SnapIndexer started for contract: 0x8CfB...
🎨 ArtIndexer started for contract: 0xa57A...
🎵 MusicIndexer started for contract: 0x252e...
🎬 FlixIndexer started for contract: 0xf661...
👥 FriendIndexer started for contract: 0xYOUR...
✅ All indexers started
```

## Deployment to Production

### Docker Build
```bash
docker build -t theragraph-rust:latest .
docker run --env-file .env theragraph-rust:latest
```

### Coolify Deployment
1. Create new service in Coolify
2. Point to `theragraph-rust` directory
3. Set environment variables
4. Deploy!

## Troubleshooting

### "Kafka connection failed"
- Ensure `docker-compose up -d` is running
- Check `KAFKA_BROKERS=localhost:9092` in `.env`

### "Database connection failed"
- Verify `DATABASE_URL` points to correct PostgreSQL
- Ensure database `theragraph_dev` exists

### "Contract address error"
- Double-check all 5 contract addresses in `.env`
- Ensure they match your deployed contracts on Sepolia
