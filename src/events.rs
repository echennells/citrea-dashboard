use crate::db::Database;
use crate::error::Result;

/// Mainnet launch: Jan 27 2026 00:00:00 UTC
const MAINNET_TS: i64 = 1769472000;

/// Minimum consecutive low-activity batches to report
const LOW_ACTIVITY_MIN_STREAK: usize = 3;
/// Tx count below which a batch is considered "low activity"
const LOW_ACTIVITY_TX_THRESHOLD: u64 = 10;
/// Streak length at which severity becomes "high"
const LOW_ACTIVITY_HIGH_THRESHOLD: usize = 10;

/// Minimum sats/tx to report a fee spike
const FEE_SPIKE_THRESHOLD: f64 = 1000.0;
/// Sats/tx above which severity becomes "high"
const FEE_SPIKE_HIGH_THRESHOLD: f64 = 2000.0;

/// Proof delay (L1 blocks) above which we report an outage
const PROOF_DELAY_THRESHOLD: u64 = 50;
/// Proof delay at which severity becomes "high"
const PROOF_DELAY_HIGH_THRESHOLD: u64 = 100;

/// RPC lag (in blocks) above which severity becomes "high"
const RPC_LAG_HIGH_THRESHOLD: i64 = 5;

/// An event to be persisted to the events table.
pub struct Event {
    pub event_key: String,
    pub event_type: String,
    pub severity: String,
    pub title: String,
    pub detail: String,
    pub batch_start: Option<u64>,
    pub batch_end: Option<u64>,
    pub time_start: Option<u64>,
    pub time_end: Option<u64>,
}

/// Detect low-activity streaks: runs of 3+ consecutive batches with < 10 txs.
fn detect_low_activity_streaks(db: &Database) -> Result<Vec<Event>> {
    let rows = db.fetch_batches_for_event_detection(MAINNET_TS)?;
    let mut events = Vec::new();
    let mut streak: Vec<&crate::db::EventBatchRow> = Vec::new();

    let flush_streak = |streak: &[&crate::db::EventBatchRow], events: &mut Vec<Event>| {
        if streak.len() >= LOW_ACTIVITY_MIN_STREAK {
            let first = streak[0];
            let last = streak[streak.len() - 1];
            let total_txs: u64 = streak
                .iter()
                .map(|r| r.l2_tx_count.unwrap_or(0))
                .sum();
            let avg = total_txs as f64 / streak.len() as f64;
            let severity = if streak.len() >= LOW_ACTIVITY_HIGH_THRESHOLD {
                "high"
            } else {
                "medium"
            };
            events.push(Event {
                event_key: format!(
                    "low_activity:{}:{}",
                    first.batch_index, last.batch_index
                ),
                event_type: "low_activity".to_string(),
                severity: severity.to_string(),
                title: format!(
                    "Low activity: {} batches with < {} txs",
                    streak.len(),
                    LOW_ACTIVITY_TX_THRESHOLD
                ),
                detail: format!(
                    "Batches {}–{} averaged {:.1} txs/batch",
                    first.batch_index, last.batch_index, avg
                ),
                batch_start: Some(first.batch_index),
                batch_end: Some(last.batch_index),
                time_start: first.btc_block_time,
                time_end: last.btc_block_time,
            });
        }
    };

    for row in &rows {
        if row.l2_tx_count.map_or(false, |c| c < LOW_ACTIVITY_TX_THRESHOLD) {
            streak.push(row);
        } else {
            flush_streak(&streak, &mut events);
            streak.clear();
        }
    }
    flush_streak(&streak, &mut events);

    Ok(events)
}

/// Detect fee spikes: batches where sats/tx > 1000.
fn detect_fee_spikes(db: &Database) -> Result<Vec<Event>> {
    let median_sats_per_tx = db.fetch_median_sats_per_tx(MAINNET_TS)?.unwrap_or(0.0);
    let spikes = db.fetch_fee_spikes_per_tx(MAINNET_TS, FEE_SPIKE_THRESHOLD)?;
    let mut events = Vec::new();

    for f in &spikes {
        let sats_per_tx = f.total_fee as f64 / f.l2_tx_count.unwrap_or(1).max(1) as f64;
        let severity = if sats_per_tx > FEE_SPIKE_HIGH_THRESHOLD {
            "high"
        } else {
            "medium"
        };
        events.push(Event {
            event_key: format!("fee_spike:{}", f.batch_index),
            event_type: "fee_spike".to_string(),
            severity: severity.to_string(),
            title: format!(
                "Fee spike: {:.0} sats/tx on batch {}",
                sats_per_tx, f.batch_index
            ),
            detail: format!(
                "Total fee {} sats across {} txs (median is {:.0} sats/tx)",
                f.total_fee,
                f.l2_tx_count.unwrap_or(0),
                median_sats_per_tx
            ),
            batch_start: Some(f.batch_index),
            batch_end: Some(f.batch_index),
            time_start: f.btc_block_time,
            time_end: f.btc_block_time,
        });
    }

    Ok(events)
}

/// Detect proof system outages: batches that took >50 L1 blocks to prove,
/// or batches currently waiting >50 blocks without a proof.
fn detect_proof_delays(db: &Database) -> Result<Vec<Event>> {
    let btc_height = match db.get_sync_value("btc_core_height")? {
        Some(s) => s.parse::<u64>()?,
        None => return Ok(Vec::new()),
    };

    let rows = db.fetch_proof_delay_batches(MAINNET_TS, PROOF_DELAY_THRESHOLD, btc_height)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    // Group consecutive batches into outage streaks
    let mut events = Vec::new();
    let mut streak: Vec<&crate::db::ProofDelayRow> = Vec::new();

    let flush = |streak: &[&crate::db::ProofDelayRow], events: &mut Vec<Event>| {
        if streak.is_empty() {
            return;
        }
        let first = streak[0];
        let last = streak[streak.len() - 1];
        let max_delay = streak.iter().map(|r| r.delay_blocks).max().unwrap_or(0);
        let severity = if max_delay > PROOF_DELAY_HIGH_THRESHOLD { "high" } else { "medium" };
        let any_unproven = streak.iter().any(|r| !r.is_proven);
        let status = if any_unproven { "ongoing" } else { "resolved" };
        let delay_hours = max_delay as f64 * 10.0 / 60.0; // ~10 min per block

        events.push(Event {
            event_key: format!("proof_delay:{}:{}", first.batch_index, last.batch_index),
            event_type: "proof_delay".to_string(),
            severity: severity.to_string(),
            title: format!(
                "Proof delay: {} batches waited up to {:.0}h ({})",
                streak.len(), delay_hours, status
            ),
            detail: format!(
                "Batches {}–{}, max delay {} L1 blocks",
                first.batch_index, last.batch_index, max_delay
            ),
            batch_start: Some(first.batch_index),
            batch_end: Some(last.batch_index),
            time_start: first.btc_block_time,
            time_end: last.btc_block_time,
        });
    };

    for row in &rows {
        if let Some(prev) = streak.last() {
            if row.batch_index != prev.batch_index + 1 {
                flush(&streak, &mut events);
                streak.clear();
            }
        }
        streak.push(row);
    }
    flush(&streak, &mut events);

    Ok(events)
}

/// Detect RPC lag: Citrea RPC L1 height behind Bitcoin Core height.
fn detect_rpc_lag(db: &Database) -> Result<Vec<Event>> {
    let btc_height = match db.get_sync_value("btc_core_height")? {
        Some(s) => s.parse::<i64>()?,
        None => return Ok(Vec::new()),
    };
    let citrea_height = match db.get_sync_value("citrea_rpc_l1_height")? {
        Some(s) => s.parse::<i64>()?,
        None => return Ok(Vec::new()),
    };

    let lag = btc_height - citrea_height;
    if lag <= 0 {
        return Ok(Vec::new());
    }

    let severity = if lag > RPC_LAG_HIGH_THRESHOLD {
        "high"
    } else {
        "medium"
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok(vec![Event {
        event_key: "rpc_lag".to_string(),
        event_type: "rpc_lag".to_string(),
        severity: severity.to_string(),
        title: format!(
            "Citrea RPC {} blocks behind Bitcoin Core",
            lag
        ),
        detail: format!(
            "Bitcoin Core at block {}, Citrea RPC synced to {}",
            btc_height, citrea_height
        ),
        batch_start: None,
        batch_end: None,
        time_start: Some(now),
        time_end: Some(now),
    }])
}

/// Run all event detectors inside a transaction.
///
/// 1. mark_all_events_resolved() — set resolved=1 on all active
/// 2. Run each detector and upsert results (resolved=0 for current events)
/// 3. Commit — events no longer detected stay resolved, still-detected stay active
pub fn pipeline_event_detection(db: &Database) -> Result<()> {
    let mut all_events = Vec::new();

    // Collect events from all detectors (outside transaction to avoid long locks)
    match detect_low_activity_streaks(db) {
        Ok(evts) => all_events.extend(evts),
        Err(e) => log::warn!("Low activity detector error: {}", e),
    }
    match detect_fee_spikes(db) {
        Ok(evts) => all_events.extend(evts),
        Err(e) => log::warn!("Fee spike detector error: {}", e),
    }
    match detect_proof_delays(db) {
        Ok(evts) => all_events.extend(evts),
        Err(e) => log::warn!("Proof delay detector error: {}", e),
    }
    match detect_rpc_lag(db) {
        Ok(evts) => all_events.extend(evts),
        Err(e) => log::warn!("RPC lag detector error: {}", e),
    }

    // Atomic update: mark all resolved, then upsert current events
    db.begin_transaction()?;
    db.mark_all_events_resolved()?;
    for event in &all_events {
        db.upsert_event(event)?;
    }
    db.commit_transaction()?;

    log::info!("Event detection: {} active events", all_events.len());
    Ok(())
}
