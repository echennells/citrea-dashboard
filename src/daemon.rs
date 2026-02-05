use crate::bitcoin_rpc::{BitcoinCoreClient, CITREA_PROVER_ADDRESS, CITREA_SEQUENCER_ADDRESS};
use crate::citrea_rpc::CitreaRpcClient;
use crate::db::Database;
use crate::l2_counter;
use std::error::Error;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Configuration for the daemon
pub struct DaemonConfig {
    pub db_path: String,
    pub poll_interval_secs: u64,
    pub backfill_only: bool,
    pub json_path: Option<String>,
    pub batches_per_cycle: usize,
    pub btc_rpc_host: String,
    pub btc_rpc_user: String,
    pub btc_rpc_pass: String,
}


/// Run the daemon with the given configuration.
pub fn run(config: DaemonConfig) -> Result<(), Box<dyn Error>> {
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let shutdown = shutdown.clone();
        ctrlc::set_handler(move || {
            log::info!("Shutdown signal received, finishing current work...");
            shutdown.store(true, Ordering::SeqCst);
        })?;
    }

    let db = Database::open(Path::new(&config.db_path))?;
    let rpc = CitreaRpcClient::new();
    let btc_rpc = BitcoinCoreClient::new(
        &config.btc_rpc_host,
        &config.btc_rpc_user,
        &config.btc_rpc_pass,
    )?;

    // Phase 1: Bootstrap from JSON if DB is empty
    if db.commitment_count()? == 0 {
        if let Some(ref json_path) = config.json_path {
            let p = Path::new(json_path);
            if p.exists() {
                bootstrap_from_json(&db, p)?;
            } else {
                log::info!("No JSON file at {}, starting from scratch", json_path);
            }
        }
    }

    log::info!(
        "DB has {} commitments, {} enriched, {} tx-counted, {} gas-counted, {} proof-enriched, {} commit-enriched, {} proof-commit-enriched",
        db.commitment_count()?,
        db.enriched_count()?,
        db.tx_counted_count()?,
        db.gas_counted_count()?,
        db.proof_enriched_count()?,
        db.commit_enriched_count()?,
        db.proof_commit_enriched_count()?
    );

    if config.backfill_only {
        // Aggressive backfill mode: run all pipelines until complete, then exit
        log::info!("Running in backfill-only mode");
        run_backfill(&db, &rpc, &btc_rpc, &shutdown, &config)?;
    } else {
        // Steady-state daemon loop
        loop {
            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            // Pipeline 1+2: Discover new commitments + enrich fees via Bitcoin Core
            if let Err(e) = pipeline_discover_and_enrich(&db, &btc_rpc) {
                log::error!("Bitcoin Core discovery/enrichment error: {}", e);
            }
            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            // Record Citrea RPC L1 height for lag detection
            match rpc.get_l1_height() {
                Ok(citrea_l1) => {
                    if let Err(e) = db.set_sync_value("citrea_rpc_l1_height", &citrea_l1.to_string()) {
                        log::error!("Failed to store citrea_rpc_l1_height: {}", e);
                    }
                }
                Err(e) => log::warn!("Failed to query Citrea RPC L1 height: {}", e),
            }
            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            // Residual fee enrichment for JSON-bootstrapped data
            if let Err(e) = pipeline_fee_enrichment(&db, &btc_rpc) {
                log::error!("Residual fee enrichment error: {}", e);
            }
            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            // Proof enrichment via batch explorer API + Bitcoin Core verification
            if let Err(e) = pipeline_proof_enrichment(&db, &rpc, &btc_rpc) {
                log::error!("Proof enrichment error: {}", e);
            }
            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            // Commit fee enrichment (parent tx of reveal for both batches and proofs)
            if let Err(e) = pipeline_commit_fee_enrichment(&db, &btc_rpc) {
                log::error!("Commit fee enrichment error: {}", e);
            }
            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            // Pipeline 3: L2 tx counting (limited per cycle)
            if let Err(e) = pipeline_l2_tx_count(&db, &rpc, config.batches_per_cycle) {
                log::error!("L2 tx counting error: {}", e);
            }
            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            // Pipeline 4: L2 gas counting (limited per cycle)
            if let Err(e) = pipeline_l2_gas_count(&db, &rpc, config.batches_per_cycle) {
                log::error!("L2 gas counting error: {}", e);
            }
            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            // Stats snapshot
            if let Err(e) = db.record_stats_snapshot() {
                log::error!("Stats snapshot error: {}", e);
            }

            log::info!(
                "Cycle complete. {} commitments, {} enriched, {} tx-counted, {} gas-counted, {} proof-enriched, {} commit-enriched, {} proof-commit-enriched. Sleeping {}s...",
                db.commitment_count()?,
                db.enriched_count()?,
                db.tx_counted_count()?,
                db.gas_counted_count()?,
                db.proof_enriched_count()?,
                db.commit_enriched_count()?,
                db.proof_commit_enriched_count()?,
                config.poll_interval_secs
            );

            // Sleep with shutdown checks
            for _ in 0..config.poll_interval_secs {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }

    log::info!("Daemon stopped gracefully.");
    Ok(())
}

/// Backfill mode: run each pipeline aggressively until all work is done.
fn run_backfill(
    db: &Database,
    rpc: &CitreaRpcClient,
    btc_rpc: &BitcoinCoreClient,
    shutdown: &Arc<AtomicBool>,
    config: &DaemonConfig,
) -> Result<(), Box<dyn Error>> {
    // Step 1: Discover all commitments + enrich fees via Bitcoin Core
    log::info!("Backfill step 1: discovering commitments and enriching fees from Bitcoin Core...");
    pipeline_discover_and_enrich(db, btc_rpc)?;
    if shutdown.load(Ordering::SeqCst) {
        return Ok(());
    }

    // Record Citrea RPC L1 height for lag detection
    match rpc.get_l1_height() {
        Ok(citrea_l1) => {
            db.set_sync_value("citrea_rpc_l1_height", &citrea_l1.to_string())?;
        }
        Err(e) => log::warn!("Failed to query Citrea RPC L1 height: {}", e),
    }

    // Step 2: Residual fee enrichment for JSON-bootstrapped data
    log::info!("Backfill step 2: enriching any remaining unenriched commitments...");
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return Ok(());
        }
        match pipeline_fee_enrichment(db, btc_rpc) {
            Ok(()) => break,
            Err(e) => {
                let enriched = db.enriched_count()?;
                let total = db.commitment_count()?;
                log::warn!(
                    "Fee enrichment hit error ({}), retrying in 30s... ({}/{} enriched)",
                    e, enriched, total
                );
                for _ in 0..30 {
                    if shutdown.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
    }
    if shutdown.load(Ordering::SeqCst) {
        return Ok(());
    }

    // Step 2b: Proof enrichment via batch explorer API + Bitcoin Core
    log::info!("Backfill step 2b: enriching proof data from batch explorer API...");
    if let Err(e) = pipeline_proof_enrichment(db, rpc, btc_rpc) {
        log::warn!("Proof enrichment error (non-fatal): {}", e);
    }
    if shutdown.load(Ordering::SeqCst) {
        return Ok(());
    }

    // Step 2c: Commit fee enrichment (parent tx of reveal for both batches and proofs)
    log::info!("Backfill step 2c: enriching commit tx fees...");
    if let Err(e) = pipeline_commit_fee_enrichment(db, btc_rpc) {
        log::warn!("Commit fee enrichment error (non-fatal): {}", e);
    }
    if shutdown.load(Ordering::SeqCst) {
        return Ok(());
    }

    // Step 3: Count all L2 txs (large batches)
    log::info!("Backfill step 3: counting L2 transactions...");
    let backfill_batch_size = if config.backfill_only { 50 } else { config.batches_per_cycle };
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let uncounted = db.uncounted_batch_indexes(backfill_batch_size as u32)?;
        if uncounted.is_empty() {
            break;
        }
        for (batch_idx, start, end) in &uncounted {
            if shutdown.load(Ordering::SeqCst) {
                return Ok(());
            }
            match l2_counter::count_l2_txs_in_range(rpc, *start, *end) {
                Ok(count) => {
                    db.set_l2_tx_count(*batch_idx, count)?;
                    log::info!(
                        "Batch {} (blocks {}-{}): {} L2 txs",
                        batch_idx,
                        start,
                        end,
                        count
                    );
                }
                Err(e) => {
                    log::error!("Failed to count L2 txs for batch {}: {}", batch_idx, e);
                }
            }
        }
        log::info!(
            "L2 tx counting progress: {}/{} batches done",
            db.tx_counted_count()?,
            db.commitment_count()?
        );
    }

    // Step 4: Count all L2 gas usage
    log::info!("Backfill step 4: counting L2 gas usage...");
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let uncounted = db.uncounted_gas_batch_indexes(backfill_batch_size as u32)?;
        if uncounted.is_empty() {
            break;
        }
        for (batch_idx, start, end) in &uncounted {
            if shutdown.load(Ordering::SeqCst) {
                return Ok(());
            }
            match l2_counter::aggregate_gas_in_range(rpc, *start, *end) {
                Ok(agg) => {
                    db.set_l2_gas_data(*batch_idx, agg.total_used, agg.total_limit, agg.max_used, agg.min_used)?;
                    let util = if agg.total_limit > 0 {
                        100.0 * agg.total_used as f64 / agg.total_limit as f64
                    } else {
                        0.0
                    };
                    log::info!(
                        "Batch {} (blocks {}-{}): gas {}/{} ({:.1}% util), min={} max={}",
                        batch_idx, start, end,
                        agg.total_used, agg.total_limit, util,
                        agg.min_used, agg.max_used
                    );
                }
                Err(e) => {
                    log::error!("Failed to count gas for batch {}: {}", batch_idx, e);
                }
            }
        }
        log::info!(
            "L2 gas counting progress: {}/{} batches done",
            db.gas_counted_count()?,
            db.commitment_count()?
        );
    }

    db.record_stats_snapshot()?;
    log::info!(
        "Backfill complete. {} commitments, {} enriched, {} tx-counted, {} gas-counted, {} proof-enriched, {} commit-enriched, {} proof-commit-enriched",
        db.commitment_count()?,
        db.enriched_count()?,
        db.tx_counted_count()?,
        db.gas_counted_count()?,
        db.proof_enriched_count()?,
        db.commit_enriched_count()?,
        db.proof_commit_enriched_count()?
    );
    Ok(())
}

// ---- Pipeline 1+2: Discover Commitments + Enrich Fees via Bitcoin Core ----

/// Discovery and fee enrichment using Bitcoin Core as the primary source.
/// Scans new Bitcoin blocks for sequencer commitment transactions, inserts commitments,
/// and enriches fee data in a single pass.
fn pipeline_discover_and_enrich(
    db: &Database,
    btc_rpc: &BitcoinCoreClient,
) -> Result<(), Box<dyn Error>> {
    let btc_height = btc_rpc.get_block_height()?;
    let last_scanned = db
        .get_sync_value("last_scanned_l1_height")?
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(btc_height.saturating_sub(15000));

    if last_scanned >= btc_height {
        log::debug!("No new Bitcoin blocks to scan (at {})", btc_height);
        return Ok(());
    }

    let start = last_scanned + 1;
    log::info!(
        "Scanning Bitcoin blocks {} to {} for commitments...",
        start,
        btc_height
    );

    let mut inserted = 0u64;
    let mut enriched = 0u64;
    let total_blocks = btc_height - start + 1;

    for height in start..=btc_height {
        let found_txs = match btc_rpc.find_sequencer_tx_in_block(height, CITREA_SEQUENCER_ADDRESS)
        {
            Ok(txs) => txs,
            Err(e) => {
                log::error!("Failed to scan block {}: {}", height, e);
                continue;
            }
        };

        for tx in &found_txs {
            let batch_idx = tx.batch_number as u64;
            let l2_end = tx.l2_end_block;

            // Derive l2_start_block from previous batch
            let l2_start = if batch_idx == 1 {
                1
            } else {
                db.get_l2_end_block(batch_idx - 1)?
                    .map(|prev_end| prev_end + 1)
                    .unwrap_or_else(|| l2_end.saturating_sub(999))
            };

            if db.insert_commitment(batch_idx, &tx.merkle_root, l2_start, l2_end, height)? {
                inserted += 1;
            }

            db.enrich_commitment_fee(
                batch_idx,
                &tx.txid,
                tx.fee_sats,
                tx.weight,
                Some(tx.block_time),
            )?;
            enriched += 1;

            log::debug!(
                "Batch {} at block {}: txid={}, fee={} sats, l2_blocks={}-{}",
                batch_idx,
                height,
                &tx.txid[..16],
                tx.fee_sats,
                l2_start,
                l2_end
            );
        }

        let scanned = height - start + 1;
        if scanned % 100 == 0 {
            log::info!(
                "Bitcoin scan progress: {}/{} blocks, {} new batches, {} enriched",
                scanned,
                total_blocks,
                inserted,
                enriched
            );
        }
    }

    db.set_sync_value("last_scanned_l1_height", &btc_height.to_string())?;
    db.set_sync_value("btc_core_height", &btc_height.to_string())?;

    if inserted > 0 || enriched > 0 {
        log::info!(
            "Bitcoin scan complete: {} new commitments, {} enriched (now {} total, {} enriched)",
            inserted,
            enriched,
            db.commitment_count()?,
            db.enriched_count()?
        );
    } else {
        log::debug!("No new commitments found in {} blocks", total_blocks);
    }

    Ok(())
}

// ---- Bootstrap from JSON ----

fn bootstrap_from_json(db: &Database, path: &Path) -> Result<(), Box<dyn Error>> {
    log::info!("Bootstrapping from {:?}...", path);
    let data = std::fs::read_to_string(path)?;
    let commitments: Vec<crate::citrea_rpc::CommitmentWithL1Height> =
        serde_json::from_str(&data)?;

    let mut inserted = 0u64;
    let total = commitments.len();

    // Sort by batch_index so we can derive l2_start_block
    let mut sorted = commitments;
    sorted.sort_by_key(|c| c.batch_index());

    for (i, c) in sorted.iter().enumerate() {
        let batch_idx = c.batch_index();
        let l2_end = c.l2_end_block();
        let l2_start = if batch_idx == 1 {
            1
        } else if i > 0 {
            sorted[i - 1].l2_end_block() + 1
        } else {
            // Fallback: assume 1000 blocks per batch
            l2_end.saturating_sub(999)
        };

        if db.insert_commitment(batch_idx, c.merkle_root(), l2_start, l2_end, c.l1_height)? {
            inserted += 1;
        }
    }

    // Set sync cursor to the max L1 height we've seen
    if let Some(max_c) = sorted.last() {
        db.set_sync_value("last_scanned_l1_height", &max_c.l1_height.to_string())?;
    }

    log::info!(
        "Bootstrapped {}/{} commitments from JSON",
        inserted,
        total
    );
    Ok(())
}

// ---- Residual Fee Enrichment (for JSON-bootstrapped data) ----

fn pipeline_fee_enrichment(
    db: &Database,
    btc_rpc: &BitcoinCoreClient,
) -> Result<(), Box<dyn Error>> {
    // Fee enrichment for JSON-bootstrapped data
    let unenriched = db.unenriched_batch_indexes_with_height(10000)?;
    if unenriched.is_empty() {
        log::debug!("All commitments already fee-enriched");
        return Ok(());
    }

    log::info!(
        "Fee enrichment: {} unenriched commitments to process",
        unenriched.len()
    );

    // Group by l1_height to avoid fetching the same block twice
    let mut by_height: std::collections::HashMap<u64, Vec<u64>> = std::collections::HashMap::new();
    for (batch_index, l1_height) in &unenriched {
        by_height
            .entry(*l1_height)
            .or_default()
            .push(*batch_index);
    }

    let mut matched = 0u64;
    let mut blocks_scanned = 0u64;

    for (l1_height, batch_indexes) in &by_height {
        let found_txs =
            match btc_rpc.find_sequencer_tx_in_block(*l1_height, CITREA_SEQUENCER_ADDRESS) {
                Ok(txs) => txs,
                Err(e) => {
                    log::error!(
                        "Failed to scan block {} for sequencer txs: {}",
                        l1_height,
                        e
                    );
                    continue;
                }
            };
        blocks_scanned += 1;

        for found_tx in &found_txs {
            let batch_idx = found_tx.batch_number as u64;
            if batch_indexes.contains(&batch_idx) {
                db.enrich_commitment_fee(
                    batch_idx,
                    &found_tx.txid,
                    found_tx.fee_sats,
                    found_tx.weight,
                    Some(found_tx.block_time),
                )?;
                matched += 1;
            }
        }

        if blocks_scanned % 50 == 0 {
            log::info!(
                "Fee enrichment progress: scanned {}/{} blocks, matched {} batches",
                blocks_scanned,
                by_height.len(),
                matched
            );
        }
    }

    if matched > 0 {
        log::info!(
            "Enriched {} commitments with fee data ({}/{} total)",
            matched,
            db.enriched_count()?,
            db.commitment_count()?
        );
    }

    Ok(())
}

// ---- Pipeline: Proof Enrichment via Batch Explorer API + Bitcoin Core ----

/// Fetch proof-to-batch mapping from the batch explorer API, then verify each
/// proof transaction on Bitcoin Core to get the actual fee. The API provides
/// the mapping (which txid proves which batch), and Bitcoin provides the fee data.
fn pipeline_proof_enrichment(
    db: &Database,
    rpc: &CitreaRpcClient,
    btc_rpc: &BitcoinCoreClient,
) -> Result<(), Box<dyn Error>> {
    let unenriched = db.proof_unenriched_batch_set()?;
    if unenriched.is_empty() {
        log::debug!("All batches already proof-enriched");
        return Ok(());
    }

    // Fetch proof-to-batch mapping from batch explorer API
    log::info!(
        "Fetching proof mapping from batch explorer API ({} batches need proofs)...",
        unenriched.len()
    );
    let proofs = rpc.fetch_proofs_from_explorer()?;
    log::info!(
        "Got {} proofs from batch explorer API",
        proofs.len()
    );

    // Filter to proofs that cover at least one unenriched batch
    let relevant: Vec<_> = proofs
        .iter()
        .filter(|p| {
            (p.first_commitment_index..=p.last_commitment_index)
                .any(|idx| unenriched.contains(&idx))
        })
        .collect();

    if relevant.is_empty() {
        log::info!("No proofs available yet for unenriched batches");
        return Ok(());
    }

    log::info!(
        "Verifying {} proof transactions on Bitcoin Core...",
        relevant.len()
    );

    let mut enriched = 0u64;
    let mut verified = 0u64;
    let mut warnings = 0u64;

    for (i, proof) in relevant.iter().enumerate() {
        // Get fee and verify on-chain via getrawtransaction
        let tx_info = match btc_rpc.get_proof_tx_info(&proof.l1_tx_id, CITREA_PROVER_ADDRESS) {
            Ok(info) => info,
            Err(e) => {
                log::warn!(
                    "Failed to fetch proof tx {}: {}",
                    &proof.l1_tx_id[..16],
                    e
                );
                continue;
            }
        };
        verified += 1;

        if !tx_info.sends_to_prover {
            log::warn!(
                "Proof tx {} does NOT send to prover address - skipping",
                &proof.l1_tx_id[..16]
            );
            warnings += 1;
            continue;
        }

        // Split fee across the batches this proof covers, giving remainder to first batch
        let batches_covered =
            proof.last_commitment_index - proof.first_commitment_index + 1;
        let base_fee = tx_info.fee_sats / batches_covered;
        let remainder = tx_info.fee_sats % batches_covered;

        for (i, batch_idx) in (proof.first_commitment_index..=proof.last_commitment_index).enumerate() {
            if unenriched.contains(&batch_idx) {
                let fee = if i == 0 { base_fee + remainder } else { base_fee };
                db.enrich_proof_data(
                    batch_idx,
                    &tx_info.txid,
                    fee,
                    tx_info.weight,
                    Some(tx_info.block_time),
                    Some(proof.found_in_l1_height),
                )?;
                enriched += 1;
            }
        }

        if (i + 1) % 100 == 0 {
            log::info!(
                "Proof enrichment progress: {}/{} proofs verified, {} batches enriched",
                i + 1,
                relevant.len(),
                enriched
            );
        }
    }

    log::info!(
        "Proof enrichment complete: {} proofs verified, {} batches enriched, {} warnings ({}/{} total proof-enriched)",
        verified,
        enriched,
        warnings,
        db.proof_enriched_count()?,
        db.commitment_count()?
    );

    Ok(())
}

// ---- Pipeline: Commit Fee Enrichment (parent of reveal tx) ----

/// Fetch commit (parent) tx fees for both batch commitments and proof reveals.
/// The commit tx is the parent of the reveal tx, found via vin[0].txid.
fn pipeline_commit_fee_enrichment(
    db: &Database,
    btc_rpc: &BitcoinCoreClient,
) -> Result<(), Box<dyn Error>> {
    // Part 1: Batch commit fees
    let unenriched = db.commit_unenriched_batch_indexes(10000)?;
    if !unenriched.is_empty() {
        log::info!(
            "Commit fee enrichment: {} batches need commit tx fees",
            unenriched.len()
        );

        let mut enriched = 0u64;
        let mut cache: std::collections::HashMap<String, crate::bitcoin_rpc::CommitTxInfo> =
            std::collections::HashMap::new();

        for (batch_idx, btc_txid) in &unenriched {
            let commit_info = if let Some(cached) = cache.get(btc_txid) {
                cached.clone()
            } else {
                match btc_rpc.get_commit_tx_info(btc_txid) {
                    Ok(info) => {
                        cache.insert(btc_txid.clone(), info.clone());
                        info
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to fetch commit tx for batch {} (reveal {}): {}",
                            batch_idx,
                            &btc_txid[..16.min(btc_txid.len())],
                            e
                        );
                        continue;
                    }
                }
            };

            db.enrich_commit_fee(
                *batch_idx,
                &commit_info.commit_txid,
                commit_info.fee_sats,
                commit_info.weight,
                commit_info.block_time,
            )?;
            enriched += 1;

            if enriched % 100 == 0 {
                log::info!(
                    "Commit fee enrichment progress: {}/{} batches done",
                    enriched,
                    unenriched.len()
                );
            }
        }

        if enriched > 0 {
            log::info!(
                "Batch commit fee enrichment complete: {} batches enriched ({}/{} total)",
                enriched,
                db.commit_enriched_count()?,
                db.commitment_count()?
            );
        }
    }

    // Part 2: Proof commit fees
    let unenriched_proof = db.proof_commit_unenriched_batch_indexes(10000)?;
    if !unenriched_proof.is_empty() {
        log::info!(
            "Proof commit fee enrichment: {} batches need proof commit tx fees",
            unenriched_proof.len()
        );

        let mut enriched = 0u64;
        let mut cache: std::collections::HashMap<String, crate::bitcoin_rpc::CommitTxInfo> =
            std::collections::HashMap::new();

        for (batch_idx, proof_txid) in &unenriched_proof {
            let commit_info = if let Some(cached) = cache.get(proof_txid) {
                cached.clone()
            } else {
                match btc_rpc.get_commit_tx_info(proof_txid) {
                    Ok(info) => {
                        cache.insert(proof_txid.clone(), info.clone());
                        info
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to fetch proof commit tx for batch {} (proof {}): {}",
                            batch_idx,
                            &proof_txid[..16.min(proof_txid.len())],
                            e
                        );
                        continue;
                    }
                }
            };

            // Split proof commit fee across all batches sharing this proof_txid,
            // giving remainder to the first batch (lowest batch_index)
            let share_count = db.count_batches_with_proof_txid(proof_txid)?;
            let (base_fee, remainder) = if share_count > 0 {
                (commit_info.fee_sats / share_count, commit_info.fee_sats % share_count)
            } else {
                (commit_info.fee_sats, 0)
            };
            let first_batch = db.first_batch_with_proof_txid(proof_txid)?;
            let fee_for_this_batch = if Some(*batch_idx) == first_batch {
                base_fee + remainder
            } else {
                base_fee
            };

            db.enrich_proof_commit_fee(
                *batch_idx,
                &commit_info.commit_txid,
                fee_for_this_batch,
                commit_info.weight,
                commit_info.block_time,
            )?;
            enriched += 1;

            if enriched % 100 == 0 {
                log::info!(
                    "Proof commit fee enrichment progress: {}/{} batches done",
                    enriched,
                    unenriched_proof.len()
                );
            }
        }

        if enriched > 0 {
            log::info!(
                "Proof commit fee enrichment complete: {} batches enriched ({}/{} total)",
                enriched,
                db.proof_commit_enriched_count()?,
                db.commitment_count()?
            );
        }
    }

    if unenriched.is_empty() && unenriched_proof.is_empty() {
        log::debug!("All batches already commit-fee-enriched");
    }

    Ok(())
}

// ---- Pipeline 3: L2 Transaction Counting ----

fn pipeline_l2_tx_count(
    db: &Database,
    rpc: &CitreaRpcClient,
    batches_per_cycle: usize,
) -> Result<(), Box<dyn Error>> {
    let uncounted = db.uncounted_batch_indexes(batches_per_cycle as u32)?;
    if uncounted.is_empty() {
        log::debug!("All batches already tx-counted");
        return Ok(());
    }

    for (batch_idx, start, end) in &uncounted {
        match l2_counter::count_l2_txs_in_range(rpc, *start, *end) {
            Ok(count) => {
                db.set_l2_tx_count(*batch_idx, count)?;
                log::info!(
                    "Batch {} (blocks {}-{}): {} L2 txs",
                    batch_idx,
                    start,
                    end,
                    count
                );
            }
            Err(e) => {
                log::error!("Failed to count L2 txs for batch {}: {}", batch_idx, e);
            }
        }
    }

    Ok(())
}

// ---- Pipeline 4: L2 Gas Counting ----

fn pipeline_l2_gas_count(
    db: &Database,
    rpc: &CitreaRpcClient,
    batches_per_cycle: usize,
) -> Result<(), Box<dyn Error>> {
    let uncounted = db.uncounted_gas_batch_indexes(batches_per_cycle as u32)?;
    if uncounted.is_empty() {
        log::debug!("All batches already gas-counted");
        return Ok(());
    }

    for (batch_idx, start, end) in &uncounted {
        match l2_counter::aggregate_gas_in_range(rpc, *start, *end) {
            Ok(agg) => {
                db.set_l2_gas_data(*batch_idx, agg.total_used, agg.total_limit, agg.max_used, agg.min_used)?;
                let util = if agg.total_limit > 0 {
                    100.0 * agg.total_used as f64 / agg.total_limit as f64
                } else {
                    0.0
                };
                log::info!(
                    "Batch {} (blocks {}-{}): gas {}/{} ({:.1}% util)",
                    batch_idx, start, end,
                    agg.total_used, agg.total_limit, util
                );
            }
            Err(e) => {
                log::error!("Failed to count gas for batch {}: {}", batch_idx, e);
            }
        }
    }

    Ok(())
}
