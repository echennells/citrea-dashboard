use citreaanalysis::citrea_decoder::{decode_from_witness, CitreaCommitment};
use citreaanalysis::citrea_rpc::{CitreaRpcClient, CommitmentWithL1Height};
use citreaanalysis::mempool_api::{MempoolClient, MempoolTransaction, CITREA_SEQUENCER_ADDRESS};
use clap::Parser;
use std::error::Error;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Query and decode Citrea sequencer commitments from Bitcoin"
)]
struct Args {
    /// Use the Citrea L2 RPC to fetch batch data (faster, no rate limits)
    #[arg(long, default_value = "true")]
    use_rpc: bool,

    /// Scan all L1 heights to find every commitment (slower but complete)
    #[arg(long)]
    scan_all: bool,

    /// Number of recent L1 blocks to scan for commitments (default: 100)
    #[arg(long, default_value = "100")]
    l1_blocks: u64,

    /// Show detailed output for each commitment
    #[arg(long)]
    verbose: bool,

    /// Export results to JSON file
    #[arg(long)]
    export: Option<String>,

    /// Query a specific transaction by txid (uses mempool.space)
    #[arg(long)]
    txid: Option<String>,

    /// Limit the number of transactions to fetch from mempool.space (0 = all)
    #[arg(long, default_value = "100")]
    limit: usize,

    /// Custom sequencer address for mempool.space queries
    #[arg(long)]
    address: Option<String>,

    /// Custom Citrea RPC URL
    #[arg(long)]
    rpc_url: Option<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    // If a specific txid is provided, decode it via mempool.space
    if let Some(txid) = &args.txid {
        let mempool = MempoolClient::new();
        return decode_single_tx(&mempool, txid);
    }

    if args.use_rpc {
        run_rpc_mode(&args)
    } else {
        run_mempool_mode(&args)
    }
}

/// Primary mode: query the Citrea L2 RPC directly
fn run_rpc_mode(args: &Args) -> Result<(), Box<dyn Error>> {
    let rpc = match &args.rpc_url {
        Some(url) => CitreaRpcClient::with_url(url),
        None => CitreaRpcClient::new(),
    };

    println!("===========================================");
    println!("  Citrea Batch Commitment Analyzer");
    println!("  (via Citrea L2 RPC)");
    println!("===========================================\n");

    // Get current status
    let summary = rpc.get_summary()?;
    println!("L1 (Bitcoin) synced to block: {}", summary.l1_height);
    println!("L2 (Citrea) synced to block:  {}", summary.l2_height);
    if let Some(idx) = summary.latest_batch_index {
        println!("Latest batch index:           {}", idx);
        println!("Estimated total commitments:  ~{}", idx + 1);
    }
    println!();

    let commitments = if args.scan_all {
        println!("Scanning all L1 heights for commitments...");
        rpc.scan_all_commitments(Some(&|found, scanned, total| {
            if scanned % 100 == 0 || scanned == total {
                eprint!(
                    "\r  Scanned {}/{} L1 blocks, found {} commitments...",
                    scanned, total, found
                );
            }
        }))?
    } else {
        let end = summary.l1_height;
        let start = end.saturating_sub(args.l1_blocks);
        println!(
            "Scanning L1 blocks {} to {} ({} blocks)...",
            start,
            end,
            end - start + 1
        );
        rpc.scan_commitments(start, end, Some(&|found, scanned, total| {
            if scanned % 50 == 0 || scanned == total {
                eprint!(
                    "\r  Scanned {}/{} L1 blocks, found {} commitments...",
                    scanned, total, found
                );
            }
        }))?
    };
    eprintln!();
    println!();

    print_rpc_results(&commitments, args.verbose);

    // Export if requested
    if let Some(path) = &args.export {
        let json = serde_json::to_string_pretty(&commitments)?;
        std::fs::write(path, &json)?;
        println!("Exported {} commitments to {}", commitments.len(), path);
    }

    Ok(())
}

fn print_rpc_results(commitments: &[CommitmentWithL1Height], verbose: bool) {
    println!("===========================================");
    println!("  RESULTS");
    println!("===========================================\n");

    println!("Total commitments found: {}", commitments.len());

    if commitments.is_empty() {
        println!("No commitments found in the scanned range.");
        println!("Try increasing --l1-blocks or using --scan-all.");
        return;
    }

    let min_batch = commitments.iter().map(|c| c.batch_index()).min().unwrap();
    let max_batch = commitments.iter().map(|c| c.batch_index()).max().unwrap();
    let min_l2 = commitments.iter().map(|c| c.l2_end_block()).min().unwrap();
    let max_l2 = commitments.iter().map(|c| c.l2_end_block()).max().unwrap();
    let min_l1 = commitments.iter().map(|c| c.l1_height).min().unwrap();
    let max_l1 = commitments.iter().map(|c| c.l1_height).max().unwrap();

    println!();
    println!("--- Batch Range ---");
    println!("  Earliest batch: #{}", min_batch);
    println!("  Latest batch:   #{}", max_batch);
    println!("  Span:           {} batches", max_batch - min_batch + 1);
    println!();

    println!("--- L2 Block Range ---");
    println!("  Earliest L2 end block: {}", min_l2);
    println!("  Latest L2 end block:   {}", max_l2);
    if max_batch > min_batch {
        println!(
            "  Avg L2 blocks per batch: {:.0}",
            (max_l2 - min_l2) as f64 / (max_batch - min_batch) as f64
        );
    }
    println!();

    println!("--- L1 Block Range ---");
    println!("  Earliest L1 height: {}", min_l1);
    println!("  Latest L1 height:   {}", max_l1);
    println!("  L1 block span:      {}", max_l1 - min_l1 + 1);
    if max_batch > min_batch {
        let l1_span = (max_l1 - min_l1 + 1) as f64;
        let batch_span = (max_batch - min_batch) as f64;
        println!(
            "  Avg L1 blocks between batches: {:.1}",
            l1_span / batch_span
        );
    }
    println!();

    // Commitment frequency stats
    let mut l1_heights_with_commits: Vec<u64> =
        commitments.iter().map(|c| c.l1_height).collect();
    l1_heights_with_commits.sort();
    l1_heights_with_commits.dedup();

    let commits_per_l1: Vec<usize> = l1_heights_with_commits
        .iter()
        .map(|h| commitments.iter().filter(|c| c.l1_height == *h).count())
        .collect();

    let max_per_l1 = commits_per_l1.iter().max().unwrap_or(&0);
    let avg_per_l1 = if !commits_per_l1.is_empty() {
        commitments.len() as f64 / commits_per_l1.len() as f64
    } else {
        0.0
    };

    println!("--- Commitment Density ---");
    println!(
        "  L1 blocks with commitments: {} / {}",
        l1_heights_with_commits.len(),
        max_l1 - min_l1 + 1
    );
    println!("  Max commitments in one L1 block: {}", max_per_l1);
    println!("  Avg commitments per L1 block (when present): {:.1}", avg_per_l1);
    println!();

    // Recent commitments table
    println!("--- Recent Commitments ---");
    println!(
        "  {:>8} {:>12} {:>12} {}",
        "Batch", "L2 End Blk", "L1 Height", "Merkle Root"
    );
    println!("  {}", "-".repeat(70));

    let mut sorted: Vec<_> = commitments.iter().collect();
    sorted.sort_by_key(|c| std::cmp::Reverse(c.batch_index()));

    for c in sorted.iter().take(20) {
        let root = c.merkle_root();
        let short_root = if root.len() > 18 {
            &root[..18]
        } else {
            root
        };
        println!(
            "  {:>8} {:>12} {:>12} {}...",
            c.batch_index(),
            c.l2_end_block(),
            c.l1_height,
            short_root,
        );
    }

    if commitments.len() > 20 {
        println!("  ... and {} more", commitments.len() - 20);
    }
    println!();

    if verbose {
        println!("--- All Commitments ---\n");
        for c in &sorted {
            println!(
                "  Batch #{} | L2 end {} | L1 {} | Root: {}",
                c.batch_index(),
                c.l2_end_block(),
                c.l1_height,
                c.merkle_root(),
            );
        }
        println!();
    }
}

/// Fallback mode: query mempool.space for Bitcoin transactions
fn run_mempool_mode(args: &Args) -> Result<(), Box<dyn Error>> {
    let client = MempoolClient::new();
    let address = args
        .address
        .as_deref()
        .unwrap_or(CITREA_SEQUENCER_ADDRESS);

    println!("===========================================");
    println!("  Citrea Transaction Query Tool");
    println!("  (via mempool.space)");
    println!("===========================================\n");

    // Get address stats
    println!("Fetching address statistics...");
    let stats = client.get_address_stats(address)?;
    println!("Address: {}", address);
    println!("Total transactions: {}", stats.chain_stats.tx_count);
    println!(
        "Total received: {:.8} BTC",
        stats.chain_stats.funded_txo_sum as f64 / 100_000_000.0
    );
    println!();

    let limit = if args.limit == 0 {
        stats.chain_stats.tx_count as usize
    } else {
        args.limit
    };

    println!("Fetching up to {} transactions...", limit);

    let txs = fetch_txs_with_pagination(&client, address, limit)?;
    println!("\rFetched {} transactions.      ", txs.len());
    println!();

    // Decode each transaction
    let mut commitments: Vec<CitreaCommitment> = Vec::new();
    let mut decode_errors = 0;

    println!("Decoding Citrea commitments...");

    for (i, tx) in txs.iter().take(limit).enumerate() {
        match decode_tx_witness(tx) {
            Ok(mut commitment) => {
                commitment.block_height = tx.status.block_height;
                commitment.block_time = tx.status.block_time;
                commitments.push(commitment);
            }
            Err(_e) => {
                if args.verbose {
                    eprintln!("  Failed to decode {}: {}", tx.txid, _e);
                }
                decode_errors += 1;
            }
        }

        if (i + 1) % 100 == 0 {
            eprint!("\rProcessed {}/{}...", i + 1, limit.min(txs.len()));
        }
    }
    eprintln!("\rProcessed {} transactions.      ", limit.min(txs.len()));
    println!();

    print_mempool_results(&commitments, decode_errors, limit.min(txs.len()), args.verbose);

    if let Some(path) = &args.export {
        let json = serde_json::to_string_pretty(&commitments)?;
        std::fs::write(path, json)?;
        println!("Exported {} commitments to {}", commitments.len(), path);
    }

    Ok(())
}

fn print_mempool_results(
    commitments: &[CitreaCommitment],
    decode_errors: usize,
    total_processed: usize,
    verbose: bool,
) {
    println!("===========================================");
    println!("  SUMMARY");
    println!("===========================================\n");

    println!("Total transactions processed: {}", total_processed);
    println!("Successfully decoded: {}", commitments.len());
    println!("Failed to decode: {}", decode_errors);
    println!();

    if commitments.is_empty() {
        return;
    }

    let mut sorted = commitments.to_vec();
    sorted.sort_by_key(|c| c.body.batch_number);

    let min_batch = sorted.first().unwrap().body.batch_number;
    let max_batch = sorted.last().unwrap().body.batch_number;
    let min_l2 = sorted.iter().map(|c| c.body.l2_block_height).min().unwrap();
    let max_l2 = sorted.iter().map(|c| c.body.l2_block_height).max().unwrap();

    println!("--- Batch Range ---");
    println!("  Earliest batch: #{}", min_batch);
    println!("  Latest batch:   #{}", max_batch);
    println!("  Total batches:  {}", max_batch - min_batch + 1);
    println!();

    println!("--- L2 Block Range ---");
    println!("  Earliest L2 block: {}", min_l2);
    println!("  Latest L2 block:   {}", max_l2);
    if max_batch > min_batch {
        println!(
            "  Avg blocks per batch: {:.1}",
            (max_l2 - min_l2) as f64 / (max_batch - min_batch) as f64
        );
    }
    println!();

    println!("--- Recent Commitments ---");
    println!(
        "  {:>8} {:>12} {:>12} {:>10} {}",
        "Batch", "L2 Block", "BTC Block", "Nonce", "State Commitment"
    );
    println!("  {}", "-".repeat(80));

    for c in sorted.iter().rev().take(10) {
        println!(
            "  {:>8} {:>12} {:>12} {:>10} {}...",
            c.body.batch_number,
            c.body.l2_block_height,
            c.block_height
                .map(|h| h.to_string())
                .unwrap_or_else(|| "pending".to_string()),
            c.nonce,
            &c.state_commitment_hex()[..16],
        );
    }
    println!();

    if verbose {
        println!("--- All Commitments ---\n");
        for c in &sorted {
            println!("{}", c);
        }
    }
}

fn fetch_txs_with_pagination(
    client: &MempoolClient,
    address: &str,
    limit: usize,
) -> Result<Vec<MempoolTransaction>, Box<dyn Error>> {
    if limit <= 50 {
        return client.get_address_txs(address);
    }

    let mut all_txs = Vec::new();
    let mut last_txid: Option<String> = None;

    loop {
        let batch = match &last_txid {
            None => client.get_address_txs(address)?,
            Some(txid) => client.get_address_txs_after(address, txid)?,
        };

        if batch.is_empty() {
            break;
        }

        last_txid = batch.last().map(|tx| tx.txid.clone());
        all_txs.extend(batch);

        eprint!("\rFetched {} transactions...", all_txs.len());

        if all_txs.len() >= limit {
            break;
        }

        // Rate limiting - be nice to mempool.space
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    Ok(all_txs)
}

fn decode_tx_witness(tx: &MempoolTransaction) -> Result<CitreaCommitment, Box<dyn Error>> {
    let witness = tx
        .vin
        .first()
        .and_then(|vin| vin.witness.as_ref())
        .ok_or("No witness data")?;

    let commitment = decode_from_witness(&tx.txid, witness)?;
    Ok(commitment)
}

fn decode_single_tx(client: &MempoolClient, txid: &str) -> Result<(), Box<dyn Error>> {
    println!("Fetching transaction {}...", txid);

    let tx = client.get_tx(txid)?;
    println!("Transaction found in block {:?}", tx.status.block_height);
    println!();

    match decode_tx_witness(&tx) {
        Ok(mut commitment) => {
            commitment.block_height = tx.status.block_height;
            commitment.block_time = tx.status.block_time;
            println!("{}", commitment);
            println!("Raw state commitment: {}", commitment.state_commitment_hex());
            println!(
                "Sequencer pubkey: {}",
                commitment.sequencer_pubkey_hex()
            );
        }
        Err(e) => {
            eprintln!("Failed to decode as Citrea commitment: {}", e);
            eprintln!("\nThis may not be a Citrea sequencer commitment transaction.");
        }
    }

    Ok(())
}
