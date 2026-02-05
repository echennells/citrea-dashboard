use serde::{Deserialize, Serialize};
use std::error::Error;

pub const CITREA_RPC_URL: &str = "https://rpc.mainnet.citrea.xyz";
pub const BATCH_EXPLORER_API_URL: &str = "https://batch-explorer.mainnet.citrea.xyz";

/// Client for the Citrea L2 JSON-RPC API
pub struct CitreaRpcClient {
    url: String,
    client: reqwest::blocking::Client,
}

/// Sync status returned by citrea_syncStatus
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SyncStatus {
    #[serde(rename = "l1Status")]
    pub l1_status: L1SyncStatus,
    #[serde(rename = "l2Status")]
    pub l2_status: L2SyncStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct L1SyncStatus {
    /// Last synced L1 block height (hex string like "0xe4168")
    pub synced: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct L2SyncStatus {
    /// Last synced L2 block height (hex string)
    pub synced: String,
}

/// A sequencer commitment as returned by the Citrea RPC
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SequencerCommitment {
    /// Merkle root of the batch (hex string with 0x prefix)
    #[serde(rename = "merkleRoot")]
    pub merkle_root: String,
    /// Batch index (hex string with 0x prefix)
    pub index: String,
    /// L2 end block number for this batch (hex string with 0x prefix)
    #[serde(rename = "l2EndBlockNumber")]
    pub l2_end_block_number: String,
}

impl SequencerCommitment {
    /// Parse the batch index from hex to u64
    pub fn batch_index(&self) -> u64 {
        u64::from_str_radix(self.index.trim_start_matches("0x"), 16).unwrap_or(0)
    }

    /// Parse the L2 end block number from hex to u64
    pub fn l2_end_block(&self) -> u64 {
        u64::from_str_radix(
            self.l2_end_block_number.trim_start_matches("0x"),
            16,
        )
        .unwrap_or(0)
    }
}

/// A commitment with its associated L1 block height
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitmentWithL1Height {
    pub l1_height: u64,
    pub commitment: SequencerCommitment,
}

impl CommitmentWithL1Height {
    pub fn batch_index(&self) -> u64 {
        self.commitment.batch_index()
    }

    pub fn l2_end_block(&self) -> u64 {
        self.commitment.l2_end_block()
    }

    pub fn merkle_root(&self) -> &str {
        &self.commitment.merkle_root
    }
}

/// A proof entry from the batch explorer API.
/// Each proof maps to one or more sequencer commitment indexes.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchExplorerProof {
    /// Bitcoin txid of the proof transaction
    pub l1_tx_id: String,
    /// L1 block height where the proof was found
    pub found_in_l1_height: u64,
    /// First commitment index this proof covers
    pub first_commitment_index: u64,
    /// Last commitment index this proof covers (may equal first for single-batch proofs)
    pub last_commitment_index: u64,
    /// L2 block height at the end of the proven range
    pub last_l2_height: u64,
}

/// Generic JSON-RPC response
#[derive(Debug, Deserialize)]
struct RpcResponse<T> {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: u64,
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    #[allow(dead_code)]
    code: i64,
    message: String,
}

impl CitreaRpcClient {
    pub fn new() -> Self {
        Self::with_url(CITREA_RPC_URL)
    }

    pub fn with_url(url: &str) -> Self {
        Self {
            url: url.to_string(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Make a JSON-RPC call with retry logic
    fn rpc_call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<Option<T>, Box<dyn Error>> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1
        });

        let mut last_err = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(500 * attempt as u64));
            }

            match self.client.post(&self.url).json(&body).send() {
                Ok(response) => {
                    let text = response.text()?;
                    match serde_json::from_str::<RpcResponse<T>>(&text) {
                        Ok(resp) => {
                            if let Some(err) = resp.error {
                                return Err(format!("RPC error: {}", err.message).into());
                            }
                            return Ok(resp.result);
                        }
                        Err(e) => {
                            last_err = format!("Parse error (attempt {}): {} - response: {}", attempt + 1, e, &text[..text.len().min(200)]);
                            continue;
                        }
                    }
                }
                Err(e) => {
                    last_err = format!("Request error (attempt {}): {}", attempt + 1, e);
                    continue;
                }
            }
        }

        Err(format!("RPC call failed after 3 attempts: {}", last_err).into())
    }

    /// Get the current sync status of the Citrea node
    pub fn get_sync_status(&self) -> Result<SyncStatus, Box<dyn Error>> {
        self.rpc_call::<SyncStatus>("citrea_syncStatus", serde_json::json!([]))?
            .ok_or_else(|| "No result in sync status response".into())
    }

    /// Get the current synced L1 block height
    pub fn get_l1_height(&self) -> Result<u64, Box<dyn Error>> {
        let status = self.get_sync_status()?;
        let height = u64::from_str_radix(
            status.l1_status.synced.trim_start_matches("0x"),
            16,
        )?;
        Ok(height)
    }

    /// Get the current synced L2 block height
    pub fn get_l2_height(&self) -> Result<u64, Box<dyn Error>> {
        let status = self.get_sync_status()?;
        let height = u64::from_str_radix(
            status.l2_status.synced.trim_start_matches("0x"),
            16,
        )?;
        Ok(height)
    }

    /// Get sequencer commitments at a specific L1 block height.
    /// Returns None if no commitments exist at that height.
    pub fn get_commitments_at_height(
        &self,
        l1_height: u64,
    ) -> Result<Option<Vec<SequencerCommitment>>, Box<dyn Error>> {
        self.rpc_call::<Vec<SequencerCommitment>>(
            "ledger_getSequencerCommitmentsOnSlotByNumber",
            serde_json::json!([l1_height]),
        )
    }

    /// Scan a range of L1 block heights and collect all sequencer commitments.
    /// Calls progress_callback with (commitments_found, heights_scanned, total_heights).
    pub fn scan_commitments(
        &self,
        start_height: u64,
        end_height: u64,
        progress_callback: Option<&dyn Fn(usize, u64, u64)>,
    ) -> Result<Vec<CommitmentWithL1Height>, Box<dyn Error>> {
        let mut all_commitments = Vec::new();
        let total = end_height.saturating_sub(start_height) + 1;

        for height in start_height..=end_height {
            match self.get_commitments_at_height(height)? {
                Some(commitments) => {
                    for c in commitments {
                        all_commitments.push(CommitmentWithL1Height {
                            l1_height: height,
                            commitment: c,
                        });
                    }
                }
                None => {}
            }

            if let Some(cb) = progress_callback {
                cb(all_commitments.len(), height - start_height + 1, total);
            }
        }

        Ok(all_commitments)
    }

    /// Find the earliest L1 height that contains a sequencer commitment
    /// by binary searching from genesis_height to current_height.
    pub fn find_first_commitment_height(
        &self,
        search_start: u64,
        search_end: u64,
    ) -> Result<Option<u64>, Box<dyn Error>> {
        // Binary search: find the lowest height with a commitment
        let mut lo = search_start;
        let mut hi = search_end;
        let mut best = None;

        while lo <= hi {
            let mid = lo + (hi - lo) / 2;

            // Check a small window around mid for commitments
            let mut found = false;
            for h in mid..=(mid + 10).min(search_end) {
                if let Some(commits) = self.get_commitments_at_height(h)? {
                    if !commits.is_empty() {
                        // Check if any commitment has a low batch index
                        let min_idx = commits.iter().map(|c| c.batch_index()).min().unwrap_or(u64::MAX);
                        if min_idx <= 10 {
                            // We're near the start
                            best = Some(h);
                            found = true;
                            hi = mid.saturating_sub(1);
                            break;
                        } else {
                            // There are earlier commitments, search lower
                            found = true;
                            hi = mid.saturating_sub(1);
                            break;
                        }
                    }
                }
            }

            if !found {
                lo = mid + 11;
            }
        }

        // If binary search didn't converge well, do a linear scan from best found
        if let Some(h) = best {
            // Scan backwards from the found height to find the true first
            let scan_start = h.saturating_sub(50);
            for height in scan_start..h {
                if let Some(commits) = self.get_commitments_at_height(height)? {
                    if !commits.is_empty() {
                        return Ok(Some(height));
                    }
                }
            }
            return Ok(Some(h));
        }

        Ok(None)
    }

    /// Scan all commitments from the beginning by finding the first commitment
    /// and scanning forward to the current L1 height.
    pub fn scan_all_commitments(
        &self,
        progress_callback: Option<&dyn Fn(usize, u64, u64)>,
    ) -> Result<Vec<CommitmentWithL1Height>, Box<dyn Error>> {
        let current_l1 = self.get_l1_height()?;

        // Citrea mainnet launched recently; start searching from a reasonable point.
        // With ~2800 batches and ~4 L1 blocks per batch, go back ~12000 blocks.
        let search_start = current_l1.saturating_sub(15000);

        eprintln!(
            "Scanning L1 heights {} to {} for sequencer commitments...",
            search_start, current_l1
        );

        self.scan_commitments(search_start, current_l1, progress_callback)
    }

    /// Get the latest batch index by checking the most recent commitment
    pub fn get_latest_batch_index(&self) -> Result<Option<u64>, Box<dyn Error>> {
        let current_l1 = self.get_l1_height()?;

        // Scan backwards from current height to find the most recent commitment
        for height in (current_l1.saturating_sub(20)..=current_l1).rev() {
            if let Some(commits) = self.get_commitments_at_height(height)? {
                if let Some(last) = commits.last() {
                    return Ok(Some(last.batch_index()));
                }
            }
        }

        Ok(None)
    }

    /// Fetch all batch proofs from the batch explorer API.
    /// Returns proofs ordered newest-first with the mapping from proof txid to commitment indexes.
    /// Uses pagination to retrieve all available proofs.
    pub fn fetch_proofs_from_explorer(&self) -> Result<Vec<BatchExplorerProof>, Box<dyn Error>> {
        let url = format!("{}/proofs?limit=10000", BATCH_EXPLORER_API_URL);
        let response = self.client.get(&url).send()?;
        let text = response.text()?;
        // The API response includes large state_diff fields we don't need;
        // serde ignores unknown fields by default so our struct only captures what we need.
        let proofs: Vec<BatchExplorerProof> = serde_json::from_str(&text)?;
        Ok(proofs)
    }

    /// Get a summary of the current state
    pub fn get_summary(&self) -> Result<BatchSummary, Box<dyn Error>> {
        let status = self.get_sync_status()?;
        let l1_height = u64::from_str_radix(
            status.l1_status.synced.trim_start_matches("0x"),
            16,
        )?;
        let l2_height = u64::from_str_radix(
            status.l2_status.synced.trim_start_matches("0x"),
            16,
        )?;

        let latest_batch = self.get_latest_batch_index()?;

        // Get the most recent few commitments for display
        let mut recent = Vec::new();
        let mut found = 0;
        for height in (l1_height.saturating_sub(50)..=l1_height).rev() {
            if let Some(commits) = self.get_commitments_at_height(height)? {
                for c in commits.into_iter().rev() {
                    recent.push(CommitmentWithL1Height {
                        l1_height: height,
                        commitment: c,
                    });
                    found += 1;
                    if found >= 10 {
                        break;
                    }
                }
            }
            if found >= 10 {
                break;
            }
        }

        Ok(BatchSummary {
            l1_height,
            l2_height,
            latest_batch_index: latest_batch,
            recent_commitments: recent,
        })
    }

    /// Get the transaction count of a single L2 block by number.
    pub fn get_block_tx_count(&self, block_number: u64) -> Result<u64, Box<dyn Error>> {
        let hex_num = format!("0x{:x}", block_number);
        let result: Option<String> = self.rpc_call(
            "eth_getBlockTransactionCountByNumber",
            serde_json::json!([hex_num]),
        )?;
        match result {
            Some(s) => {
                let count = u64::from_str_radix(s.trim_start_matches("0x"), 16)?;
                Ok(count)
            }
            None => Ok(0),
        }
    }

    /// Batch JSON-RPC: get transaction counts for multiple L2 blocks in a single HTTP request.
    /// Sends up to `batch_size` requests per HTTP call. Returns (block_number, tx_count) pairs.
    pub fn get_block_tx_counts_batch(
        &self,
        block_numbers: &[u64],
        batch_size: usize,
    ) -> Result<Vec<(u64, u64)>, Box<dyn Error>> {
        let mut results = Vec::with_capacity(block_numbers.len());

        for chunk in block_numbers.chunks(batch_size) {
            let batch_body: Vec<serde_json::Value> = chunk
                .iter()
                .enumerate()
                .map(|(i, &bn)| {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "eth_getBlockTransactionCountByNumber",
                        "params": [format!("0x{:x}", bn)],
                        "id": i + 1
                    })
                })
                .collect();

            let mut last_err = String::new();
            let mut batch_results: Option<Vec<serde_json::Value>> = None;

            for attempt in 0..3u64 {
                if attempt > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(500 * attempt));
                }

                match self
                    .client
                    .post(&self.url)
                    .json(&batch_body)
                    .send()
                {
                    Ok(response) => {
                        let text = response.text()?;
                        match serde_json::from_str::<Vec<serde_json::Value>>(&text) {
                            Ok(parsed) => {
                                batch_results = Some(parsed);
                                break;
                            }
                            Err(e) => {
                                last_err = format!(
                                    "Batch parse error (attempt {}): {}",
                                    attempt + 1,
                                    e
                                );
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        last_err =
                            format!("Batch request error (attempt {}): {}", attempt + 1, e);
                        continue;
                    }
                }
            }

            let parsed = batch_results.ok_or_else(|| {
                format!("Batch RPC failed after 3 attempts: {}", last_err)
            })?;

            // Match responses back to block numbers using the id field
            let mut counts: Vec<(usize, u64)> = Vec::new();
            for entry in &parsed {
                let id = entry
                    .get("id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let count = entry
                    .get("result")
                    .and_then(|v| v.as_str())
                    .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                    .unwrap_or(0);
                counts.push((id, count));
            }
            counts.sort_by_key(|&(id, _)| id);

            for (i, &block_num) in chunk.iter().enumerate() {
                let count = counts
                    .iter()
                    .find(|&&(id, _)| id == i + 1)
                    .map(|&(_, c)| c)
                    .unwrap_or(0);
                results.push((block_num, count));
            }
        }

        Ok(results)
    }

    /// Batch JSON-RPC: get gas data for multiple L2 blocks in a single HTTP request.
    /// Uses `eth_getBlockByNumber` with `false` (no full txs).
    /// Returns (block_number, gas_used, gas_limit) triples.
    pub fn get_block_gas_batch(
        &self,
        block_numbers: &[u64],
        batch_size: usize,
    ) -> Result<Vec<(u64, u64, u64)>, Box<dyn Error>> {
        let mut results = Vec::with_capacity(block_numbers.len());

        for chunk in block_numbers.chunks(batch_size) {
            let batch_body: Vec<serde_json::Value> = chunk
                .iter()
                .enumerate()
                .map(|(i, &bn)| {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "eth_getBlockByNumber",
                        "params": [format!("0x{:x}", bn), false],
                        "id": i + 1
                    })
                })
                .collect();

            let mut last_err = String::new();
            let mut batch_results: Option<Vec<serde_json::Value>> = None;

            for attempt in 0..3u64 {
                if attempt > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(500 * attempt));
                }

                match self
                    .client
                    .post(&self.url)
                    .json(&batch_body)
                    .send()
                {
                    Ok(response) => {
                        let text = response.text()?;
                        match serde_json::from_str::<Vec<serde_json::Value>>(&text) {
                            Ok(parsed) => {
                                batch_results = Some(parsed);
                                break;
                            }
                            Err(e) => {
                                last_err = format!(
                                    "Batch parse error (attempt {}): {}",
                                    attempt + 1,
                                    e
                                );
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        last_err =
                            format!("Batch request error (attempt {}): {}", attempt + 1, e);
                        continue;
                    }
                }
            }

            let parsed = batch_results.ok_or_else(|| {
                format!("Batch RPC failed after 3 attempts: {}", last_err)
            })?;

            // Match responses back to block numbers using the id field
            let mut entries: Vec<(usize, u64, u64)> = Vec::new();
            for entry in &parsed {
                let id = entry
                    .get("id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let result = entry.get("result");
                let gas_used = result
                    .and_then(|r| r.get("gasUsed"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                    .unwrap_or(0);
                let gas_limit = result
                    .and_then(|r| r.get("gasLimit"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                    .unwrap_or(0);
                entries.push((id, gas_used, gas_limit));
            }
            entries.sort_by_key(|&(id, _, _)| id);

            for (i, &block_num) in chunk.iter().enumerate() {
                let (gas_used, gas_limit) = entries
                    .iter()
                    .find(|&&(id, _, _)| id == i + 1)
                    .map(|&(_, gu, gl)| (gu, gl))
                    .unwrap_or((0, 0));
                results.push((block_num, gas_used, gas_limit));
            }
        }

        Ok(results)
    }
}

impl Default for CitreaRpcClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of the current batch state
#[derive(Debug, Clone, Serialize)]
pub struct BatchSummary {
    pub l1_height: u64,
    pub l2_height: u64,
    pub latest_batch_index: Option<u64>,
    pub recent_commitments: Vec<CommitmentWithL1Height>,
}

impl std::fmt::Display for BatchSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Citrea Batch Status")?;
        writeln!(f, "  L1 (Bitcoin) Height: {}", self.l1_height)?;
        writeln!(f, "  L2 (Citrea) Height:  {}", self.l2_height)?;
        if let Some(idx) = self.latest_batch_index {
            writeln!(f, "  Latest Batch Index:  {}", idx)?;
        }
        writeln!(f)?;

        if !self.recent_commitments.is_empty() {
            writeln!(
                f,
                "  {:>8} {:>12} {:>12} {}",
                "Batch", "L2 End Blk", "L1 Height", "Merkle Root"
            )?;
            writeln!(f, "  {}", "-".repeat(70))?;
            for c in &self.recent_commitments {
                writeln!(
                    f,
                    "  {:>8} {:>12} {:>12} {}",
                    c.batch_index(),
                    c.l2_end_block(),
                    c.l1_height,
                    &c.merkle_root()[..18],
                )?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires network access
    fn test_sync_status() {
        let client = CitreaRpcClient::new();
        let status = client.get_sync_status().expect("Failed to get sync status");
        println!("L1 synced: {}", status.l1_status.synced);
        println!("L2 synced: {}", status.l2_status.synced);
    }

    #[test]
    #[ignore] // Requires network access
    fn test_get_commitments() {
        let client = CitreaRpcClient::new();
        // Known height with commitments from our earlier scan
        let commits = client
            .get_commitments_at_height(934246)
            .expect("Failed to get commitments");
        assert!(commits.is_some());
        let commits = commits.unwrap();
        assert!(!commits.is_empty());
        println!("Found {} commitments at height 934246", commits.len());
        for c in &commits {
            println!(
                "  Batch #{}, L2 end block {}, root {}",
                c.batch_index(),
                c.l2_end_block(),
                &c.merkle_root[..18]
            );
        }
    }

    #[test]
    #[ignore] // Requires network access
    fn test_summary() {
        let client = CitreaRpcClient::new();
        let summary = client.get_summary().expect("Failed to get summary");
        println!("{}", summary);
    }
}
