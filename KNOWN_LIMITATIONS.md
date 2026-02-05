# Known Limitations

## Fees not tracked

### CPFP bump transactions

Citrea's fee service (`bitcoin-da/src/fee.rs`) can broadcast Child-Pays-For-Parent
transactions when a commit or reveal is stuck in the mempool. The CPFP transaction
spends the change output of the stuck transaction to boost its effective fee rate.
Its miner fee is an additional settlement cost not captured by the dashboard.

### Chunk transaction fees

If a batch proof exceeds 397KB (`MAX_TX_BODY_SIZE`), Citrea splits it into multiple
chunk transactions (type `0x02`), each with its own commit/reveal pair. An aggregate
transaction (type `0x01`) then ties the chunks together. The dashboard records only
the fee of the main proof transaction (Complete or Aggregate), not the individual
chunk commit/reveal fees.

### BatchProofMethodId transaction fees

Security council method-ID update transactions (type `0x03`) also use the
commit/reveal pattern and pay miner fees. These governance transactions are not
tracked.

## Data source dependencies

### Batch Explorer API

Proof-to-batch mappings are fetched from Citrea's Batch Explorer API at
`batch-explorer.mainnet.citrea.xyz/proofs`. This is a centralized service operated
by Citrea. All other data sources (Bitcoin Core for on-chain fees, Citrea JSON-RPC
for L2 metrics) are decentralized node RPCs. If the Batch Explorer API returns
incorrect mappings or becomes unavailable, proof fee enrichment will stall or
produce incorrect per-batch allocations.

## Precision

### Float-to-integer satoshi conversion

Bitcoin Core's JSON-RPC returns transaction values as floating-point BTC amounts.
These are converted to integer satoshis via `(btc * 100_000_000.0).round() as u64`
in `bitcoin_rpc.rs`. The `.round()` handles most precision issues, but
floating-point arithmetic can theoretically produce off-by-one-sat errors on
individual transactions.
