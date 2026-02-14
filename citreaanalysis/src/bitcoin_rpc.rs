use crate::citrea_decoder;
use crate::error::Result;
use bitcoincore_rpc::{Auth, Client, RpcApi};
use serde::Deserialize;

/// The known Citrea sequencer address on Bitcoin mainnet
pub const CITREA_SEQUENCER_ADDRESS: &str =
    "bc1pqqjrzkc79qkgys40yfu48s8y04d4zud26cnt48qd98mywl9l70zsadyvz5";

/// The known Citrea prover address on Bitcoin mainnet
pub const CITREA_PROVER_ADDRESS: &str =
    "bc1ppuj4hke08g3qjqlspdud8mkymgfq6y9l4ud4vaglj7fc5k3rrudqm5947j";

/// A sequencer transaction found in a Bitcoin block
#[derive(Debug)]
pub struct FoundSequencerTx {
    pub txid: String,
    pub batch_number: u32,
    pub fee_sats: u64,
    pub weight: u32,
    pub block_time: u64,
    pub merkle_root: String,
    pub l2_end_block: u64,
}

/// Commit (parent) transaction info retrieved from Bitcoin Core
#[derive(Debug, Clone)]
pub struct CommitTxInfo {
    pub commit_txid: String,
    pub fee_sats: u64,
    pub weight: u32,
    pub block_time: u64,
}

/// Verified proof transaction info retrieved from Bitcoin Core
#[derive(Debug)]
pub struct VerifiedProofTx {
    pub txid: String,
    pub fee_sats: u64,
    pub weight: u32,
    pub block_time: u64,
    /// Whether the tx actually sends to the expected prover address
    pub sends_to_prover: bool,
}

/// Bitcoin Core RPC client for fee enrichment
pub struct BitcoinCoreClient {
    client: Client,
}

/// Block response from `getblock` with verbosity=2 (decoded transactions, no prevout data)
#[derive(Debug, Deserialize)]
struct VerboseBlock {
    #[serde(default)]
    tx: Vec<VerboseTx>,
    time: u64,
}

#[derive(Debug, Deserialize)]
struct VerboseTx {
    txid: String,
    hex: String,
    weight: u64,
    #[allow(dead_code)]
    vin: Vec<VerboseVin>,
    vout: Vec<VerboseVout>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct VerboseVin {
    #[serde(default)]
    txid: Option<String>,
    #[serde(default)]
    vout: Option<u32>,
    #[serde(default)]
    prevout: Option<VerbosePrevout>,
    #[serde(default)]
    coinbase: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VerbosePrevout {
    value: f64,
}

#[derive(Debug, Deserialize)]
struct VerboseVout {
    value: f64,
    #[serde(rename = "scriptPubKey")]
    script_pub_key: ScriptPubKey,
}

#[derive(Debug, Deserialize)]
struct ScriptPubKey {
    address: Option<String>,
}

/// Response from `scanblocks` RPC (Bitcoin Core v25+, requires blockfilterindex=1)
#[derive(Debug, Deserialize)]
struct ScanBlocksResult {
    relevant_blocks: Vec<String>, // block hashes
}

/// Response from `getblockheader` with verbosity=1 (JSON)
#[derive(Debug, Deserialize)]
struct BlockHeader {
    height: u64,
}

/// Response from `getrawtransaction` with verbosity=2 (includes prevout)
#[derive(Debug, Deserialize)]
struct RawTxVerbose {
    txid: String,
    weight: u64,
    vin: Vec<VerboseVin>,
    vout: Vec<VerboseVout>,
    #[serde(default)]
    blocktime: Option<u64>,
}

/// Convert a BTC float value (as returned by Bitcoin Core JSON-RPC) to integer satoshis.
fn btc_to_sats(btc: f64) -> u64 {
    (btc * 100_000_000.0).round() as u64
}

/// Compute transaction fee: sum(input prevout values) - sum(output values) in satoshis.
fn compute_tx_fee(vin: &[VerboseVin], vout: &[VerboseVout]) -> u64 {
    let input_sats: u64 = vin
        .iter()
        .filter_map(|v| {
            if v.coinbase.is_some() {
                return None;
            }
            v.prevout.as_ref().map(|p| btc_to_sats(p.value))
        })
        .sum();
    let output_sats: u64 = vout.iter().map(|v| btc_to_sats(v.value)).sum();
    input_sats.saturating_sub(output_sats)
}

impl BitcoinCoreClient {
    /// Create a new Bitcoin Core RPC client
    pub fn new(host: &str, user: &str, pass: &str) -> Result<Self> {
        let url = if host.contains("://") {
            host.to_string()
        } else {
            format!("http://{}", host)
        };
        // Ensure port is included
        let url = if url.contains(':') && url.matches(':').count() >= 2 {
            url
        } else {
            format!("{}:8332", url)
        };
        log::info!("Connecting to Bitcoin Core at {}...", url);
        let client = Client::new(&url, Auth::UserPass(user.to_string(), pass.to_string()))?;

        // Test the connection
        let info = client.get_blockchain_info()?;
        log::info!(
            "Bitcoin Core connected: chain={}, blocks={}",
            info.chain,
            info.blocks
        );

        Ok(Self { client })
    }

    /// Get the current Bitcoin block height via `getblockchaininfo`
    pub fn get_block_height(&self) -> Result<u64> {
        let info = self.client.get_blockchain_info()?;
        Ok(info.blocks)
    }

    /// Find sequencer transactions in a specific Bitcoin block.
    ///
    /// Uses a two-phase strategy for speed:
    /// 1. `getblock` with verbosity=2 (no prevout) to scan for sequencer outputs (~0.5-1s)
    /// 2. `getrawtransaction` with verbosity=2 only on matching txs to get prevout for fee calc
    ///
    /// This is ~10x faster than verbosity=3 on typical mainnet blocks, since prevout
    /// data is only fetched for the rare blocks containing sequencer transactions.
    pub fn find_sequencer_tx_in_block(
        &self,
        l1_height: u64,
        sequencer_address: &str,
    ) -> Result<Vec<FoundSequencerTx>> {
        let block_hash = self.client.get_block_hash(l1_height)?;

        // Phase 1: getblock with verbosity=2 — decoded txs WITHOUT prevout data.
        // This is enough to check output addresses and decode commitments from tx hex.
        let block: VerboseBlock = self.client.call(
            "getblock",
            &[
                serde_json::to_value(block_hash.to_string())?,
                serde_json::to_value(2u32)?,
            ],
        )?;

        let block_time = block.time;
        let mut results = Vec::new();

        for tx in &block.tx {
            // Check if any output goes to the sequencer address
            let sends_to_sequencer = tx.vout.iter().any(|vout| {
                vout.script_pub_key
                    .address
                    .as_deref()
                    == Some(sequencer_address)
            });

            if !sends_to_sequencer {
                continue;
            }

            // Try to decode the commitment from the raw transaction hex
            let commitment = match citrea_decoder::decode_from_tx_hex(&tx.txid, &tx.hex) {
                Ok(c) => c,
                Err(_) => continue, // Not a valid Citrea commitment, skip
            };
            let batch_number = commitment.body.batch_number;
            let merkle_root = commitment.body.state_commitment.clone();
            let l2_end_block = commitment.body.l2_block_height as u64;

            // Phase 2: fetch this specific tx with prevout data for fee calculation.
            // getrawtransaction verbosity=2 includes prevout and is fast (~0.1s).
            let full_tx: RawTxVerbose = self.client.call(
                "getrawtransaction",
                &[
                    serde_json::to_value(&tx.txid)?,
                    serde_json::to_value(2u32)?,
                ],
            )?;
            let fee_sats = compute_tx_fee(&full_tx.vin, &full_tx.vout);

            results.push(FoundSequencerTx {
                txid: tx.txid.clone(),
                batch_number,
                fee_sats,
                weight: tx.weight as u32,
                block_time,
                merkle_root,
                l2_end_block,
            });
        }

        Ok(results)
    }

    /// Get proof transaction info from Bitcoin Core using `getrawtransaction` with verbosity=2.
    ///
    /// Returns fee, weight, block time, and whether the tx sends to the prover address.
    /// This is much faster than fetching the entire block (~0.1s vs ~10s per call).
    pub fn get_proof_tx_info(
        &self,
        txid_hex: &str,
        prover_address: &str,
    ) -> Result<VerifiedProofTx> {
        let tx: RawTxVerbose = self.client.call(
            "getrawtransaction",
            &[
                serde_json::to_value(txid_hex)?,
                serde_json::to_value(2u32)?,
            ],
        )?;

        let sends_to_prover = tx.vout.iter().any(|vout| {
            vout.script_pub_key.address.as_deref() == Some(prover_address)
        });

        let fee_sats = compute_tx_fee(&tx.vin, &tx.vout);

        Ok(VerifiedProofTx {
            txid: tx.txid,
            fee_sats,
            weight: tx.weight as u32,
            block_time: tx.blocktime.unwrap_or(0),
            sends_to_prover,
        })
    }

    /// Use `scanblocks` (Bitcoin Core v25+, requires `blockfilterindex=1`) to find
    /// blocks in a height range that likely contain transactions involving `address`.
    ///
    /// Returns a sorted `Vec<u64>` of block heights. May include false positives
    /// from BIP158 compact block filters — callers must still inspect the blocks.
    /// Returns an error if the node doesn't support scanblocks or lacks the index.
    pub fn scan_blocks_for_address(
        &self,
        address: &str,
        start_height: u64,
        stop_height: u64,
    ) -> Result<Vec<u64>> {
        let scan_object =
            serde_json::json!([{ "desc": format!("addr({})", address) }]);

        let result: ScanBlocksResult = self.client.call(
            "scanblocks",
            &[
                serde_json::to_value("start")?,
                scan_object,
                serde_json::to_value(start_height)?,
                serde_json::to_value(stop_height)?,
            ],
        )?;

        let mut heights = Vec::with_capacity(result.relevant_blocks.len());
        for block_hash in &result.relevant_blocks {
            heights.push(self.get_block_header_height(block_hash)?);
        }
        heights.sort_unstable();
        Ok(heights)
    }

    /// Get the height of a block by its hash using `getblockheader` (verbosity=1).
    /// Very cheap (~1ms per call).
    fn get_block_header_height(&self, block_hash: &str) -> Result<u64> {
        let header: BlockHeader = self.client.call(
            "getblockheader",
            &[
                serde_json::to_value(block_hash)?,
                serde_json::to_value(true)?,
            ],
        )?;
        Ok(header.height)
    }

    /// Get the commit (parent) transaction info for a reveal transaction.
    ///
    /// The commit tx is the parent of the reveal tx, found via `vin[0].txid`.
    /// Fetches it with `getrawtransaction` verbosity=2, then calculates the fee.
    pub fn get_commit_tx_info(&self, reveal_txid: &str) -> Result<CommitTxInfo> {
        // Fetch the reveal tx to find its parent (commit) txid
        let reveal_tx: RawTxVerbose = self.client.call(
            "getrawtransaction",
            &[
                serde_json::to_value(reveal_txid)?,
                serde_json::to_value(2u32)?,
            ],
        )?;

        let commit_txid = reveal_tx
            .vin
            .first()
            .and_then(|vin| vin.txid.as_ref())
            .ok_or_else(|| format!("Reveal tx {} has no vin[0].txid", reveal_txid))?
            .clone();

        // Fetch the commit tx
        let commit_tx: RawTxVerbose = self.client.call(
            "getrawtransaction",
            &[
                serde_json::to_value(&commit_txid)?,
                serde_json::to_value(2u32)?,
            ],
        )?;

        let fee_sats = compute_tx_fee(&commit_tx.vin, &commit_tx.vout);

        Ok(CommitTxInfo {
            commit_txid,
            fee_sats,
            weight: commit_tx.weight as u32,
            block_time: commit_tx.blocktime.unwrap_or(0),
        })
    }
}
