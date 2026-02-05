# Handoff: Citrea Dashboard — Fee Accuracy + Pre-Launch Data

## What's in this tarball

`citrea-dev.tar.gz` contains the full Citrea analytics project. Extract it and rename `data/citrea-dev.db` to `data/citrea.db`. The database has 3,019 batches — 2,684 pre-launch (before Jan 27 2026) and 335 post-launch.

## What's already done: Pre-launch data support

The web dashboard (`web/`) already has a working pre-launch toggle. Changes span:

- **web/server.js**: `LAUNCH_BATCH_INDEX` computed at startup (batch 2685). All API endpoints (`/api/overview`, `/api/batches`, `/api/charts/*`) accept `?prelaunch=1`. When set and period is `all`, the `MAINNET_TS` floor is dropped to include all data. Averages always use `CASE WHEN btc_block_time >= 1769472000` so pre-launch data is excluded from averages but included in totals. Chart endpoints return `{ data: [...], launch_batch_index: N }` wrapper format.
- **web/public/index.html**: Toggle switch next to period buttons.
- **web/public/app.js**: `showPrelaunch` state persisted in `localStorage`. Toggle destroys and recreates all charts (annotations are init-only in ApexCharts). Green dashed "Launch" annotation line at batch 2685. Table rows with `is_pre_launch=1` get green left border and "(pre-launch)" badge.
- **web/public/style.css**: CSS toggle switch, `.pre-launch-row`, `.pre-launch-badge`, `.prelaunch-indicator` styles.
- **web/public/chart.html**: Popout charts have their own prelaunch toggle, read from URL params.
- **docker-compose.yml**: `dashboard-dev` service on port 3001.

This feature is complete and tested. The dashboard should work as-is with `docker compose up dashboard-dev`.

## What still needs to be done: Commit tx fee tracking

### The problem

Citrea batch commitments use a Bitcoin commit/reveal pattern (like inscriptions). Two transactions per batch:

1. **Commit tx**: Creates a taproot output committing to a script containing the batch data. Pays a miner fee.
2. **Reveal tx**: Spends that commit output, revealing the batch data in the witness. Pays a miner fee.

The daemon currently only tracks the reveal tx fee as `btc_fee_sats`. The commit tx fee is not recorded. This means roughly half the on-chain cost of each batch commitment is missing from the totals.

The same pattern likely applies to proof transactions — there's a commit tx before each proof reveal — but verify this.

### Reference implementation

The repo at https://github.com/stutxo/rollupfees.com (cloned to `/home/ubuntu/rollupfees.com` on the original machine, not in the tarball) handles this. Their approach in `src/main.rs`:

- They match the reveal tx, then call `get_related_txids` which returns all input txids (`tx.input.iter().map(|i| i.previous_output.txid).collect()`)
- They look up those parent txs in the same block (`block_txs.get(&txid)`) and calculate their fees too
- **Limitation**: they only find the commit tx if it's in the same block as the reveal. If it landed in an earlier block, they miss it.
- They do **NOT** track proof fees at all — only batch commitment reveals.

### How the Rust daemon works (key files)

- **src/daemon.rs**: Main loop with pipelines — block scanning, fee enrichment, proof enrichment, L2 tx counting, gas counting. The block scanning pipeline calls `find_sequencer_tx_in_block()` and stores results.
- **src/bitcoin_rpc.rs**: `find_sequencer_tx_in_block()` (line ~130) scans a block for txs sending to the sequencer address, decodes the batch data, calculates fee as `inputs - outputs`. `get_proof_tx_info()` (line ~211) does the same for proof txs.
- **src/db.rs**: SQLite schema and queries. Has a migration system — `migrate()` runs on every startup and does idempotent `ALTER TABLE ADD COLUMN`. This is how you add new columns.
- **src/citrea_decoder.rs**: Decodes batch commitment data from witness/transaction hex.
- **src/bin/citread.rs**: The daemon binary entrypoint.
- **Dockerfile.daemon**: Two-stage build (`rust:1.83-bookworm` builder, `debian:bookworm-slim` runtime).

### Key constants

- **Sequencer address**: `bc1pqqjrzkc79qkgys40yfu48s8y04d4zud26cnt48qd98mywl9l70zsadyvz5`
- **Prover address**: `bc1ppuj4hke08g3qjqlspdud8mkymgfq6y9l4ud4vaglj7fc5k3rrudqm5947j`
- **MAINNET_TS** = `1769472000` (Jan 27 2026 00:00:00 UTC)

## What to implement

1. **Add columns** to commitments table via the migration system in `db.rs:migrate()`:
   - `commit_fee_sats INTEGER` — fee paid by the commit tx
   - `commit_txid TEXT` — txid of the commit tx
   - Possibly same for proofs: `proof_commit_fee_sats`, `proof_commit_txid`

2. **Modify `find_sequencer_tx_in_block()`** in `bitcoin_rpc.rs` (or add a new function): After finding the reveal tx, look at its inputs, fetch the parent tx(es), and calculate the commit tx fee. The commit tx is the one that created the taproot output being spent. Unlike rollupfees.com, don't limit to same-block — use `getrawtransaction` to fetch the parent tx even if it's in an earlier block.

3. **Add an enrichment pipeline** in `daemon.rs`: Similar to the proof enrichment pipeline — query batches where `commit_fee_sats IS NULL` and `btc_txid IS NOT NULL`, fetch the reveal tx, find its parent commit tx, calculate fee, write back. This backfills all 3,019 existing batches.

4. **Update the dashboard** (`web/server.js`): Include `commit_fee_sats` in the total fee calculations. Currently `total_btc_fees = btc_fee_sats + proof_fee_sats`. It should become `btc_fee_sats + commit_fee_sats + proof_fee_sats + proof_commit_fee_sats` (or however the columns end up named).

5. **Update the table display** (`web/public/app.js`): Add commit fee column or fold it into the existing fee display.

## Environment requirements

- Docker and Docker Compose
- Access to a Bitcoin Core RPC node (the daemon needs `BTC_RPC_HOST`, `BTC_RPC_USER`, `BTC_RPC_PASS` env vars — check `docker-compose.yml` for defaults)
- Rust toolchain if building outside Docker
- 2-4GB RAM minimum (Rust compilation is the bottleneck, runtime is ~85MB total)

## How to run

```bash
tar xzf citrea-dev.tar.gz
cd citreaanalysis
mv data/citrea-dev.db data/citrea.db
# Edit docker-compose.yml to point BTC_RPC_HOST at your Bitcoin node
docker compose up dashboard-dev -d  # dashboard on :3001
docker compose build daemon         # after Rust changes
docker compose up daemon -d         # runs against data/citrea.db
```

---

That should give the new session full context to continue both features.
