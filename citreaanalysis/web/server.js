const express = require('express');
const Database = require('better-sqlite3');
const path = require('path');
const crypto = require('crypto');

const app = express();
const PORT = parseInt(process.env.PORT, 10) || 3000;
const DB_PATH = process.env.DB_PATH || '/data/citrea.db';

let db;
try {
  db = new Database(DB_PATH, { fileMustExist: true });
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
const COMMITMENT_FEE     = 'COALESCE(btc_fee_sats,0) + COALESCE(commit_fee_sats,0)';
const PROOF_FEE          = 'COALESCE(proof_fee_sats,0) + COALESCE(proof_commit_fee_sats,0)';
const TOTAL_FEE          = `${COMMITMENT_FEE} + ${PROOF_FEE}`;
const COMMITMENT_FEE_SUM = 'COALESCE(SUM(btc_fee_sats),0) + COALESCE(SUM(commit_fee_sats),0)';
const PROOF_FEE_SUM      = 'COALESCE(SUM(proof_fee_sats),0) + COALESCE(SUM(proof_commit_fee_sats),0)';
const TOTAL_FEE_SUM      = `${COMMITMENT_FEE_SUM} + ${PROOF_FEE_SUM}`;

const FIND_TX_BATCH_SQL = `
  SELECT batch_index,
    CASE
      WHEN commit_txid = ? THEN 'Commitment Inscribe'
      WHEN btc_txid = ? THEN 'Commitment Reveal'
      WHEN proof_commit_txid = ? THEN 'Proof Inscribe'
      WHEN proof_txid = ? THEN 'Proof Reveal'
    END AS tx_type
  FROM commitments
  WHERE commit_txid = ? OR btc_txid = ? OR proof_commit_txid = ? OR proof_txid = ?
  LIMIT 1
`;

function findTxBatch(txid) {
  return db.prepare(FIND_TX_BATCH_SQL).get(
    txid, txid, txid, txid,
    txid, txid, txid, txid
  );
}

function periodToMinTime(period, includePrelaunch) {
  const now = Math.floor(Date.now() / 1000);
  if (period === '4h') return Math.max(now - 14400, MAINNET_TS);
  if (period === '24h') return Math.max(now - 86400, MAINNET_TS);
  if (period === '7d') return Math.max(now - 7 * 86400, MAINNET_TS);
  // period === 'all'
  return (includePrelaunch ? 0 : MAINNET_TS);
}

// --- In-memory response cache (60s TTL, max 200 entries) ---
const CACHE_TTL_MS = 60_000;
const CACHE_MAX_SIZE = 200;
const responseCache = new Map();

// Sweep expired entries every 60s
setInterval(() => {
  const now = Date.now();
  for (const [key, entry] of responseCache) {
    if (now - entry.ts >= CACHE_TTL_MS) responseCache.delete(key);
  }
}, CACHE_TTL_MS);

function cached(keyFn, handler, cacheTTL = 300) {
  return (req, res) => {
    const key = keyFn(req);
    const entry = responseCache.get(key);
    if (entry && Date.now() - entry.ts < CACHE_TTL_MS) {
      res.setHeader('X-Cache', 'HIT');
      res.setHeader('Cache-Control', `public, max-age=${cacheTTL}`);
      return res.json(entry.data);
    }
    const origJson = res.json.bind(res);
    res.json = (data) => {
      if (responseCache.size >= CACHE_MAX_SIZE) {
        // Evict oldest entry
        const oldest = responseCache.keys().next().value;
        responseCache.delete(oldest);
      }
      responseCache.set(key, { data, ts: Date.now() });
      res.setHeader('X-Cache', 'MISS');
      res.setHeader('Cache-Control', `public, max-age=${cacheTTL}`);
      return origJson(data);
    };
    handler(req, res);
  };
}

app.use(express.json());

// --- CORS ---
const CORS_ORIGIN = process.env.CORS_ORIGIN || '';
if (CORS_ORIGIN) {
  app.use((req, res, next) => {
    res.setHeader('Access-Control-Allow-Origin', CORS_ORIGIN);
    res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
    res.setHeader('Access-Control-Allow-Headers', 'Content-Type');
    if (req.method === 'OPTIONS') return res.sendStatus(204);
    next();
  });
}

// --- Rate limiting ---
function rateLimit(windowMs, max) {
  const hits = new Map();
  setInterval(() => hits.clear(), windowMs);
  return (req, res, next) => {
    const key = req.ip;
    const count = (hits.get(key) || 0) + 1;
    hits.set(key, count);
    if (count > max) {
      return res.status(429).json({ error: 'Too many requests' });
    }
    next();
  };
}

// 300 requests per minute for general API, 10 per minute for payment creation
app.use('/api/', rateLimit(60_000, 300));
app.use('/api/payment/create-invoice', rateLimit(60_000, 10));

app.get('/llm.txt', (_req, res) => res.redirect(301, '/llms.txt'));
app.use(express.static(path.join(__dirname, 'public'), {
  maxAge: '1d',
  immutable: false,
  setHeaders(res, filePath) {
    if (filePath.endsWith('.html')) {
      // HTML files: cache but revalidate
      res.setHeader('Cache-Control', 'public, max-age=300, must-revalidate');
    } else if (filePath.endsWith('.js') || filePath.endsWith('.css')) {
      // JS/CSS: cache for 1 hour
      res.setHeader('Cache-Control', 'public, max-age=3600');
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
      ROUND(1.0 * SUM(l2_tx_count) / NULLIF(MAX(l1_height) - MIN(l1_height), 0), 1) AS avg_l2_txs_per_btc_block,
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
  // Count recent batches with txs but no proof data yet
  const { gap } = db.prepare(`
    SELECT COUNT(*) AS gap FROM commitments
    WHERE l2_tx_count > 0 AND proof_fee_sats IS NULL
      AND (btc_block_time IS NULL OR btc_block_time >= ?)
  `).get(minTime);

  res.json({ data: rows, launch_batch_index: LAUNCH_BATCH_INDEX, proof_gap: gap });
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

// GET /api/charts/l2-throughput?period=all|7d|24h|4h&prelaunch=0|1
app.get('/api/charts/l2-throughput', cached(req => `chart:l2tp:${req.query.period || 'all'}:${req.query.prelaunch || '0'}`, (req, res) => {
  const period = req.query.period || 'all';
  const prelaunch = req.query.prelaunch === '1';
  const minTime = periodToMinTime(period, prelaunch);
  const where = 'l1_height IS NOT NULL AND l2_tx_count IS NOT NULL AND (btc_block_time IS NULL OR btc_block_time >= ?)';
  const { cnt } = db.prepare(`SELECT COUNT(*) AS cnt FROM commitments WHERE ${where}`).get(minTime);

  let rows;
  if (cnt <= MAX_CHART_POINTS) {
    // For each batch, compute l2_tx_count / (l1_blocks since previous batch), clamped to min 1
    rows = db.prepare(`
      WITH lagged AS (
        SELECT batch_index,
               l2_tx_count,
               l1_height,
               LAG(l1_height) OVER (ORDER BY batch_index) AS prev_l1
        FROM commitments
        WHERE ${where}
      )
      SELECT batch_index AS x,
             ROUND(1.0 * l2_tx_count / MAX(l1_height - prev_l1, 1), 1) AS y
      FROM lagged
      WHERE prev_l1 IS NOT NULL
      ORDER BY batch_index
    `).all(minTime);
  } else {
    const bucketSize = Math.ceil(cnt / MAX_CHART_POINTS);
    rows = db.prepare(`
      WITH lagged AS (
        SELECT batch_index,
               l2_tx_count,
               l1_height,
               LAG(l1_height) OVER (ORDER BY batch_index) AS prev_l1
        FROM commitments
        WHERE ${where}
      ),
      numbered AS (
        SELECT batch_index AS x,
               1.0 * l2_tx_count / MAX(l1_height - prev_l1, 1) AS y,
               (ROW_NUMBER() OVER (ORDER BY batch_index) - 1) / ${bucketSize} AS bucket
        FROM lagged
        WHERE prev_l1 IS NOT NULL
      )
      SELECT CAST((MIN(x) + MAX(x)) / 2 AS INTEGER) AS x,
             ROUND(AVG(y), 1) AS y
      FROM numbered
      GROUP BY bucket
      ORDER BY bucket
    `).all(minTime);
  }
  res.json({ data: rows, launch_batch_index: LAUNCH_BATCH_INDEX });
}));

// GET /api/tx/:txid — proxy mempool.space + enrich with batch info
app.get('/api/tx/:txid', cached(req => `tx:${req.params.txid}`, async (req, res) => {
  const txid = req.params.txid;
  if (!/^[a-f0-9]{64}$/i.test(txid)) {
    return res.status(400).json({ error: 'Invalid txid' });
  }
  try {
    const [txResp, hexResp] = await Promise.all([
      fetch(`https://mempool.space/api/tx/${txid}`),
      fetch(`https://mempool.space/api/tx/${txid}/hex`)
    ]);
    if (!txResp.ok) return res.status(txResp.status).json({ error: 'Transaction not found' });
    const tx = await txResp.json();
    tx.hex = await hexResp.text();

    // Enrich with Citrea batch info
    const batchInfo = findTxBatch(txid);
    if (batchInfo) {
      tx.citrea = { batch_index: batchInfo.batch_index, tx_type: batchInfo.tx_type };

      // Build UTXO chain context: what comes before and after this tx
      const bi = batchInfo.batch_index;
      const tt = batchInfo.tx_type;
      const chain = {};

      // Input origin: what feeds into this tx
      for (const vin of tx.vin) {
        const origin = findTxBatch(vin.txid);
        if (origin) {
          if (!chain.from) chain.from = [];
          chain.from.push({ txid: vin.txid, batch_index: origin.batch_index, tx_type: origin.tx_type });
        }
      }

      // Output destination: what spends this tx's outputs
      // Reveal txs feed into the next batch's commit; commit txs feed into same batch's reveal
      let nextTxid = null, nextType = null, nextBatch = null;
      if (tt === 'Commitment Reveal') {
        const row = db.prepare('SELECT commit_txid FROM commitments WHERE batch_index = ?').get(bi + 1);
        if (row && row.commit_txid) { nextTxid = row.commit_txid; nextType = 'Commitment Inscribe'; nextBatch = bi + 1; }
      } else if (tt === 'Proof Reveal') {
        const row = db.prepare('SELECT proof_commit_txid FROM commitments WHERE batch_index = ?').get(bi + 1);
        if (row && row.proof_commit_txid) { nextTxid = row.proof_commit_txid; nextType = 'Proof Inscribe'; nextBatch = bi + 1; }
      } else if (tt === 'Commitment Inscribe') {
        const row = db.prepare('SELECT btc_txid FROM commitments WHERE batch_index = ?').get(bi);
        if (row && row.btc_txid) { nextTxid = row.btc_txid; nextType = 'Commitment Reveal'; nextBatch = bi; }
      } else if (tt === 'Proof Inscribe') {
        const row = db.prepare('SELECT proof_txid FROM commitments WHERE batch_index = ?').get(bi);
        if (row && row.proof_txid) { nextTxid = row.proof_txid; nextType = 'Proof Reveal'; nextBatch = bi; }
      }
      if (nextTxid) {
        chain.to = { txid: nextTxid, batch_index: nextBatch, tx_type: nextType };
      }
      tx.citrea.chain = chain;
    }
    res.json(tx);
  } catch (err) {
    console.error('Failed to fetch transaction:', err.message);
    res.status(502).json({ error: 'Failed to fetch transaction data' });
  }
}));

// GET /api/transactions?offset=0&limit=50&prelaunch=0|1&type=commit|proof
app.get('/api/transactions', cached(req => `txs:${req.query.offset||0}:${req.query.limit||50}:${req.query.prelaunch||'0'}:${req.query.type||'all'}`, (req, res) => {
  const offset = Math.max(0, parseInt(req.query.offset, 10) || 0);
  const limit = Math.min(200, Math.max(1, parseInt(req.query.limit, 10) || 50));
  const prelaunch = req.query.prelaunch === '1';
  const timeFloor = prelaunch ? 0 : MAINNET_TS;
  const typeFilter = req.query.type || 'all';

  const commitQueries = [
    { label: 'Commitment Inscribe', txCol: 'commit_txid', timeCol: 'commit_block_time' },
    { label: 'Commitment Reveal', txCol: 'btc_txid', timeCol: 'btc_block_time' },
  ];
  const proofQueries = [
    { label: 'Proof Inscribe', txCol: 'proof_commit_txid', timeCol: 'proof_commit_block_time' },
    { label: 'Proof Reveal', txCol: 'proof_txid', timeCol: 'proof_block_time' },
  ];
  let queries;
  if (typeFilter === 'commit') queries = commitQueries;
  else if (typeFilter === 'proof') queries = proofQueries;
  else queries = [...commitQueries, ...proofQueries];

  const countParts = queries.map(q =>
    `(SELECT COUNT(*) FROM commitments WHERE ${q.txCol} IS NOT NULL AND (btc_block_time IS NULL OR btc_block_time >= ?))`
  ).join(' + ');
  const countParams = queries.map(() => timeFloor);
  const total = db.prepare(`SELECT ${countParts} AS cnt`).get(...countParams).cnt;

  const unionParts = queries.map(q =>
    `SELECT batch_index, '${q.label}' AS tx_type, ${q.txCol} AS txid, ${q.timeCol} AS block_time FROM commitments WHERE ${q.txCol} IS NOT NULL AND (btc_block_time IS NULL OR btc_block_time >= ?)`
  ).join(' UNION ALL ');
  const rowParams = [...queries.map(() => timeFloor), limit, offset];
  const rows = db.prepare(`${unionParts} ORDER BY block_time DESC LIMIT ? OFFSET ?`).all(...rowParams);

  res.json({ total, offset, limit, rows });
}));

// GET /api/events — read pre-computed events from the daemon's events table
app.get('/api/events', cached(() => 'events', (_req, res) => {
  const rows = db.prepare(`
    SELECT event_type AS type, severity, title, detail,
           batch_start, batch_end, time_start, time_end
    FROM events WHERE resolved = 0 ORDER BY time_end DESC
  `).all();
  res.json(rows);
}));

// --- Internal Sync API (for satellite servers) ---

const SYNC_AUTH_TOKEN = process.env.SYNC_AUTH_TOKEN || '';
const IS_PRIMARY = process.env.IS_PRIMARY === 'true';

// Middleware to authenticate internal requests
function authenticateInternal(req, res, next) {
  if (!SYNC_AUTH_TOKEN) {
    return res.status(500).json({ error: 'SYNC_AUTH_TOKEN not configured' });
  }

  const authHeader = req.headers.authorization;
  if (!authHeader || !authHeader.startsWith('Bearer ')) {
    return res.status(401).json({ error: 'Missing or invalid Authorization header' });
  }

  const token = authHeader.substring(7);
  if (token !== SYNC_AUTH_TOKEN) {
    return res.status(403).json({ error: 'Invalid token' });
  }

  next();
}

// Compute version hash from database state
function computeDataVersion() {
  try {
    // Use max batch_index + count as version indicator
    const { maxBatch, count } = db.prepare(`
      SELECT
        COALESCE(MAX(batch_index), 0) as maxBatch,
        COUNT(*) as count
      FROM commitments
    `).get();

    // Create hash from max batch and count
    const versionString = `${maxBatch}-${count}`;
    return crypto.createHash('md5').update(versionString).digest('hex').substring(0, 8);
  } catch (err) {
    console.error('Error computing version:', err);
    return crypto.randomBytes(4).toString('hex');
  }
}

// Build complete dataset for sync
function buildFullDataset() {
  try {
    const commitments = db.prepare('SELECT * FROM commitments ORDER BY batch_index').all();
    const sequencer_txs = db.prepare('SELECT * FROM sequencer_txs ORDER BY btc_block_height').all();
    const stats_snapshots = db.prepare('SELECT * FROM stats_snapshots ORDER BY timestamp DESC LIMIT 1000').all();
    const events = db.prepare('SELECT * FROM events ORDER BY created_at DESC').all();
    const sync_state = db.prepare('SELECT * FROM sync_state').all();

    return {
      commitments,
      sequencer_txs,
      stats_snapshots,
      events,
      sync_state: sync_state.reduce((obj, row) => {
        obj[row.key] = row.value;
        return obj;
      }, {})
    };
  } catch (err) {
    console.error('Error building dataset:', err);
    throw err;
  }
}

// Internal sync endpoint for satellites
app.get('/internal/sync', authenticateInternal, (req, res) => {
  try {
    const currentVersion = computeDataVersion();

    // Check If-None-Match for conditional request
    const clientVersion = req.headers['if-none-match'];
    if (clientVersion === currentVersion) {
      res.set('ETag', currentVersion);
      return res.status(304).end();
    }

    // Build and return full dataset
    const data = buildFullDataset();
    const payload = {
      version: currentVersion,
      generated_at: new Date().toISOString(),
      data
    };

    res.set('ETag', currentVersion);
    res.set('Cache-Control', 'no-store'); // Don't cache this endpoint
    res.json(payload);

    console.log(`[SYNC] Sent full dataset: version=${currentVersion}, ` +
      `commitments=${data.commitments.length}, txs=${data.sequencer_txs.length}`);
  } catch (err) {
    console.error('[SYNC] Error:', err);
    res.status(500).json({ error: 'Failed to build sync payload' });
  }
});

// Health check endpoint
app.get('/health', (req, res) => {
  try {
    const { count } = db.prepare('SELECT COUNT(*) as count FROM commitments').get();
    res.json({
      status: 'ok',
      mode: IS_PRIMARY ? 'primary' : 'satellite',
      commitments: count,
      timestamp: new Date().toISOString()
    });
  } catch (err) {
    res.status(503).json({ status: 'error', error: err.message });
  }
});

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
