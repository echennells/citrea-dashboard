use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::error::Error;
use std::path::Path;

/// Database handle wrapping a SQLite connection
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open (or create) the SQLite database at the given path
    pub fn open(path: &Path) -> Result<Self, Box<dyn Error>> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let db = Database { conn };
        db.create_tables()?;
        db.migrate()?;
        Ok(db)
    }

    fn create_tables(&self) -> Result<(), Box<dyn Error>> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS commitments (
                batch_index       INTEGER PRIMARY KEY,
                merkle_root       TEXT NOT NULL,
                l2_start_block    INTEGER NOT NULL,
                l2_end_block      INTEGER NOT NULL,
                l1_height         INTEGER NOT NULL,
                btc_txid          TEXT,
                btc_fee_sats      INTEGER,
                btc_weight        INTEGER,
                btc_block_time    INTEGER,
                l2_tx_count       INTEGER,
                l2_gas_used       INTEGER,
                l2_gas_limit      INTEGER,
                l2_gas_used_max   INTEGER,
                l2_gas_used_min   INTEGER,
                created_at        TEXT NOT NULL DEFAULT (datetime('now')),
                fee_enriched_at   TEXT,
                tx_counted_at     TEXT,
                l2_gas_counted_at TEXT
            );

            CREATE TABLE IF NOT EXISTS sequencer_txs (
                txid              TEXT PRIMARY KEY,
                btc_block_height  INTEGER,
                btc_block_time    INTEGER,
                fee_sats          INTEGER NOT NULL,
                weight            INTEGER NOT NULL,
                size_bytes        INTEGER NOT NULL,
                decoded_batch     INTEGER,
                created_at        TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS sync_state (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS stats_snapshots (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp         TEXT NOT NULL DEFAULT (datetime('now')),
                total_l2_txs      INTEGER,
                total_batches     INTEGER,
                total_btc_fees    INTEGER,
                avg_fee_per_batch REAL
            );",
        )?;
        Ok(())
    }

    /// Migrate existing databases: add columns that may not exist yet.
    fn migrate(&self) -> Result<(), Box<dyn Error>> {
        // (column_name, SQL type)
        let columns: &[(&str, &str)] = &[
            ("l2_gas_used", "INTEGER"),
            ("l2_gas_limit", "INTEGER"),
            ("l2_gas_used_max", "INTEGER"),
            ("l2_gas_used_min", "INTEGER"),
            ("l2_gas_counted_at", "TEXT"),
            // Proof tracking columns
            ("proof_txid", "TEXT"),
            ("proof_fee_sats", "INTEGER"),
            ("proof_weight", "INTEGER"),
            ("proof_block_time", "INTEGER"),
            ("proof_l1_height", "INTEGER"),
            ("proof_enriched_at", "TEXT"),
            // Batch commit (parent of reveal) tracking columns
            ("commit_txid", "TEXT"),
            ("commit_fee_sats", "INTEGER"),
            ("commit_weight", "INTEGER"),
            ("commit_block_time", "INTEGER"),
            ("commit_enriched_at", "TEXT"),
            // Proof commit (parent of proof reveal) tracking columns
            ("proof_commit_txid", "TEXT"),
            ("proof_commit_fee_sats", "INTEGER"),
            ("proof_commit_weight", "INTEGER"),
            ("proof_commit_block_time", "INTEGER"),
            ("proof_commit_enriched_at", "TEXT"),
        ];
        for (col, typ) in columns {
            let sql = format!("ALTER TABLE commitments ADD COLUMN {} {}", col, typ);
            // Ignore "duplicate column" errors for idempotency
            match self.conn.execute_batch(&sql) {
                Ok(_) => {}
                Err(e) if e.to_string().contains("duplicate column") => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    // ---- sync_state helpers ----

    pub fn get_sync_value(&self, key: &str) -> Result<Option<String>, Box<dyn Error>> {
        let val = self
            .conn
            .query_row(
                "SELECT value FROM sync_state WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(val)
    }

    pub fn set_sync_value(&self, key: &str, value: &str) -> Result<(), Box<dyn Error>> {
        self.conn.execute(
            "INSERT INTO sync_state (key, value, updated_at)
             VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value],
        )?;
        Ok(())
    }

    // ---- commitment CRUD ----

    /// Insert a commitment if it doesn't already exist. Returns true if inserted.
    pub fn insert_commitment(
        &self,
        batch_index: u64,
        merkle_root: &str,
        l2_start_block: u64,
        l2_end_block: u64,
        l1_height: u64,
    ) -> Result<bool, Box<dyn Error>> {
        let rows = self.conn.execute(
            "INSERT OR IGNORE INTO commitments (batch_index, merkle_root, l2_start_block, l2_end_block, l1_height)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![batch_index as i64, merkle_root, l2_start_block as i64, l2_end_block as i64, l1_height as i64],
        )?;
        Ok(rows > 0)
    }

    /// Get the lowest l1_height in the DB, or None if empty.
    pub fn min_l1_height(&self) -> Result<Option<u64>, Box<dyn Error>> {
        let val = self
            .conn
            .query_row(
                "SELECT MIN(l1_height) FROM commitments",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )?;
        Ok(val.map(|v| v as u64))
    }

    /// Get the highest batch_index in the DB, or None if empty.
    pub fn max_batch_index(&self) -> Result<Option<u64>, Box<dyn Error>> {
        let val = self
            .conn
            .query_row(
                "SELECT MAX(batch_index) FROM commitments",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )?;
        Ok(val.map(|v| v as u64))
    }

    /// Get the l2_end_block for a specific batch_index.
    pub fn get_l2_end_block(&self, batch_index: u64) -> Result<Option<u64>, Box<dyn Error>> {
        let val = self
            .conn
            .query_row(
                "SELECT l2_end_block FROM commitments WHERE batch_index = ?1",
                params![batch_index as i64],
                |row| row.get::<_, i64>(0).map(|v| v as u64),
            )
            .optional()?;
        Ok(val)
    }

    /// Get the count of commitments.
    pub fn commitment_count(&self) -> Result<u64, Box<dyn Error>> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM commitments", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    // ---- fee enrichment ----

    /// Get batch_indexes that have NOT been fee-enriched yet, up to `limit`.
    pub fn unenriched_batch_indexes(&self, limit: u32) -> Result<Vec<u64>, Box<dyn Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT batch_index FROM commitments WHERE fee_enriched_at IS NULL ORDER BY batch_index ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            row.get::<_, i64>(0).map(|v| v as u64)
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Update a commitment with fee data from mempool.space.
    pub fn enrich_commitment_fee(
        &self,
        batch_index: u64,
        btc_txid: &str,
        fee_sats: u64,
        weight: u32,
        block_time: Option<u64>,
    ) -> Result<(), Box<dyn Error>> {
        self.conn.execute(
            "UPDATE commitments SET btc_txid = ?1, btc_fee_sats = ?2, btc_weight = ?3,
             btc_block_time = ?4, fee_enriched_at = datetime('now')
             WHERE batch_index = ?5",
            params![
                btc_txid,
                fee_sats as i64,
                weight as i64,
                block_time.map(|t| t as i64),
                batch_index as i64
            ],
        )?;
        Ok(())
    }

    /// Get batch_indexes + l1_height for unenriched commitments, up to `limit`.
    pub fn unenriched_batch_indexes_with_height(
        &self,
        limit: u32,
    ) -> Result<Vec<(u64, u64)>, Box<dyn Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT batch_index, l1_height FROM commitments WHERE fee_enriched_at IS NULL ORDER BY batch_index ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, i64>(1)? as u64,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Count how many commitments are fee-enriched.
    pub fn enriched_count(&self) -> Result<u64, Box<dyn Error>> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM commitments WHERE fee_enriched_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    // ---- proof enrichment ----

    /// Attach proof data to a commitment by batch_index.
    pub fn enrich_proof_data(
        &self,
        batch_index: u64,
        proof_txid: &str,
        proof_fee_sats: u64,
        proof_weight: u32,
        proof_block_time: Option<u64>,
        proof_l1_height: Option<u64>,
    ) -> Result<(), Box<dyn Error>> {
        self.conn.execute(
            "UPDATE commitments SET proof_txid = ?1, proof_fee_sats = ?2, proof_weight = ?3,
             proof_block_time = ?4, proof_l1_height = ?5, proof_enriched_at = datetime('now')
             WHERE batch_index = ?6",
            params![
                proof_txid,
                proof_fee_sats as i64,
                proof_weight as i64,
                proof_block_time.map(|t| t as i64),
                proof_l1_height.map(|h| h as i64),
                batch_index as i64
            ],
        )?;
        Ok(())
    }

    /// Count how many commitments do NOT have proof data yet.
    pub fn unenriched_proof_count(&self) -> Result<u64, Box<dyn Error>> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM commitments WHERE proof_enriched_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Count how many commitments have proof data.
    pub fn proof_enriched_count(&self) -> Result<u64, Box<dyn Error>> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM commitments WHERE proof_enriched_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Get batch_indexes + l1_height for commitments missing proof data, up to `limit`.
    /// We look at batches that have fee data (so we know their L1 height) but no proof yet.
    pub fn unenriched_proof_batch_indexes_with_height(
        &self,
        limit: u32,
    ) -> Result<Vec<(u64, u64)>, Box<dyn Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT batch_index, l1_height FROM commitments
             WHERE proof_enriched_at IS NULL AND fee_enriched_at IS NOT NULL
             ORDER BY batch_index ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, i64>(1)? as u64,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ---- commit fee enrichment (parent of reveal tx) ----

    /// Update a commitment with commit (parent of reveal) tx data.
    pub fn enrich_commit_fee(
        &self,
        batch_index: u64,
        commit_txid: &str,
        fee_sats: u64,
        weight: u32,
        block_time: u64,
    ) -> Result<(), Box<dyn Error>> {
        self.conn.execute(
            "UPDATE commitments SET commit_txid = ?1, commit_fee_sats = ?2, commit_weight = ?3,
             commit_block_time = ?4, commit_enriched_at = datetime('now')
             WHERE batch_index = ?5",
            params![
                commit_txid,
                fee_sats as i64,
                weight as i64,
                block_time as i64,
                batch_index as i64
            ],
        )?;
        Ok(())
    }

    /// Update a commitment with proof commit (parent of proof reveal) tx data.
    pub fn enrich_proof_commit_fee(
        &self,
        batch_index: u64,
        proof_commit_txid: &str,
        fee_sats: u64,
        weight: u32,
        block_time: u64,
    ) -> Result<(), Box<dyn Error>> {
        self.conn.execute(
            "UPDATE commitments SET proof_commit_txid = ?1, proof_commit_fee_sats = ?2, proof_commit_weight = ?3,
             proof_commit_block_time = ?4, proof_commit_enriched_at = datetime('now')
             WHERE batch_index = ?5",
            params![
                proof_commit_txid,
                fee_sats as i64,
                weight as i64,
                block_time as i64,
                batch_index as i64
            ],
        )?;
        Ok(())
    }

    /// Get batch_indexes + btc_txid for commitments that have NOT been commit-fee-enriched yet.
    pub fn commit_unenriched_batch_indexes(
        &self,
        limit: u32,
    ) -> Result<Vec<(u64, String)>, Box<dyn Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT batch_index, btc_txid FROM commitments
             WHERE commit_enriched_at IS NULL AND btc_txid IS NOT NULL
             ORDER BY batch_index ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, String>(1)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Get batch_indexes + proof_txid for commitments that have NOT been proof-commit-fee-enriched yet.
    pub fn proof_commit_unenriched_batch_indexes(
        &self,
        limit: u32,
    ) -> Result<Vec<(u64, String)>, Box<dyn Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT batch_index, proof_txid FROM commitments
             WHERE proof_commit_enriched_at IS NULL AND proof_txid IS NOT NULL
             ORDER BY batch_index ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, String>(1)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Count how many commitments have been commit-fee-enriched.
    pub fn commit_enriched_count(&self) -> Result<u64, Box<dyn Error>> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM commitments WHERE commit_enriched_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Count how many commitments have been proof-commit-fee-enriched.
    pub fn proof_commit_enriched_count(&self) -> Result<u64, Box<dyn Error>> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM commitments WHERE proof_commit_enriched_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Count how many batches share a given proof_txid (for splitting proof commit fees).
    pub fn count_batches_with_proof_txid(&self, proof_txid: &str) -> Result<u64, Box<dyn Error>> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM commitments WHERE proof_txid = ?1",
            params![proof_txid],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    pub fn first_batch_with_proof_txid(&self, proof_txid: &str) -> Result<Option<u64>, Box<dyn Error>> {
        let result: Option<i64> = self.conn.query_row(
            "SELECT MIN(batch_index) FROM commitments WHERE proof_txid = ?1",
            params![proof_txid],
            |row| row.get(0),
        )?;
        Ok(result.map(|v| v as u64))
    }

    /// Get the set of all batch indexes that do NOT have proof data yet.
    pub fn proof_unenriched_batch_set(&self) -> Result<HashSet<u64>, Box<dyn Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT batch_index FROM commitments WHERE proof_enriched_at IS NULL",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0).map(|v| v as u64))?;
        let mut set = HashSet::new();
        for r in rows {
            set.insert(r?);
        }
        Ok(set)
    }

    // ---- sequencer_txs ----

    /// Insert a sequencer transaction if it doesn't exist.
    pub fn insert_sequencer_tx(
        &self,
        txid: &str,
        block_height: Option<u64>,
        block_time: Option<u64>,
        fee_sats: u64,
        weight: u32,
        size_bytes: u32,
        decoded_batch: Option<u32>,
    ) -> Result<bool, Box<dyn Error>> {
        let rows = self.conn.execute(
            "INSERT OR IGNORE INTO sequencer_txs (txid, btc_block_height, btc_block_time, fee_sats, weight, size_bytes, decoded_batch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                txid,
                block_height.map(|h| h as i64),
                block_time.map(|t| t as i64),
                fee_sats as i64,
                weight as i64,
                size_bytes as i64,
                decoded_batch.map(|b| b as i64),
            ],
        )?;
        Ok(rows > 0)
    }

    /// Count sequencer_txs rows.
    pub fn sequencer_tx_count(&self) -> Result<u64, Box<dyn Error>> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sequencer_txs", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Find sequencer_tx by decoded_batch number.
    pub fn find_sequencer_tx_by_batch(
        &self,
        batch_number: u32,
    ) -> Result<Option<SequencerTxRow>, Box<dyn Error>> {
        let row = self
            .conn
            .query_row(
                "SELECT txid, btc_block_height, btc_block_time, fee_sats, weight, size_bytes
                 FROM sequencer_txs WHERE decoded_batch = ?1",
                params![batch_number as i64],
                |row| {
                    Ok(SequencerTxRow {
                        txid: row.get(0)?,
                        block_height: row.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                        block_time: row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                        fee_sats: row.get::<_, i64>(3)? as u64,
                        weight: row.get::<_, i64>(4)? as u32,
                        size_bytes: row.get::<_, i64>(5)? as u32,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Get the txid of the oldest sequencer tx (by block height) for pagination resume.
    /// Mempool.space returns txs newest-first, so the "oldest" is where we left off.
    pub fn oldest_sequencer_txid(&self) -> Result<Option<String>, Box<dyn Error>> {
        let val = self
            .conn
            .query_row(
                "SELECT txid FROM sequencer_txs ORDER BY btc_block_height ASC, rowid DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(val)
    }

    // ---- L2 tx counting ----

    /// Get batch_indexes that need L2 tx counting, up to `limit`.
    pub fn uncounted_batch_indexes(&self, limit: u32) -> Result<Vec<(u64, u64, u64)>, Box<dyn Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT batch_index, l2_start_block, l2_end_block FROM commitments
             WHERE tx_counted_at IS NULL ORDER BY batch_index ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, i64>(2)? as u64,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Set the L2 tx count for a batch.
    pub fn set_l2_tx_count(
        &self,
        batch_index: u64,
        tx_count: u64,
    ) -> Result<(), Box<dyn Error>> {
        self.conn.execute(
            "UPDATE commitments SET l2_tx_count = ?1, tx_counted_at = datetime('now')
             WHERE batch_index = ?2",
            params![tx_count as i64, batch_index as i64],
        )?;
        Ok(())
    }

    /// Count how many batches have been tx-counted.
    pub fn tx_counted_count(&self) -> Result<u64, Box<dyn Error>> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM commitments WHERE tx_counted_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    // ---- L2 gas counting ----

    /// Get batch_indexes that need L2 gas counting, up to `limit`.
    pub fn uncounted_gas_batch_indexes(&self, limit: u32) -> Result<Vec<(u64, u64, u64)>, Box<dyn Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT batch_index, l2_start_block, l2_end_block FROM commitments
             WHERE l2_gas_counted_at IS NULL ORDER BY batch_index ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, i64>(2)? as u64,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Set the L2 gas data for a batch.
    pub fn set_l2_gas_data(
        &self,
        batch_index: u64,
        gas_used: u64,
        gas_limit: u64,
        max_used: u64,
        min_used: u64,
    ) -> Result<(), Box<dyn Error>> {
        self.conn.execute(
            "UPDATE commitments SET l2_gas_used = ?1, l2_gas_limit = ?2,
             l2_gas_used_max = ?3, l2_gas_used_min = ?4, l2_gas_counted_at = datetime('now')
             WHERE batch_index = ?5",
            params![gas_used as i64, gas_limit as i64, max_used as i64, min_used as i64, batch_index as i64],
        )?;
        Ok(())
    }

    /// Count how many batches have been gas-counted.
    pub fn gas_counted_count(&self) -> Result<u64, Box<dyn Error>> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM commitments WHERE l2_gas_counted_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    // ---- stats snapshots ----

    // ---- stats snapshots ----

    /// Record a stats snapshot. total_btc_fees includes all 4 fee types (reveal + commit for both commitment and proof).
    pub fn record_stats_snapshot(&self) -> Result<(), Box<dyn Error>> {
        self.conn.execute(
            "INSERT INTO stats_snapshots (total_l2_txs, total_batches, total_btc_fees, avg_fee_per_batch)
             SELECT
                SUM(l2_tx_count),
                COUNT(*),
                COALESCE(SUM(btc_fee_sats), 0) + COALESCE(SUM(commit_fee_sats), 0)
                  + COALESCE(SUM(proof_fee_sats), 0) + COALESCE(SUM(proof_commit_fee_sats), 0),
                AVG(COALESCE(btc_fee_sats, 0) + COALESCE(commit_fee_sats, 0)
                  + COALESCE(proof_fee_sats, 0) + COALESCE(proof_commit_fee_sats, 0))
             FROM commitments",
            [],
        )?;
        Ok(())
    }
}

/// Row from sequencer_txs table
pub struct SequencerTxRow {
    pub txid: String,
    pub block_height: Option<u64>,
    pub block_time: Option<u64>,
    pub fee_sats: u64,
    pub weight: u32,
    pub size_bytes: u32,
}
