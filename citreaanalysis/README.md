# Citrea Dashboard

An independent analytics dashboard for [Citrea](https://citrea.xyz), a ZK-rollup that settles on Bitcoin. Not affiliated with or endorsed by the Citrea team.

Tracks batch commitments, ZK proofs, Bitcoin settlement fees, L2 transaction volume, gas utilization, and network anomalies.

## Architecture

Two components:

- **Daemon** (`citread`) — A Rust service that continuously scans Bitcoin blocks for Citrea sequencer and prover transactions, enriches each batch with L2 data from Citrea's RPC, and writes everything to a local SQLite database.
- **Dashboard** (`web/`) — A Node.js server and static frontend that queries the database and auto-refreshes every 60 seconds.

### Data sources

- **Bitcoin Core** — block and transaction data via `getblock` / `getrawtransaction` RPC calls. Used to discover commitment and proof transactions and compute on-chain fees.
- **Citrea JSON-RPC** (`rpc.mainnet.citrea.xyz`) — L2 block data, transaction counts, gas usage, and sync status.
- **Citrea Batch Explorer** (`batch-explorer.mainnet.citrea.xyz`) — proof-to-batch mappings.

## Requirements

- Docker and Docker Compose
- Access to a Bitcoin Core full node with RPC enabled
- Rust toolchain (if building outside Docker)

## Setup

1. Clone the repo and create a `.env` file:

```bash
cp .env.example .env  # then fill in your values
```

Required environment variables:

| Variable | Description |
|---|---|
| `BTC_RPC_HOST` | Bitcoin Core RPC host and port (e.g. `127.0.0.1:8332`) |
| `BTC_RPC_USER` | Bitcoin Core RPC username |
| `BTC_RPC_PASS` | Bitcoin Core RPC password |
| `SPARK_MNEMONIC` | *(optional)* BIP39 mnemonic for Spark Lightning payments. If unset, all data is freely available. |
| `SPARK_NETWORK` | *(optional)* `MAINNET` or `REGTEST`. Defaults to `MAINNET`. |

2. Start the services:

```bash
docker compose up -d
```

The dashboard will be available at `http://localhost:3000`. The daemon will begin scanning Bitcoin blocks and populating the database.

## Development

Build and run outside Docker:

```bash
# Daemon
cargo build --release
RUST_LOG=info BTC_RPC_HOST=... BTC_RPC_USER=... BTC_RPC_PASS=... ./target/release/citread

# Dashboard
cd web
npm install
DB_PATH=../data/citrea.db node server.js
```

## Known Limitations

See [KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md).

## License

[MIT](LICENSE)
