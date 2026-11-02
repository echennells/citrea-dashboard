use crate::bitcoin_rpc::{BitcoinCoreClient, FoundSequencerTx, CITREA_PROVER_ADDRESS, CITREA_SEQUENCER_ADDRESS};
use crate::citrea_rpc::CitreaRpcClient;
use crate::db::Database;
use crate::error::Result;
use crate::events;
use crate::l2_counter;
use rayon::prelude::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashMap;
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
    pub citrea_rpc_url: String,
    pub batch_explorer_url: String,
}


/// Check if shutdown has been requested; break from the enclosing loop if so.
macro_rules! check_shutdown {
    ($flag:expr) => {
        if $flag.load(Ordering::SeqCst) {
            break;
        }
    };
    ($flag:expr, return) => {
        if $flag.load(Ordering::SeqCst) {
            return Ok(());
        }
    };
}

/// Run the daemon with the given configuration.
pub fn run(config: DaemonConfig) -> Result<()> {
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let shutdown = shutdown.clone();
        ctrlc::set_handler(move || {
            log::info!("Shutdown signal received, finishing current work...");
            shutdown.store(true, Ordering::SeqCst);
        })?;
    }

    let db = Database::open(Path::new(&config.db_path))?;
    let rpc = CitreaRpcClient::with_urls(&config.citrea_rpc_url, &config.batch_explorer_url);
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
            check_shutdown!(shutdown);

            // Pipeline 1+2: Discover new commitments + enrich fees via Bitcoin Core
            if let Err(e) = pipeline_discover_and_enrich(&db, &btc_rpc) {
                log::error!("Bitcoin Core discovery/enrichment error: {}", e);
            }
            check_shutdown!(shutdown);

            // Record Citrea RPC L1 height for lag detection
            match rpc.get_l1_height() {
                Ok(citrea_l1) => {
                    if let Err(e) = db.set_sync_value("citrea_rpc_l1_height", &citrea_l1.to_string()) {
                        log::error!("Failed to store citrea_rpc_l1_height: {}", e);
                    }
                }
                Err(e) => log::warn!("Failed to query Citrea RPC L1 height: {}", e),
            }
            check_shutdown!(shutdown);

            // Residual fee enrichment for JSON-bootstrapped data
            if let Err(e) = pipeline_fee_enrichment(&db, &btc_rpc) {
                log::error!("Residual fee enrichment error: {}", e);
            }
            check_shutdown!(shutdown);

            // Proof enrichment via batch explorer API + Bitcoin Core verification
            if let Err(e) = pipeline_proof_enrichment(&db, &rpc, &btc_rpc) {
                log::error!("Proof enrichment error: {}", e);
            }
            check_shutdown!(shutdown);

            // Commit fee enrichment (parent tx of reveal for both batches and proofs)
            if let Err(e) = pipeline_commit_fee_enrichment(&db, &btc_rpc) {
                log::error!("Commit fee enrichment error: {}", e);
            }
            check_shutdown!(shutdown);

            // Pipeline 3: L2 tx counting (limited per cycle)
            if let Err(e) = pipeline_l2_tx_count(&db, &rpc, config.batches_per_cycle) {
                log::error!("L2 tx counting error: {}", e);
            }
            check_shutdown!(shutdown);

            // Pipeline 4: L2 gas counting (limited per cycle)
            if let Err(e) = pipeline_l2_gas_count(&db, &rpc, config.batches_per_cycle) {
                log::error!("L2 gas counting error: {}", e);
            }
            check_shutdown!(shutdown);

            // Stats snapshot
            if let Err(e) = db.record_stats_snapshot() {
                log::error!("Stats snapshot error: {}", e);
            }
            check_shutdown!(shutdown);

            // Event detection
            if let Err(e) = events::pipeline_event_detection(&db) {
                log::error!("Event detection error: {}", e);
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
                check_shutdown!(shutdown);
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
) -> Result<()> {
    // Step 1: Discover all commitments + enrich fees via Bitcoin Core
    log::info!("Backfill step 1: discovering commitments and enriching fees from Bitcoin Core...");
    pipeline_discover_and_enrich(db, btc_rpc)?;
    check_shutdown!(shutdown, return);

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
        check_shutdown!(shutdown, return);
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
                    check_shutdown!(shutdown, return);
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
    }
    check_shutdown!(shutdown, return);

    // Step 2b: Proof enrichment via batch explorer API + Bitcoin Core
    log::info!("Backfill step 2b: enriching proof data from batch explorer API...");
    if let Err(e) = pipeline_proof_enrichment(db, rpc, btc_rpc) {
        log::warn!("Proof enrichment error (non-fatal): {}", e);
    }
    check_shutdown!(shutdown, return);

    // Step 2c: Commit fee enrichment (parent tx of reveal for both batches and proofs)
    log::info!("Backfill step 2c: enriching commit tx fees...");
    if let Err(e) = pipeline_commit_fee_enrichment(db, btc_rpc) {
        log::warn!("Commit fee enrichment error (non-fatal): {}", e);
    }
    check_shutdown!(shutdown, return);

    // Step 3: Count all L2 txs (large batches)
    log::info!("Backfill step 3: counting L2 transactions...");
    let backfill_batch_size = if config.backfill_only { 50 } else { config.batches_per_cycle };
    loop {
        check_shutdown!(shutdown);
        let uncounted = db.uncounted_batch_indexes(backfill_batch_size as u32)?;
        if uncounted.is_empty() {
            break;
        }
        for (batch_idx, start, end) in &uncounted {
            check_shutdown!(shutdown, return);
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
        check_shutdown!(shutdown);
        let uncounted = db.uncounted_gas_batch_indexes(backfill_batch_size as u32)?;
        if uncounted.is_empty() {
            break;
        }
        for (batch_idx, start, end) in &uncounted {
            check_shutdown!(shutdown, return);
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

/// Size of each parallel chunk of block heights to scan concurrently.
const PARALLEL_CHUNK_SIZE: usize = 500;

/// Number of rayon threads for I/O-bound Bitcoin Core RPC calls.
const RPC_THREAD_COUNT: usize = 16;

// ---- Pipeline 1+2: Discover Commitments + Enrich Fees via Bitcoin Core ----

/// Discovery and fee enrichment using Bitcoin Core as the primary source.
/// Scans new Bitcoin blocks for sequencer commitment transactions, inserts commitments,
/// and enriches fee data in a single pass.
///
/// Uses chunked parallel processing: each chunk of PARALLEL_CHUNK_SIZE blocks is
/// scanned concurrently via rayon, then results are written to DB sequentially
/// in a transaction. Progress is checkpointed after each chunk for crash safety.
fn pipeline_discover_and_enrich(
    db: &Database,
    btc_rpc: &BitcoinCoreClient,
) -> Result<()> {
    let btc_height = btc_rpc.get_block_height()?;
    let last_scanned = db
        .get_sync_value("last_scanned_l1_height")?
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(btc_height.saturating_sub(17280));

    if last_scanned >= btc_height {
        log::debug!("No new Bitcoin blocks to scan (at {})", btc_height);
        return Ok(());
    }

    let start = last_scanned + 1;
    let total_blocks = (btc_height - start + 1) as usize;

    // Try scanblocks (Bitcoin Core v25+, blockfilterindex=1) to pre-filter heights.
    // Falls back to scanning all blocks if unavailable.
    let heights_to_scan: Vec<u64> = match btc_rpc.scan_blocks_for_address(
        CITREA_SEQUENCER_ADDRESS, start, btc_height,
    ) {
        Ok(relevant) => {
            log::info!(
                "scanblocks found {} relevant blocks out of {} (skipping {})",
                relevant.len(), total_blocks, total_blocks - relevant.len()
            );
            relevant
        }
        Err(e) => {
            log::info!(
                "scanblocks unavailable ({}), scanning all {} blocks",
                e, total_blocks
            );
            (start..=btc_height).collect()
        }
    };

    let scan_count = heights_to_scan.len();
    log::info!(
        "Bitcoin block scan: {} blocks to scan ({} to {}), chunk_size={}, threads={}",
        scan_count, start, btc_height, PARALLEL_CHUNK_SIZE, RPC_THREAD_COUNT
    );

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(RPC_THREAD_COUNT)
        .build()
        .map_err(|e| crate::error::AppError::App(format!("Failed to build rayon pool: {}", e)))?;

    let mut total_inserted = 0u64;
    let mut total_enriched = 0u64;
    let mut lowest_failed_height: Option<u64> = None;

    for chunk in heights_to_scan.chunks(PARALLEL_CHUNK_SIZE) {
        let heights: Vec<u64> = chunk.to_vec();

        // Phase 1: Parallel RPC scan — no DB access
        // Returns Ok(Some(...)) for blocks with txs, Ok(None) for empty blocks, Err for failures
        let scan_results: Vec<std::result::Result<Option<(u64, Vec<FoundSequencerTx>)>, u64>> = pool.install(|| {
            heights
                .par_iter()
                .map(|height| {
                    match btc_rpc.find_sequencer_tx_in_block(*height, CITREA_SEQUENCER_ADDRESS) {
                        Ok(txs) => {
                            if txs.is_empty() {
                                Ok(None)
                            } else {
                                Ok(Some((*height, txs)))
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to scan block {}: {}", height, e);
                            Err(*height)
                        }
                    }
                })
                .collect()
        });

        // Separate successes from failures
        let mut successful: Vec<(u64, Vec<FoundSequencerTx>)> = Vec::new();
        for result in scan_results {
            match result {
                Ok(Some(pair)) => successful.push(pair),
                Ok(None) => {}
                Err(failed_height) => {
                    lowest_failed_height = Some(match lowest_failed_height {
                        Some(prev) => prev.min(failed_height),
                        None => failed_height,
                    });
                }
            }
        }
        let scan_results = successful;

        // Phase 2: Sort by height for sequential l2_start derivation
        let mut sorted_results = scan_results;
        sorted_results.sort_by_key(|(h, _)| *h);

        // Phase 3: Write to DB in a single transaction
        if !sorted_results.is_empty() {
            db.begin_transaction()?;
            for (height, found_txs) in &sorted_results {
                for tx in found_txs {
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

                    if db.insert_commitment(batch_idx, &tx.merkle_root, l2_start, l2_end, *height)? {
                        total_inserted += 1;
                    }

                    db.enrich_commitment_fee(
                        batch_idx,
                        &tx.txid,
                        tx.fee_sats,
                        tx.weight,
                        Some(tx.block_time),
                    )?;
                    total_enriched += 1;

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
            }
            db.commit_transaction()?;
        }

        // Phase 4: Log progress
        log::info!(
            "Bitcoin scan progress: {} commitments found, {} enriched so far",
            total_inserted, total_enriched
        );
    }

    // Only checkpoint up to the block before the lowest failure, so failed blocks
    // get retried on the next cycle. If no failures, advance to btc_height.
    let checkpoint = match lowest_failed_height {
        Some(failed) => {
            log::warn!(
                "Block scan had failures; checkpointing to {} (lowest failed: {})",
                failed - 1,
                failed
            );
            failed - 1
        }
        None => btc_height,
    };
    db.set_sync_value("last_scanned_l1_height", &checkpoint.to_string())?;

    db.set_sync_value("btc_core_height", &btc_height.to_string())?;

    if total_inserted > 0 || total_enriched > 0 {
        log::info!(
            "Bitcoin scan complete: {} new commitments, {} enriched (now {} total, {} enriched)",
            total_inserted,
            total_enriched,
            db.commitment_count()?,
            db.enriched_count()?
        );
    } else {
        log::debug!("No new commitments found in {} blocks", total_blocks);
    }

    Ok(())
}

// ---- Bootstrap from JSON ----

fn bootstrap_from_json(db: &Database, path: &Path) -> Result<()> {
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
) -> Result<()> {
    let unenriched = db.unenriched_batch_indexes_with_height(10000)?;
    if unenriched.is_empty() {
        log::debug!("All commitments already fee-enriched");
        return Ok(());
    }

    // Group by l1_height to avoid fetching the same block twice
    let mut by_height: HashMap<u64, Vec<u64>> = HashMap::new();
    for (batch_index, l1_height) in &unenriched {
        by_height
            .entry(*l1_height)
            .or_default()
            .push(*batch_index);
    }

    let heights: Vec<(u64, Vec<u64>)> = by_height.into_iter().collect();
    let total_heights = heights.len();
    log::info!("Fee enrichment: {} unique heights to scan", total_heights);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(RPC_THREAD_COUNT)
        .build()
        .map_err(|e| crate::error::AppError::App(format!("Failed to build rayon pool: {}", e)))?;

    let mut total_matched = 0u64;

    // Process in chunks for batched DB writes + progress logging
    for chunk in heights.chunks(PARALLEL_CHUNK_SIZE) {
        // Phase 1: Parallel RPC scan — collect (l1_height, batch_indexes, found_txs)
        let scan_results: Vec<(u64, Vec<u64>, Vec<FoundSequencerTx>)> = pool.install(|| {
            chunk
                .par_iter()
                .filter_map(|(l1_height, batch_indexes)| {
                    match btc_rpc.find_sequencer_tx_in_block(*l1_height, CITREA_SEQUENCER_ADDRESS) {
                        Ok(txs) => {
                            if txs.is_empty() {
                                None
                            } else {
                                Some((*l1_height, batch_indexes.clone(), txs))
                            }
                        }
                        Err(e) => {
                            log::error!(
                                "Failed to scan block {} for sequencer txs: {}",
                                l1_height,
                                e
                            );
                            None
                        }
                    }
                })
                .collect()
        });

        // Phase 2: Write to DB in a single transaction
        if !scan_results.is_empty() {
            db.begin_transaction()?;
            for (_l1_height, batch_indexes, found_txs) in &scan_results {
                for found_tx in found_txs {
                    let batch_idx = found_tx.batch_number as u64;
                    if batch_indexes.contains(&batch_idx) {
                        db.enrich_commitment_fee(
                            batch_idx,
                            &found_tx.txid,
                            found_tx.fee_sats,
                            found_tx.weight,
                            Some(found_tx.block_time),
                        )?;
                        total_matched += 1;
                    }
                }
            }
            db.commit_transaction()?;
        }
    }

    if total_matched > 0 {
        log::info!(
            "Enriched {} commitments with fee data ({}/{} total)",
            total_matched,
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
) -> Result<()> {
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

/// Lookup (or cache) the commit tx for a reveal txid and return its info.
fn resolve_commit_tx(
    btc_rpc: &BitcoinCoreClient,
    cache: &mut std::collections::HashMap<String, crate::bitcoin_rpc::CommitTxInfo>,
    reveal_txid: &str,
    label: &str,
    batch_idx: u64,
) -> Option<crate::bitcoin_rpc::CommitTxInfo> {
    if let Some(cached) = cache.get(reveal_txid) {
        return Some(cached.clone());
    }
    match btc_rpc.get_commit_tx_info(reveal_txid) {
        Ok(info) => {
            cache.insert(reveal_txid.to_string(), info.clone());
            Some(info)
        }
        Err(e) => {
            log::warn!(
                "Failed to fetch {} commit tx for batch {} (reveal {}): {}",
                label,
                batch_idx,
                &reveal_txid[..16.min(reveal_txid.len())],
                e
            );
            None
        }
    }
}

/// Fetch commit (parent) tx fees for both batch commitments and proof reveals.
/// The commit tx is the parent of the reveal tx, found via vin[0].txid.
fn pipeline_commit_fee_enrichment(
    db: &Database,
    btc_rpc: &BitcoinCoreClient,
) -> Result<()> {
    let mut cache: std::collections::HashMap<String, crate::bitcoin_rpc::CommitTxInfo> =
        std::collections::HashMap::new();

    // Part 1: Batch commit fees
    let unenriched = db.commit_unenriched_batch_indexes(10000)?;
    if !unenriched.is_empty() {
        log::info!(
            "Commit fee enrichment: {} batches need commit tx fees",
            unenriched.len()
        );

        let mut enriched = 0u64;
        for (batch_idx, btc_txid) in &unenriched {
            let commit_info = match resolve_commit_tx(btc_rpc, &mut cache, btc_txid, "batch", *batch_idx) {
                Some(info) => info,
                None => continue,
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
        for (batch_idx, proof_txid) in &unenriched_proof {
            let commit_info = match resolve_commit_tx(btc_rpc, &mut cache, proof_txid, "proof", *batch_idx) {
                Some(info) => info,
                None => continue,
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
) -> Result<()> {
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
) -> Result<()> {
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
