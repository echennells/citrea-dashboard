use serde::{Deserialize, Serialize};
use std::error::Error;

/// The known Citrea sequencer address on Bitcoin mainnet
pub const CITREA_SEQUENCER_ADDRESS: &str =
    "bc1pqqjrzkc79qkgys40yfu48s8y04d4zud26cnt48qd98mywl9l70zsadyvz5";

/// Mempool.space API client for querying Bitcoin transactions
pub struct MempoolClient {
    base_url: String,
    client: reqwest::blocking::Client,
}

/// Address statistics from mempool.space
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AddressStats {
    pub address: String,
    pub chain_stats: ChainStats,
    pub mempool_stats: MempoolStats,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChainStats {
    pub funded_txo_count: u64,
    pub funded_txo_sum: u64,
    pub spent_txo_count: u64,
    pub spent_txo_sum: u64,
    pub tx_count: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MempoolStats {
    pub funded_txo_count: u64,
    pub funded_txo_sum: u64,
    pub spent_txo_count: u64,
    pub spent_txo_sum: u64,
    pub tx_count: u64,
}

/// Transaction from mempool.space API
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MempoolTransaction {
    pub txid: String,
    pub version: u32,
    pub locktime: u32,
    pub vin: Vec<Vin>,
    pub vout: Vec<Vout>,
    pub size: u32,
    pub weight: u32,
    pub fee: u64,
    pub status: TxStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Vin {
    pub txid: String,
    pub vout: u32,
    pub prevout: Option<Prevout>,
    pub scriptsig: String,
    pub scriptsig_asm: String,
    pub witness: Option<Vec<String>>,
    pub is_coinbase: bool,
    pub sequence: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Prevout {
    pub scriptpubkey: String,
    pub scriptpubkey_asm: String,
    pub scriptpubkey_type: String,
    pub scriptpubkey_address: Option<String>,
    pub value: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Vout {
    pub scriptpubkey: String,
    pub scriptpubkey_asm: String,
    pub scriptpubkey_type: String,
    pub scriptpubkey_address: Option<String>,
    pub value: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TxStatus {
    pub confirmed: bool,
    pub block_height: Option<u64>,
    pub block_hash: Option<String>,
    pub block_time: Option<u64>,
}

impl MempoolClient {
    /// Create a new client with default mempool.space URL
    pub fn new() -> Self {
        Self::with_base_url("https://mempool.space/api")
    }

    /// Create a new client with a custom base URL
    pub fn with_base_url(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Make a GET request with 429 retry and exponential backoff.
    fn get_with_retry(&self, url: &str) -> Result<reqwest::blocking::Response, Box<dyn Error>> {
        let mut delay_ms = 1000u64;
        for attempt in 0..5u32 {
            let response = self.client.get(url).send()?;
            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if attempt < 4 {
                    log::warn!("429 rate limited on {}, retrying in {}ms", url, delay_ms);
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    delay_ms *= 2;
                    continue;
                }
                return Err("mempool.space rate limit (429) after 5 retries".into());
            }
            if !response.status().is_success() {
                return Err(format!("API error: {} for {}", response.status(), url).into());
            }
            return Ok(response);
        }
        unreachable!()
    }

    /// Get address statistics
    pub fn get_address_stats(&self, address: &str) -> Result<AddressStats, Box<dyn Error>> {
        let url = format!("{}/address/{}", self.base_url, address);
        let response = self.get_with_retry(&url)?;
        let stats: AddressStats = response.json()?;
        Ok(stats)
    }

    /// Get transactions for an address (returns up to 50 at a time)
    pub fn get_address_txs(&self, address: &str) -> Result<Vec<MempoolTransaction>, Box<dyn Error>> {
        let url = format!("{}/address/{}/txs", self.base_url, address);
        let response = self.get_with_retry(&url)?;
        let txs: Vec<MempoolTransaction> = response.json()?;
        Ok(txs)
    }

    /// Get transactions for an address with pagination (using last_seen_txid)
    pub fn get_address_txs_after(
        &self,
        address: &str,
        last_seen_txid: &str,
    ) -> Result<Vec<MempoolTransaction>, Box<dyn Error>> {
        let url = format!(
            "{}/address/{}/txs/chain/{}",
            self.base_url, address, last_seen_txid
        );
        let response = self.get_with_retry(&url)?;
        let txs: Vec<MempoolTransaction> = response.json()?;
        Ok(txs)
    }

    /// Get all transactions for an address (handles pagination)
    pub fn get_all_address_txs(
        &self,
        address: &str,
        progress_callback: Option<&dyn Fn(usize)>,
    ) -> Result<Vec<MempoolTransaction>, Box<dyn Error>> {
        let mut all_txs = Vec::new();
        let mut last_txid: Option<String> = None;

        loop {
            let txs = match &last_txid {
                None => self.get_address_txs(address)?,
                Some(txid) => self.get_address_txs_after(address, txid)?,
            };

            if txs.is_empty() {
                break;
            }

            last_txid = txs.last().map(|tx| tx.txid.clone());
            all_txs.extend(txs);

            if let Some(callback) = progress_callback {
                callback(all_txs.len());
            }

            // Rate limiting - be nice to the API
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Ok(all_txs)
    }

    /// Get raw transaction hex
    pub fn get_tx_hex(&self, txid: &str) -> Result<String, Box<dyn Error>> {
        let url = format!("{}/tx/{}/hex", self.base_url, txid);
        let response = self.get_with_retry(&url)?;
        let hex = response.text()?;
        Ok(hex)
    }

    /// Get transaction details
    pub fn get_tx(&self, txid: &str) -> Result<MempoolTransaction, Box<dyn Error>> {
        let url = format!("{}/tx/{}", self.base_url, txid);
        let response = self.get_with_retry(&url)?;
        let tx: MempoolTransaction = response.json()?;
        Ok(tx)
    }
}

impl Default for MempoolClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper to extract witness data from a transaction
pub fn extract_witness_hex(tx: &MempoolTransaction) -> Option<Vec<String>> {
    tx.vin.first().and_then(|vin| vin.witness.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires network access
    fn test_get_address_stats() {
        let client = MempoolClient::new();
        let stats = client
            .get_address_stats(CITREA_SEQUENCER_ADDRESS)
            .expect("Failed to get stats");
        println!("Total transactions: {}", stats.chain_stats.tx_count);
        assert!(stats.chain_stats.tx_count > 0);
    }
}
