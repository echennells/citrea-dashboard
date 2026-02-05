const express = require('express');
const Database = require('better-sqlite3');
const path = require('path');
const crypto = require('crypto');

const app = express();
const PORT = parseInt(process.env.PORT, 10) || 3000;
const DB_PATH = process.env.DB_PATH || '/data/citrea.db';

let db;
try {
  db = new Database(DB_PATH, { readonly: true, fileMustExist: true });
  db.pragma('journal_mode = WAL');
  const row = db.prepare('SELECT COUNT(*) AS cnt FROM commitments').get();
  console.log(`Database loaded: ${row.cnt} commitments`);
} catch (err) {
  console.error('Failed to open database:', err.message);
  process.exit(1);
}

// Mainnet launch: Jan 27 2026 00:00:00 UTC
const MAINNET_TS = 1769472000;

// Compute the first batch at or after mainnet launch
const LAUNCH_BATCH_INDEX = (() => {
  try {
    const row = db.prepare(
      'SELECT MIN(batch_index) AS idx FROM commitments WHERE btc_block_time >= ?'
    ).get(MAINNET_TS);
    const idx = row ? row.idx : null;
    console.log(`Launch batch index: ${idx}`);
    return idx;
  } catch (e) {
    console.error('Failed to compute launch batch index:', e.message);
    return null;
  }
})();

const PRE_LAUNCH_BATCH_COUNT = (() => {
  try {
    const row = db.prepare(
      'SELECT COUNT(*) AS cnt FROM commitments WHERE btc_block_time < ?'
    ).get(MAINNET_TS);
    return row ? row.cnt : 0;
  } catch (e) { return 0; }
})();

const MAX_CHART_POINTS = 400;

// --- SQL fee fragments (single source of truth) ---
// Per-row expressions
const TOTAL_FEE = 'COALESCE(btc_fee_sats,0) + COALESCE(commit_fee_sats,0) + COALESCE(proof_fee_sats,0) + COALESCE(proof_commit_fee_sats,0)';
const COMMITMENT_FEE = 'COALESCE(btc_fee_sats,0) + COALESCE(commit_fee_sats,0)';
const PROOF_FEE = 'COALESCE(proof_fee_sats,0) + COALESCE(proof_commit_fee_sats,0)';
// Aggregated expressions
const TOTAL_FEE_SUM = 'COALESCE(SUM(btc_fee_sats),0) + COALESCE(SUM(commit_fee_sats),0) + COALESCE(SUM(proof_fee_sats),0) + COALESCE(SUM(proof_commit_fee_sats),0)';
const COMMITMENT_FEE_SUM = 'COALESCE(SUM(btc_fee_sats),0) + COALESCE(SUM(commit_fee_sats),0)';
const PROOF_FEE_SUM = 'COALESCE(SUM(proof_fee_sats),0) + COALESCE(SUM(proof_commit_fee_sats),0)';

function periodToMinTime(period, includePrelaunch) {
  const now = Math.floor(Date.now() / 1000);
  if (period === '4h') return Math.max(now - 14400, MAINNET_TS);
  if (period === '24h') return Math.max(now - 86400, MAINNET_TS);
  if (period === '7d') return Math.max(now - 7 * 86400, MAINNET_TS);
  // period === 'all'
  return (includePrelaunch ? 0 : MAINNET_TS);
}

// --- In-memory response cache (60s TTL) ---
const CACHE_TTL_MS = 60_000;
const responseCache = new Map();

function cached(keyFn, handler) {
  return (req, res) => {
    const key = keyFn(req);
    const entry = responseCache.get(key);
    if (entry && Date.now() - entry.ts < CACHE_TTL_MS) {
      res.setHeader('X-Cache', 'HIT');
      return res.json(entry.data);
    }
    const origJson = res.json.bind(res);
    res.json = (data) => {
      responseCache.set(key, { data, ts: Date.now() });
      res.setHeader('X-Cache', 'MISS');
      return origJson(data);
    };
    handler(req, res);
  };
}

app.use(express.json());
app.get('/llm.txt', (_req, res) => res.redirect(301, '/llms.txt'));
app.use(express.static(path.join(__dirname, 'public'), {
  maxAge: '1d',
  immutable: false,
  setHeaders(res, filePath) {
    if (filePath.endsWith('.html') || filePath.endsWith('.js') || filePath.endsWith('.css')) {
      res.setHeader('Cache-Control', 'no-cache');
    }
  }
}));

// --- Spark wallet setup ---
const PAYMENT_AMOUNT_SATS = 100;
const FREE_BATCH_LIMIT = 100;
const SESSION_TTL_MS = 2 * 60 * 60 * 1000; // 2 hours
const INVOICE_EXPIRY_SEC = 3600; // 1 hour

let sparkWallet = null;
const paymentSessions = new Map();
// token -> { feature, bolt11, status: 'pending'|'paid'|'expired', createdAt, invoiceId }

function isPaymentsEnabled() {
  return sparkWallet !== null;
}

function isTokenPaid(token) {
  if (!isPaymentsEnabled()) return true; // graceful fallback: everything unlocked
  if (!token) return false;
  const session = paymentSessions.get(token);
  if (!session) return false;
  return session.status === 'paid';
}

// Cleanup expired sessions every 5 minutes
setInterval(() => {
  const now = Date.now();
  for (const [token, session] of paymentSessions) {
    if (now - session.createdAt > SESSION_TTL_MS) {
      paymentSessions.delete(token);
    }
  }
}, 5 * 60 * 1000);

async function initSpark() {
  const mnemonic = process.env.SPARK_MNEMONIC;
  if (!mnemonic) {
    console.log('SPARK_MNEMONIC not set — payment gating disabled, all data freely available');
    return;
  }
  try {
    const { SparkWallet } = await import('@buildonspark/spark-sdk');
    const network = process.env.SPARK_NETWORK || 'MAINNET';
    const { wallet } = await SparkWallet.initialize({
      mnemonicOrSeed: mnemonic,
      options: { network },
    });
    sparkWallet = wallet;
    console.log(`Spark wallet initialized on ${network}`);
  } catch (err) {
    console.error('Failed to init Spark wallet:', err.message);
    console.log('Payment gating disabled due to wallet error');
    sparkWallet = null;
  }
}

// --- Payment endpoints ---

// GET /api/payment/config
app.get('/api/payment/config', (_req, res) => {
  res.json({
    enabled: isPaymentsEnabled(),
    amountSats: PAYMENT_AMOUNT_SATS,
    freeLimit: FREE_BATCH_LIMIT
  });
});

// POST /api/payment/create-invoice
app.post('/api/payment/create-invoice', async (req, res) => {
  if (!isPaymentsEnabled()) {
    return res.status(503).json({ error: 'Payments not configured' });
  }

  try {
    const receiveReq = await sparkWallet.createLightningInvoice({
      amountSats: PAYMENT_AMOUNT_SATS,
      memo: 'Citrea Dashboard: full access',
      expirySeconds: INVOICE_EXPIRY_SEC
    });

    const bolt11 = receiveReq.invoice.encodedInvoice;
    const token = crypto.randomBytes(24).toString('hex');
    paymentSessions.set(token, {
      bolt11,
      receiveRequestId: receiveReq.id,
      status: 'pending',
      createdAt: Date.now()
    });

    res.json({ token, bolt11 });
  } catch (err) {
    console.error('Invoice creation failed:', err.message);
    res.status(500).json({ error: 'Failed to create invoice' });
  }
});

// GET /api/payment/status?token=
app.get('/api/payment/status', async (req, res) => {
  if (!isPaymentsEnabled()) {
    return res.status(503).json({ error: 'Payments not configured' });
  }

  const { token } = req.query;
  if (!token) {
    return res.status(400).json({ error: 'Missing token' });
  }

  const session = paymentSessions.get(token);
  if (!session) {
    return res.json({ status: 'expired' });
  }

  if (session.status === 'paid') {
    return res.json({ status: 'paid' });
  }

  // Check if invoice has expired by time
  const elapsed = Date.now() - session.createdAt;
  if (elapsed > INVOICE_EXPIRY_SEC * 1000) {
    session.status = 'expired';
    return res.json({ status: 'expired' });
  }

  // Check invoice status via Spark
  try {
    const req = await sparkWallet.getLightningReceiveRequest(session.receiveRequestId);
    if (req && req.status === 'TRANSFER_COMPLETED') {
      session.status = 'paid';
      return res.json({ status: 'paid' });
    }
  } catch (err) {
    console.error('Error checking payment status:', err.message);
  }

  res.json({ status: 'pending' });
});

// --- Existing API endpoints ---

// GET /api/overview?period=all|7d|24h|4h&prelaunch=0|1
app.get('/api/overview', cached(req => `overview:${req.query.period || 'all'}:${req.query.prelaunch || '0'}`, (req, res) => {
  const period = req.query.period || 'all';
  const prelaunch = req.query.prelaunch === '1';
  const minTime = periodToMinTime(period, prelaunch);

  const row = db.prepare(`
    SELECT
      COUNT(*)                                           AS total_batches,
      COALESCE(SUM(l2_tx_count), 0)                      AS total_l2_txs,
      ROUND(AVG(CASE WHEN btc_block_time >= ${MAINNET_TS} AND l2_gas_limit > 0
        THEN l2_gas_used * 100.0 / l2_gas_limit END), 2) AS avg_gas_util_pct,
      ${COMMITMENT_FEE_SUM} AS total_commitment_fees,
      ${PROOF_FEE_SUM} AS total_proof_fees,
      ${TOTAL_FEE_SUM} AS total_btc_fees,
      ROUND(AVG(CASE WHEN btc_block_time >= ${MAINNET_TS}
        THEN l2_tx_count END), 2)                        AS avg_txs_per_batch,
      MIN(btc_block_time)                                AS time_min,
      MAX(btc_block_time)                                AS time_max,
      ROUND(AVG(CASE WHEN btc_block_time >= ${MAINNET_TS}
        THEN ${TOTAL_FEE} END), 1)                       AS avg_fee_per_batch,
      MAX(l1_height) - MIN(l1_height)                    AS l1_blocks_spanned,
      ROUND(AVG(l2_end_block - l2_start_block + 1), 2)  AS avg_l2_blocks_per_batch,
      MAX(batch_index)                                   AS latest_batch
    FROM commitments
    WHERE (btc_block_time IS NULL OR btc_block_time >= ?)
  `).get(minTime);
  // Proof lag: how many L1 blocks since the last proof was posted
  const proofLag = db.prepare(`
    SELECT MAX(proof_l1_height) AS last_proof_block,
           (SELECT CAST(value AS INTEGER) FROM sync_state WHERE key = 'btc_core_height') AS btc_tip,
           COUNT(*) AS batches_waiting
    FROM commitments
    WHERE proof_l1_height IS NULL AND btc_block_time >= ?
  `).get(MAINNET_TS);

  const lastProofBlock = db.prepare(`SELECT MAX(proof_l1_height) AS v FROM commitments`).get();
  const proofGap = (proofLag && lastProofBlock && lastProofBlock.v && proofLag.btc_tip)
    ? proofLag.btc_tip - lastProofBlock.v
    : 0;

  row.proof_gap_blocks = proofGap;
  row.proof_batches_waiting = proofLag ? proofLag.batches_waiting : 0;
  row.last_proof_l1_height = lastProofBlock ? lastProofBlock.v : null;
  row.launch_batch_index = LAUNCH_BATCH_INDEX;
  row.pre_launch_batches = PRE_LAUNCH_BATCH_COUNT;

  res.json(row);
}));

// GET /api/batches?offset=0&limit=50&token=&prelaunch=0|1
app.get('/api/batches', cached(req => `batches:${req.query.offset||0}:${req.query.limit||50}:${isTokenPaid(req.query.token||'')}:${req.query.prelaunch||'0'}`, (req, res) => {
  const offset = Math.max(0, parseInt(req.query.offset, 10) || 0);
  let limit = Math.min(200, Math.max(1, parseInt(req.query.limit, 10) || 50));
  const token = req.query.token || '';
  const prelaunch = req.query.prelaunch === '1';
  const timeFloor = prelaunch ? 0 : MAINNET_TS;

  const fullTotal = db.prepare('SELECT COUNT(*) AS cnt FROM commitments WHERE (btc_block_time IS NULL OR btc_block_time >= ?)').get(timeFloor).cnt;
  const unlocked = isTokenPaid(token);

  // Cap rows when payment-gated and not paid
  const effectiveTotal = unlocked ? fullTotal : Math.min(fullTotal, FREE_BATCH_LIMIT);
  if (!unlocked && offset + limit > FREE_BATCH_LIMIT) {
    limit = Math.max(0, FREE_BATCH_LIMIT - offset);
  }

  const rows = db.prepare(`
    SELECT
      batch_index,
      l1_height,
      l2_start_block,
      l2_end_block,
      l2_tx_count,
      l2_gas_used,
      l2_gas_limit,
      CASE WHEN l2_gas_limit > 0
        THEN ROUND(l2_gas_used * 100.0 / l2_gas_limit, 4)
        ELSE NULL END AS gas_utilization_pct,
      btc_fee_sats,
      commit_fee_sats,
      proof_fee_sats,
      proof_commit_fee_sats,
      proof_txid,
      proof_l1_height,
      ${TOTAL_FEE} AS total_fee_sats,
      CASE WHEN l2_tx_count > 0
        THEN ROUND((${TOTAL_FEE}) * 1.0 / l2_tx_count, 2)
        ELSE NULL END AS fee_per_tx,
      btc_txid,
      btc_block_time,
      CASE WHEN btc_block_time < ${MAINNET_TS} THEN 1 ELSE 0 END AS is_pre_launch
    FROM commitments
    WHERE (btc_block_time IS NULL OR btc_block_time >= ?)
    ORDER BY batch_index DESC
    LIMIT ? OFFSET ?
  `).all(timeFloor, limit, offset);

  res.json({ total: effectiveTotal, fullTotal, unlocked, offset, limit, rows, launch_batch_index: LAUNCH_BATCH_INDEX });
}));

// GET /api/csv?token=
app.get('/api/csv', (req, res) => {
  const token = req.query.token || '';

  if (!isTokenPaid(token)) {
    return res.status(402).json({ error: 'Payment required for CSV download' });
  }

  const rows = db.prepare(`
    SELECT
      batch_index,
      l1_height,
      l2_start_block,
      l2_end_block,
      l2_tx_count,
      l2_gas_used,
      l2_gas_limit,
      CASE WHEN l2_gas_limit > 0
        THEN ROUND(l2_gas_used * 100.0 / l2_gas_limit, 4)
        ELSE NULL END AS gas_utilization_pct,
      btc_fee_sats,
      commit_fee_sats,
      proof_fee_sats,
      proof_commit_fee_sats,
      proof_txid,
      proof_l1_height,
      ${TOTAL_FEE} AS total_fee_sats,
      CASE WHEN l2_tx_count > 0
        THEN ROUND((${TOTAL_FEE}) * 1.0 / l2_tx_count, 2)
        ELSE NULL END AS fee_per_tx,
      btc_txid,
      btc_block_time
    FROM commitments
    WHERE (btc_block_time IS NULL OR btc_block_time >= ?)
    ORDER BY batch_index DESC
  `).all(MAINNET_TS);

  const header = 'batch_index,l1_height,l2_start_block,l2_end_block,l2_tx_count,l2_gas_used,l2_gas_limit,gas_utilization_pct,btc_fee_sats,commit_fee_sats,proof_fee_sats,proof_commit_fee_sats,total_fee_sats,fee_per_tx,btc_txid,proof_txid,proof_l1_height,btc_block_time';
  const csvLines = rows.map((r) =>
    [r.batch_index, r.l1_height, r.l2_start_block, r.l2_end_block, r.l2_tx_count,
     r.l2_gas_used, r.l2_gas_limit, r.gas_utilization_pct, r.btc_fee_sats,
     r.commit_fee_sats, r.proof_fee_sats, r.proof_commit_fee_sats, r.total_fee_sats,
     r.fee_per_tx, r.btc_txid, r.proof_txid, r.proof_l1_height, r.btc_block_time].join(',')
  );

  res.setHeader('Content-Type', 'text/csv');
  res.setHeader('Content-Disposition', 'attachment; filename="citrea_batches.csv"');
  res.send(header + '\n' + csvLines.join('\n') + '\n');
});

// GET /api/charts/fee-per-tx?period=all|7d|24h|4h&prelaunch=0|1
app.get('/api/charts/fee-per-tx', cached(req => `chart:fee-per-tx:${req.query.period || 'all'}:${req.query.prelaunch || '0'}`, (req, res) => {
  const period = req.query.period || 'all';
  const prelaunch = req.query.prelaunch === '1';
  const minTime = periodToMinTime(period, prelaunch);
  const where = 'l2_tx_count > 0 AND proof_fee_sats IS NOT NULL AND (btc_block_time IS NULL OR btc_block_time >= ?)';
  const { cnt } = db.prepare(`SELECT COUNT(*) AS cnt FROM commitments WHERE ${where}`).get(minTime);

  let rows;
  if (cnt <= MAX_CHART_POINTS) {
    rows = db.prepare(`
      SELECT batch_index AS x,
             ROUND((${TOTAL_FEE}) * 1.0 / l2_tx_count, 2) AS y
      FROM commitments
      WHERE ${where}
      ORDER BY batch_index
    `).all(minTime);
  } else {
    const bucketSize = Math.ceil(cnt / MAX_CHART_POINTS);
    rows = db.prepare(`
      WITH numbered AS (
        SELECT batch_index AS x,
               (${TOTAL_FEE}) * 1.0 / l2_tx_count AS y,
               (ROW_NUMBER() OVER (ORDER BY batch_index) - 1) / ${bucketSize} AS bucket
        FROM commitments
        WHERE ${where}
      )
      SELECT CAST((MIN(x) + MAX(x)) / 2 AS INTEGER) AS x,
             ROUND(AVG(y), 2) AS y
      FROM numbered
      GROUP BY bucket
      ORDER BY bucket
    `).all(minTime);
  }
  res.json({ data: rows, launch_batch_index: LAUNCH_BATCH_INDEX });
}));

// GET /api/charts/tx-count?period=all|7d|24h|4h&prelaunch=0|1
app.get('/api/charts/tx-count', cached(req => `chart:tx:${req.query.period || 'all'}:${req.query.prelaunch || '0'}`, (req, res) => {
  const period = req.query.period || 'all';
  const prelaunch = req.query.prelaunch === '1';
  const minTime = periodToMinTime(period, prelaunch);
  const where = 'l2_tx_count IS NOT NULL AND (btc_block_time IS NULL OR btc_block_time >= ?)';
  const { cnt } = db.prepare(`SELECT COUNT(*) AS cnt FROM commitments WHERE ${where}`).get(minTime);

  let rows;
  if (cnt <= MAX_CHART_POINTS) {
    rows = db.prepare(`
      SELECT batch_index AS x, l2_tx_count AS y
      FROM commitments
      WHERE ${where}
      ORDER BY batch_index
    `).all(minTime);
  } else {
    const bucketSize = Math.ceil(cnt / MAX_CHART_POINTS);
    rows = db.prepare(`
      WITH numbered AS (
        SELECT batch_index AS x, l2_tx_count AS y,
               (ROW_NUMBER() OVER (ORDER BY batch_index) - 1) / ${bucketSize} AS bucket
        FROM commitments
        WHERE ${where}
      )
      SELECT CAST((MIN(x) + MAX(x)) / 2 AS INTEGER) AS x,
             SUM(y) AS y
      FROM numbered
      GROUP BY bucket
      ORDER BY bucket
    `).all(minTime);
  }
  res.json({ data: rows, launch_batch_index: LAUNCH_BATCH_INDEX });
}));

// GET /api/charts/fees?period=all|7d|24h|4h&prelaunch=0|1
app.get('/api/charts/fees', cached(req => `chart:fees:${req.query.period || 'all'}:${req.query.prelaunch || '0'}`, (req, res) => {
  const period = req.query.period || 'all';
  const prelaunch = req.query.prelaunch === '1';
  const minTime = periodToMinTime(period, prelaunch);
  const where = 'btc_fee_sats IS NOT NULL AND (btc_block_time IS NULL OR btc_block_time >= ?)';
  const { cnt } = db.prepare(`SELECT COUNT(*) AS cnt FROM commitments WHERE ${where}`).get(minTime);

  let rows;
  if (cnt <= MAX_CHART_POINTS) {
    rows = db.prepare(`
      SELECT batch_index AS x,
             ${TOTAL_FEE} AS y,
             ${COMMITMENT_FEE} AS commitment_fee,
             ${PROOF_FEE} AS proof_fee
      FROM commitments
      WHERE ${where}
      ORDER BY batch_index
    `).all(minTime);
  } else {
    const bucketSize = Math.ceil(cnt / MAX_CHART_POINTS);
    rows = db.prepare(`
      WITH numbered AS (
        SELECT batch_index AS x,
               ${COMMITMENT_FEE} AS commitment_fee,
               ${PROOF_FEE} AS proof_fee,
               (ROW_NUMBER() OVER (ORDER BY batch_index) - 1) / ${bucketSize} AS bucket
        FROM commitments
        WHERE ${where}
      )
      SELECT CAST((MIN(x) + MAX(x)) / 2 AS INTEGER) AS x,
             SUM(commitment_fee) + SUM(proof_fee) AS y,
             SUM(commitment_fee) AS commitment_fee,
             SUM(proof_fee) AS proof_fee
      FROM numbered
      GROUP BY bucket
      ORDER BY bucket
    `).all(minTime);
  }
  res.json({ data: rows, launch_batch_index: LAUNCH_BATCH_INDEX });
}));

// GET /api/charts/utilization?period=all|7d|24h|4h&prelaunch=0|1
app.get('/api/charts/utilization', cached(req => `chart:util:${req.query.period || 'all'}:${req.query.prelaunch || '0'}`, (req, res) => {
  const period = req.query.period || 'all';
  const prelaunch = req.query.prelaunch === '1';
  const minTime = periodToMinTime(period, prelaunch);
  const where = 'l2_gas_limit > 0 AND (btc_block_time IS NULL OR btc_block_time >= ?)';
  const { cnt } = db.prepare(`SELECT COUNT(*) AS cnt FROM commitments WHERE ${where}`).get(minTime);

  let rows;
  if (cnt <= MAX_CHART_POINTS) {
    rows = db.prepare(`
      SELECT batch_index AS x,
             ROUND(l2_gas_used * 100.0 / l2_gas_limit, 4) AS y
      FROM commitments
      WHERE ${where}
      ORDER BY batch_index
    `).all(minTime);
  } else {
    const bucketSize = Math.ceil(cnt / MAX_CHART_POINTS);
    rows = db.prepare(`
      WITH numbered AS (
        SELECT batch_index AS x,
               l2_gas_used * 100.0 / l2_gas_limit AS y,
               (ROW_NUMBER() OVER (ORDER BY batch_index) - 1) / ${bucketSize} AS bucket
        FROM commitments
        WHERE ${where}
      )
      SELECT CAST((MIN(x) + MAX(x)) / 2 AS INTEGER) AS x,
             ROUND(AVG(y), 4) AS y
      FROM numbered
      GROUP BY bucket
      ORDER BY bucket
    `).all(minTime);
  }
  res.json({ data: rows, launch_batch_index: LAUNCH_BATCH_INDEX });
}));

// GET /api/events — detect noteworthy anomalies
app.get('/api/events', cached(() => 'events', (_req, res) => {
  const events = [];

  // 1. Low-activity streaks: runs of 3+ consecutive batches with < 10 txs
  const allRows = db.prepare(`
    SELECT batch_index, l2_tx_count, btc_block_time
    FROM commitments
    WHERE (btc_block_time IS NULL OR btc_block_time >= ${MAINNET_TS})
    ORDER BY batch_index
  `).all();

  let streak = [];
  for (const r of allRows) {
    if (r.l2_tx_count !== null && r.l2_tx_count < 10) {
      streak.push(r);
    } else {
      if (streak.length >= 3) {
        events.push({
          type: 'low_activity',
          severity: streak.length >= 10 ? 'high' : 'medium',
          title: `Low activity: ${streak.length} batches with < 10 txs`,
          detail: `Batches ${streak[0].batch_index}–${streak[streak.length - 1].batch_index} averaged ${(streak.reduce((s, r) => s + r.l2_tx_count, 0) / streak.length).toFixed(1)} txs/batch`,
          batch_start: streak[0].batch_index,
          batch_end: streak[streak.length - 1].batch_index,
          time_start: streak[0].btc_block_time,
          time_end: streak[streak.length - 1].btc_block_time
        });
      }
      streak = [];
    }
  }
  if (streak.length >= 3) {
    events.push({
      type: 'low_activity',
      severity: streak.length >= 10 ? 'high' : 'medium',
      title: `Low activity: ${streak.length} batches with < 10 txs`,
      detail: `Batches ${streak[0].batch_index}–${streak[streak.length - 1].batch_index} averaged ${(streak.reduce((s, r) => s + r.l2_tx_count, 0) / streak.length).toFixed(1)} txs/batch`,
      batch_start: streak[0].batch_index,
      batch_end: streak[streak.length - 1].batch_index,
      time_start: streak[0].btc_block_time,
      time_end: streak[streak.length - 1].btc_block_time
    });
  }

  // 2. Batch bunching: 4+ batches in the same L1 block
  const bunches = db.prepare(`
    SELECT l1_height, COUNT(*) AS cnt,
           MIN(batch_index) AS batch_start, MAX(batch_index) AS batch_end,
           SUM(l2_tx_count) AS total_txs, SUM(btc_fee_sats) AS total_fees,
           MIN(btc_block_time) AS time
    FROM commitments
    WHERE (btc_block_time IS NULL OR btc_block_time >= ${MAINNET_TS})
    GROUP BY l1_height
    HAVING cnt >= 4
    ORDER BY batch_start DESC
  `).all();
  for (const b of bunches) {
    events.push({
      type: 'batch_bunching',
      severity: b.cnt >= 5 ? 'high' : 'medium',
      title: `${b.cnt} batches in one L1 block`,
      detail: `Batches ${b.batch_start}–${b.batch_end} all landed in L1 block ${b.l1_height} (${b.total_txs} total txs, ${b.total_fees} sats fees)`,
      batch_start: b.batch_start,
      batch_end: b.batch_end,
      time_start: b.time,
      time_end: b.time
    });
  }

  // 3. Commitment posting delays — disabled for now, needs more tuning
  // to properly distinguish slow Bitcoin blocks from slow Citrea posting.

  // 4. Fee spikes: dynamically threshold at > 5x the median fee
  const medianRow = db.prepare(`
    WITH ordered AS (
      SELECT ${TOTAL_FEE} AS total_fee,
             ROW_NUMBER() OVER (ORDER BY ${TOTAL_FEE}) AS rn,
             COUNT(*) OVER () AS total
      FROM commitments
      WHERE btc_fee_sats IS NOT NULL AND (btc_block_time IS NULL OR btc_block_time >= ${MAINNET_TS})
    )
    SELECT total_fee AS median FROM ordered WHERE rn = (total + 1) / 2
  `).get();
  const medianFee = medianRow ? medianRow.median : 0;
  const feeThreshold = medianFee * 5;

  if (feeThreshold > 0) {
    const feeSpikes = db.prepare(`
      SELECT batch_index,
             ${TOTAL_FEE} AS total_fee,
             l2_tx_count, btc_block_time,
             CASE WHEN l2_tx_count > 0
               THEN ROUND((${TOTAL_FEE}) * 1.0 / l2_tx_count, 1)
               ELSE NULL END AS fee_per_tx
      FROM commitments
      WHERE (${TOTAL_FEE}) > ?
        AND (btc_block_time IS NULL OR btc_block_time >= ${MAINNET_TS})
      ORDER BY batch_index DESC
    `).all(feeThreshold);
    for (const f of feeSpikes) {
      events.push({
        type: 'fee_spike',
        severity: f.total_fee > medianFee * 10 ? 'high' : 'medium',
        title: `Fee spike: ${f.total_fee} sats on batch ${f.batch_index}`,
        detail: `${f.l2_tx_count} txs, ${f.fee_per_tx != null ? f.fee_per_tx + ' sats/tx' : 'no txs'} (median is ${medianFee} sats)`,
        batch_start: f.batch_index,
        batch_end: f.batch_index,
        time_start: f.btc_block_time,
        time_end: f.btc_block_time
      });
    }
  }

  // 5. RPC lag: Citrea RPC L1 height behind Bitcoin Core height
  try {
    const btcRow = db.prepare(`SELECT value FROM sync_state WHERE key = 'btc_core_height'`).get();
    const citreaRow = db.prepare(`SELECT value FROM sync_state WHERE key = 'citrea_rpc_l1_height'`).get();
    if (btcRow && citreaRow) {
      const btcHeight = parseInt(btcRow.value, 10);
      const citreaHeight = parseInt(citreaRow.value, 10);
      const lag = btcHeight - citreaHeight;
      if (lag > 0) {
        events.push({
          type: 'rpc_lag',
          severity: lag > 5 ? 'high' : 'medium',
          title: `Citrea RPC ${lag} blocks behind Bitcoin Core`,
          detail: `Bitcoin Core at block ${btcHeight}, Citrea RPC synced to ${citreaHeight}`,
          batch_start: null,
          batch_end: null,
          time_start: Math.floor(Date.now() / 1000),
          time_end: Math.floor(Date.now() / 1000)
        });
      }
    }
  } catch (e) {
    // sync_state may not have these keys yet
  }

  // Sort all events by most recent first
  events.sort((a, b) => (b.time_end || 0) - (a.time_end || 0));

  res.json(events);
}));

// --- Startup ---

(async () => {
  await initSpark();

  app.listen(PORT, '0.0.0.0', () => {
    console.log(`Dashboard running on http://0.0.0.0:${PORT}`);
  });
})();

// --- Graceful shutdown ---

async function cleanup() {
  console.log('Shutting down...');
  if (sparkWallet) {
    try {
      await sparkWallet.cleanup();
      console.log('Spark wallet cleaned up');
    } catch (err) {
      console.error('Error cleaning up Spark wallet:', err.message);
    }
  }
  if (db) {
    db.close();
  }
  process.exit(0);
}

process.on('SIGTERM', cleanup);
process.on('SIGINT', cleanup);
