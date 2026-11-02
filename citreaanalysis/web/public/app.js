(function () {
  var C = window.CitreaCharts;
  var fmt = C.fmt;
  var fmtSats = C.fmtSats;

  const REFRESH_MS = 60_000;

  // ApexCharts instances keyed by element ID
  const charts = {};

  // --- Payment state ---
  let paymentEnabled = false;
  let paymentAmountSats = 100;
  let paymentFreeLimit = 100;
  let currentPollInterval = null;
  let currentPaymentFeature = null;

  // --- Pre-launch state ---
  const PRELAUNCH_KEY = 'citrea_prelaunch';
  let showPrelaunch = true;
  let launchBatchIndex = null;

  try {
    const stored = localStorage.getItem(PRELAUNCH_KEY);
    if (stored !== null) showPrelaunch = stored === '1';
  } catch { /* ignore */ }

  const TOKEN_KEY = 'citrea_token';

  function getStoredToken() {
    try { return localStorage.getItem(TOKEN_KEY) || ''; }
    catch { return ''; }
  }

  function setStoredToken(token) {
    try { localStorage.setItem(TOKEN_KEY, token); }
    catch { /* ignore */ }
  }

  // --- Payment config ---

  async function initPaymentConfig() {
    try {
      const cfg = await fetch('api/payment/config').then((r) => r.json());
      paymentEnabled = cfg.enabled;
      paymentAmountSats = cfg.amountSats;
      paymentFreeLimit = cfg.freeLimit;

      if (!paymentEnabled) return; // buttons stay hidden

      const btnUnlock = document.getElementById('btn-unlock');
      const btnCsv = document.getElementById('btn-csv');
      const token = getStoredToken();

      if (token) {
        const st = await fetch(`api/payment/status?token=${encodeURIComponent(token)}`).then((r) => r.json());
        if (st.status === 'paid') {
          // Already paid — hide unlock, show direct CSV download
          btnCsv.textContent = 'Download CSV';
          btnCsv.onclick = () => triggerCsvDownload(token);
          btnCsv.style.display = 'inline-block';
          return;
        }
        // Token expired/invalid — clear it
        setStoredToken('');
      }

      // Not paid — show both buttons
      btnUnlock.style.display = 'inline-block';
      btnCsv.style.display = 'inline-block';
    } catch (e) {
      console.error('Failed to load payment config', e);
    }
  }

  // --- Payment modal ---

  window.openPaymentModal = async function (triggerFeature) {
    currentPaymentFeature = triggerFeature;
    const modal = document.getElementById('payment-modal');
    const title = document.getElementById('modal-title');
    const desc = document.getElementById('modal-desc');
    const bolt11El = document.getElementById('modal-bolt11');
    const statusEl = document.getElementById('modal-status');

    title.textContent = 'Unlock Full Access';
    desc.textContent = `Pay ${paymentAmountSats} sats via Lightning to unlock full batch history and CSV export.`;
    bolt11El.textContent = 'Generating invoice...';
    statusEl.innerHTML = '<span class="spinner"></span> Creating invoice...';

    modal.classList.add('active');

    try {
      const resp = await fetch('api/payment/create-invoice', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({})
      });
      if (!resp.ok) {
        statusEl.textContent = 'Failed to create invoice. Try again.';
        return;
      }
      const { token, bolt11 } = await resp.json();
      bolt11El.textContent = bolt11;
      const qrCanvas = document.getElementById('modal-qr');
      try {
        await QRCode.toCanvas(qrCanvas, bolt11.toUpperCase(), { width: 220, margin: 2 });
      } catch (qrErr) {
        console.error('QR generation failed', qrErr);
      }
      statusEl.innerHTML = '<span class="spinner"></span> Waiting for payment...';

      // Start polling
      if (currentPollInterval) clearInterval(currentPollInterval);
      currentPollInterval = setInterval(async () => {
        try {
          const st = await fetch(`api/payment/status?token=${encodeURIComponent(token)}`).then((r) => r.json());
          if (st.status === 'paid') {
            clearInterval(currentPollInterval);
            currentPollInterval = null;
            setStoredToken(token);
            statusEl.textContent = 'Payment received!';

            // Unlock everything
            document.getElementById('btn-unlock').style.display = 'none';
            const btnCsv = document.getElementById('btn-csv');
            btnCsv.textContent = 'Download CSV';
            btnCsv.onclick = () => triggerCsvDownload(token);
            loadTable();

            // If they clicked CSV, auto-start the download
            if (triggerFeature === 'csv_download') {
              triggerCsvDownload(token);
            }

            setTimeout(() => closePaymentModal(), 1500);
          } else if (st.status === 'expired') {
            clearInterval(currentPollInterval);
            currentPollInterval = null;
            statusEl.textContent = 'Invoice expired. Close and try again.';
          }
        } catch (err) {
          console.error('Poll error', err);
        }
      }, 3000);
    } catch (err) {
      statusEl.textContent = 'Error creating invoice. Try again.';
      console.error('Invoice error', err);
    }
  };

  window.closePaymentModal = function () {
    document.getElementById('payment-modal').classList.remove('active');
    if (currentPollInterval) {
      clearInterval(currentPollInterval);
      currentPollInterval = null;
    }
  };

  window.copyBolt11 = function () {
    const text = document.getElementById('modal-bolt11').textContent;
    if (!text || text === 'Generating invoice...') return;
    navigator.clipboard.writeText(text).then(() => {
      const btn = document.querySelector('.invoice-box .btn-copy');
      const orig = btn.textContent;
      btn.textContent = 'Copied!';
      setTimeout(() => { btn.textContent = orig; }, 1500);
    }).catch(() => {});
  };

  function triggerCsvDownload(token) {
    const a = document.createElement('a');
    a.href = `api/csv?token=${encodeURIComponent(token)}`;
    a.download = 'citrea_batches.csv';
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
  }

  function updateFreeBanner(d) {
    const banner = document.getElementById('free-banner');
    if (!banner) return;
    if (paymentEnabled && !d.unlocked && d.fullTotal > d.total) {
      banner.style.display = 'block';
      banner.textContent = `Showing ${d.total} of ${d.fullTotal} batches — unlock full history for ${paymentAmountSats} sats`;
    } else {
      banner.style.display = 'none';
    }
  }

  // --- Pre-launch toggle ---
  const prelaunchCheck = document.getElementById('prelaunch-check');
  if (prelaunchCheck) {
    prelaunchCheck.checked = showPrelaunch;
    prelaunchCheck.addEventListener('change', () => {
      showPrelaunch = prelaunchCheck.checked;
      try { localStorage.setItem(PRELAUNCH_KEY, showPrelaunch ? '1' : '0'); }
      catch { /* ignore */ }
      // Destroy all charts so they get re-created with/without annotations
      Object.keys(charts).forEach((id) => {
        if (charts[id]) { charts[id].destroy(); delete charts[id]; }
      });
      refreshAll();
    });
  }

  function prelaunchParam() {
    return showPrelaunch ? '1' : '0';
  }

  // --- Charts ---

  function chartOpts() {
    return { showPrelaunch: showPrelaunch, launchBatchIndex: launchBatchIndex };
  }

  function showChartLoading(elId) {
    const box = document.getElementById(elId)?.closest('.chart-box');
    if (!box || box.querySelector('.chart-loading')) return;
    const overlay = document.createElement('div');
    overlay.className = 'chart-loading';
    overlay.innerHTML = '<span class="spinner"></span>';
    box.appendChild(overlay);
  }

  function hideChartLoading(elId) {
    const box = document.getElementById(elId)?.closest('.chart-box');
    if (!box) return;
    const overlay = box.querySelector('.chart-loading');
    if (overlay) overlay.remove();
  }

  function nextFrame() {
    return new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  }

  function upsertChart(elId, label, data, color, type = 'area', extra = {}) {
    const xyData = data.map((d) => ({ x: d.x, y: d.y }));
    const logScale = chartLogScale[elId] || false;

    // Always destroy and recreate — annotations are init-only in ApexCharts
    if (charts[elId]) {
      charts[elId].destroy();
      delete charts[elId];
    }

    const opts = C.makeChartOpts(type, label, color, xyData, { logScale: logScale, ...chartOpts(), ...extra });
    const chart = new ApexCharts(document.getElementById(elId), opts);
    chart.render();
    charts[elId] = chart;
  }

  function upsertFeesChart(elId, data) {
    const logScale = chartLogScale[elId] || false;

    if (charts[elId]) {
      charts[elId].destroy();
      delete charts[elId];
    }

    const opts = C.makeFeesChartOpts(data, { logScale: logScale, ...chartOpts() });
    const chart = new ApexCharts(document.getElementById(elId), opts);
    chart.render();
    charts[elId] = chart;
  }

  // --- Period toggle (overview cards) ---
  let overviewPeriod = 'all';

  document.getElementById('period-toggle').addEventListener('click', (e) => {
    const btn = e.target.closest('.period-btn');
    if (!btn) return;
    const period = btn.dataset.period;
    if (period === overviewPeriod) return;
    overviewPeriod = period;
    document.querySelectorAll('#period-toggle .period-btn').forEach((b) => b.classList.remove('active'));
    btn.classList.add('active');
    refreshOverview();
  });

  // --- Per-chart log scale state ---
  const chartLogScale = {
    'chart-fee-per-tx': false,
    'chart-fees': false,
    'chart-tx': false,
    'chart-util': false
  };

  // --- Per-chart period state ---
  const chartPeriods = {
    'chart-fee-per-tx': '24h',
    'chart-fees': '24h',
    'chart-tx': '24h',
    'chart-util': '24h',
  };

  const chartEndpoints = {
    'chart-fee-per-tx': 'api/charts/fee-per-tx',
    'chart-fees': 'api/charts/fees',
    'chart-tx': 'api/charts/tx-count',
    'chart-util': 'api/charts/l2-throughput',
  };

  function updateProofGapWarning(proofGap) {
    const box = document.getElementById('chart-fee-per-tx')?.closest('.chart-box');
    if (!box) return;
    let banner = box.querySelector('.proof-gap-banner');
    if (proofGap > 10) {
      if (!banner) {
        banner = document.createElement('div');
        banner.className = 'proof-gap-banner';
        box.querySelector('.chart-header').after(banner);
      }
      banner.textContent = `${proofGap} recent batches excluded — proof data not yet available`;
    } else if (banner) {
      banner.remove();
    }
  }

  async function refreshSingleChart(chartId) {
    showChartLoading(chartId);
    try {
      const period = chartPeriods[chartId];
      const url = `${chartEndpoints[chartId]}?period=${period}&prelaunch=${prelaunchParam()}`;
      const resp = await fetch(url).then((r) => r.json());
      const data = resp.data;
      if (resp.launch_batch_index != null) launchBatchIndex = resp.launch_batch_index;
      await nextFrame();
      if (chartId === 'chart-fee-per-tx') {
        upsertChart('chart-fee-per-tx', 'Fee/Tx (sats)', data, '#ea580c');
        updateProofGapWarning(resp.proof_gap || 0);
      }
      else if (chartId === 'chart-tx') upsertChart('chart-tx', 'Tx Count', data, '#f97316', 'bar');
      else if (chartId === 'chart-fees') upsertFeesChart('chart-fees', data);
      else if (chartId === 'chart-util') upsertChart('chart-util', 'L2 Txs/BTC Block', data, '#3b82f6');
    } catch (e) {
      console.error('Failed to refresh chart', chartId, e);
    } finally {
      hideChartLoading(chartId);
    }
  }

  // Map chart element IDs to pop-out URL slugs
  const chartSlugMap = {
    'chart-fee-per-tx': 'fee-per-tx',
    'chart-fees': 'fees',
    'chart-tx': 'tx-count',
    'chart-util': 'l2-throughput',
  };

  // Delegated click handler for chart period toggles
  document.querySelector('.charts').addEventListener('click', (e) => {
    const btn = e.target.closest('.period-btn');
    if (!btn) return;
    const toggle = btn.closest('.period-toggle');
    if (!toggle || !toggle.dataset.chart) return;
    const chartId = toggle.dataset.chart;
    const period = btn.dataset.period;
    if (period === chartPeriods[chartId]) return;
    chartPeriods[chartId] = period;
    toggle.querySelectorAll('.period-btn').forEach((b) => b.classList.remove('active'));
    btn.classList.add('active');
    refreshSingleChart(chartId);

    // Update pop-out link to reflect selected period
    const slug = chartSlugMap[chartId];
    if (slug) {
      const popout = toggle.closest('.chart-header').querySelector('.chart-popout');
      if (popout) popout.href = `chart.html?chart=${slug}&period=${period}${showPrelaunch ? '&prelaunch=1' : ''}`;
    }
  });

  // --- Log scale toggle handler ---
  document.querySelector('.charts').addEventListener('click', (e) => {
    const btn = e.target.closest('.log-toggle');
    if (!btn) return;
    const chartId = btn.dataset.chart;
    if (!chartId || !(chartId in chartLogScale)) return;
    chartLogScale[chartId] = !chartLogScale[chartId];
    btn.classList.toggle('active', chartLogScale[chartId]);
    // Destroy and re-render to apply scale change
    if (charts[chartId]) {
      charts[chartId].destroy();
      delete charts[chartId];
    }
    refreshSingleChart(chartId);
  });

  // --- Refresh functions ---

  async function refreshOverview() {
    try {
      const ov = await fetch(`api/overview?period=${overviewPeriod}&prelaunch=${prelaunchParam()}`).then((r) => r.json());
      if (ov.launch_batch_index != null) launchBatchIndex = ov.launch_batch_index;
      const empty = !ov.total_batches;
      const pend = 'pending';
      const feePerTx = (!empty && ov.total_l2_txs > 0 && ov.total_btc_fees > 0)
        ? (ov.total_btc_fees / ov.total_l2_txs).toFixed(2)
        : null;
      document.getElementById('card-fee-per-tx').textContent =
        empty ? pend : (feePerTx != null ? feePerTx + ' sats' : '—');
      document.getElementById('card-txs').textContent = empty ? pend : fmt(ov.total_l2_txs);
      document.getElementById('card-util').textContent =
        empty ? pend : (ov.avg_l2_txs_per_btc_block != null ? ov.avg_l2_txs_per_btc_block : '—');
      document.getElementById('card-fees').textContent = empty ? pend : fmtSats(ov.total_btc_fees);
      document.getElementById('card-avg-txs').textContent =
        empty ? pend : (ov.avg_txs_per_batch != null ? ov.avg_txs_per_batch : '—');
      document.getElementById('card-avg-fee').textContent =
        empty ? pend : (ov.avg_fee_per_batch != null ? fmtSats(Math.round(ov.avg_fee_per_batch)) : '—');

      // Show/hide pre-launch indicator on total cards
      const totalCardIds = ['card-txs', 'card-fees'];
      totalCardIds.forEach((id) => {
        const el = document.getElementById(id);
        if (!el) return;
        let indicator = el.parentElement.querySelector('.prelaunch-indicator');
        if (showPrelaunch && ov.pre_launch_batches > 0 && overviewPeriod === 'all') {
          if (!indicator) {
            indicator = document.createElement('span');
            indicator.className = 'prelaunch-indicator';
            el.parentElement.appendChild(indicator);
          }
          indicator.textContent = '(incl. pre-launch)';
        } else if (indicator) {
          indicator.remove();
        }
      });

      if (!empty && ov.time_min && ov.time_max) {
        const d = (t) => new Date(t * 1000).toISOString().slice(0, 10);
        document.getElementById('time-range').textContent =
          `${d(ov.time_min)}  to  ${d(ov.time_max)}`;
      }

      // Proof delay banner — historical max gap is ~12 blocks, flag above that
      const proofBanner = document.getElementById('proof-delay-banner');
      if (proofBanner && ov.proof_gap_blocks > 12) {
        proofBanner.style.display = 'block';
        proofBanner.textContent = `Proof delay: no new proofs in ${ov.proof_gap_blocks} L1 blocks (${ov.proof_batches_waiting} batches waiting). Last proof at L1 block ${fmt(ov.last_proof_l1_height)}.`;
      } else if (proofBanner) {
        proofBanner.style.display = 'none';
      }
    } catch (e) {
      console.error('Failed to load overview', e);
    }
  }

  const typeLabels = {
    low_activity: 'Low Activity',
    fee_spike: 'Fee Spike',
    proof_delay: 'Proof Delay',
    rpc_lag: 'RPC Lag'
  };

  async function refreshEvents() {
    try {
      const events = await fetch('api/events').then((r) => r.json());
      const el = document.getElementById('events-list');
      el.textContent = '';
      if (events.length === 0) {
        const p = document.createElement('p');
        p.className = 'events-loading';
        p.textContent = 'No anomalies detected.';
        el.appendChild(p);
      } else {
        const fmtTime = (t) => t ? new Date(t * 1000).toISOString().replace('T', ' ').slice(0, 16) + ' UTC' : '';
        const ALLOWED_SEVERITIES = { low: 1, medium: 1, high: 1 };
        events.forEach((e) => {
          const timeStr = e.time_start === e.time_end
            ? fmtTime(e.time_start)
            : `${fmtTime(e.time_start)} \u2014 ${fmtTime(e.time_end)}`;
          const card = document.createElement('div');
          card.className = 'event-card';
          if (ALLOWED_SEVERITIES[e.severity]) card.classList.add('severity-' + e.severity);
          const badge = document.createElement('span');
          badge.className = 'event-badge';
          if (typeLabels[e.type]) badge.classList.add(e.type);
          badge.textContent = typeLabels[e.type] || e.type;
          const body = document.createElement('div');
          body.className = 'event-body';
          const title = document.createElement('div');
          title.className = 'event-title';
          title.textContent = e.title;
          const detail = document.createElement('div');
          detail.className = 'event-detail';
          detail.textContent = e.detail;
          const time = document.createElement('div');
          time.className = 'event-time';
          time.textContent = timeStr;
          body.append(title, detail, time);
          card.append(badge, body);
          el.appendChild(card);
        });
      }
    } catch (e) {
      console.error('Failed to load events', e);
    }
  }

  async function refreshCharts() {
    const chartIds = ['chart-fee-per-tx', 'chart-tx', 'chart-fees', 'chart-util'];
    chartIds.forEach(showChartLoading);
    try {
      const pl = prelaunchParam();
      const [feePerTxResp, txResp, feesResp, utilResp] = await Promise.all([
        fetch(`api/charts/fee-per-tx?period=${chartPeriods['chart-fee-per-tx']}&prelaunch=${pl}`).then((r) => r.json()),
        fetch(`api/charts/tx-count?period=${chartPeriods['chart-tx']}&prelaunch=${pl}`).then((r) => r.json()),
        fetch(`api/charts/fees?period=${chartPeriods['chart-fees']}&prelaunch=${pl}`).then((r) => r.json()),
        fetch(`api/charts/l2-throughput?period=${chartPeriods['chart-util']}&prelaunch=${pl}`).then((r) => r.json())
      ]);
      // Update launch batch index from any response
      if (feePerTxResp.launch_batch_index != null) launchBatchIndex = feePerTxResp.launch_batch_index;

      // Stagger renders to avoid blocking the main thread
      await nextFrame();
      upsertChart('chart-fee-per-tx', 'Fee/Tx (sats)', feePerTxResp.data, '#ea580c');
      updateProofGapWarning(feePerTxResp.proof_gap || 0);
      hideChartLoading('chart-fee-per-tx');
      await nextFrame();
      upsertChart('chart-tx', 'Tx Count', txResp.data, '#f97316', 'bar');
      hideChartLoading('chart-tx');
      await nextFrame();
      upsertFeesChart('chart-fees', feesResp.data);
      hideChartLoading('chart-fees');
      await nextFrame();
      upsertChart('chart-util', 'L2 Txs/BTC Block', utilResp.data, '#3b82f6');
      hideChartLoading('chart-util');
    } catch (e) {
      console.error('Failed to load charts', e);
      chartIds.forEach(hideChartLoading);
    }
  }

  // Paginated table
  const PAGE = 25;
  let offset = 0;
  const tbody = document.querySelector('#batch-table tbody');
  const info = document.getElementById('table-info');
  const btnPrev = document.getElementById('btn-prev');
  const btnNext = document.getElementById('btn-next');

  async function loadTable() {
    try {
      const token = getStoredToken();
      const tokenParam = token ? `&token=${encodeURIComponent(token)}` : '';
      const d = await fetch(`api/batches?offset=${offset}&limit=${PAGE}${tokenParam}&prelaunch=${prelaunchParam()}`)
        .then((r) => r.json());
      tbody.innerHTML = '';
      for (const r of d.rows) {
        const isPreLaunch = r.is_pre_launch === 1;
        const time = r.btc_block_time
          ? new Date(r.btc_block_time * 1000).toLocaleString(undefined, { year:'numeric', month:'2-digit', day:'2-digit', hour:'2-digit', minute:'2-digit', second:'2-digit', hour12: false })
          : '—';
        const preBadge = isPreLaunch ? '<span class="pre-launch-badge">(pre-launch)</span>' : '';
        const tr = document.createElement('tr');
        if (isPreLaunch) tr.className = 'pre-launch-row';
        tr.innerHTML =
          `<td>${r.batch_index}</td>` +
          `<td>${fmt(r.l1_height)}</td>` +
          `<td>${fmt(r.l2_start_block)}–${fmt(r.l2_end_block)}</td>` +
          `<td>${fmt(r.l2_tx_count)}</td>` +
          `<td>${fmt(r.l2_gas_used)}</td>` +
          `<td>${fmt(r.l2_gas_limit)}</td>` +
          `<td>${r.gas_utilization_pct != null ? r.gas_utilization_pct + '%' : '—'}</td>` +
          `<td>${r.btc_fee_sats != null ? fmt((r.btc_fee_sats || 0) + (r.commit_fee_sats || 0)) : '—'}</td>` +
          `<td>${r.proof_fee_sats != null ? fmt((r.proof_fee_sats || 0) + (r.proof_commit_fee_sats || 0)) : '—'}</td>` +
          `<td>${fmt(r.proof_l1_height)}</td>` +
          `<td>${(r.proof_l1_height != null && r.l1_height != null) ? (r.proof_l1_height - r.l1_height) : '—'}</td>` +
          `<td>${fmt(r.total_fee_sats)}</td>` +
          `<td>${r.fee_per_tx != null ? fmt(r.fee_per_tx) : '—'}</td>` +
          `<td>${time}${preBadge}</td>`;
        tbody.appendChild(tr);
      }
      const from = d.total === 0 ? 0 : offset + 1;
      const to = Math.min(offset + PAGE, d.total);
      info.textContent = `${from}–${to} of ${d.total}`;
      btnPrev.disabled = offset === 0;
      btnNext.disabled = offset + PAGE >= d.total;

      updateFreeBanner(d);
    } catch (e) {
      console.error('Failed to load table', e);
    }
  }

  btnPrev.addEventListener('click', () => { offset = Math.max(0, offset - PAGE); loadTable(); });
  btnNext.addEventListener('click', () => { offset += PAGE; loadTable(); });

  // --- Transactions split panes ---
  const TXS_PAGE = 10;
  const TX_BASE = 'tx.html?txid=';

  function makeTxPane(prefix, typeFilter) {
    const tbody = document.querySelector(`#${prefix}-table tbody`);
    const info = document.getElementById(`${prefix}-table-info`);
    const btnPrev = document.getElementById(`${prefix}-btn-prev`);
    const btnNext = document.getElementById(`${prefix}-btn-next`);
    let offset = 0;

    async function load() {
      try {
        const d = await fetch(`api/transactions?offset=${offset}&limit=${TXS_PAGE}&prelaunch=${prelaunchParam()}&type=${typeFilter}`)
          .then(r => r.json());
        tbody.innerHTML = '';
        for (const r of d.rows) {
          const shortTxid = r.txid.slice(0, 8) + '\u2026' + r.txid.slice(-8);
          const tr = document.createElement('tr');
          tr.innerHTML =
            `<td>${r.tx_type}</td>` +
            `<td>${r.batch_index}</td>` +
            `<td><a href="${TX_BASE}${r.txid}" target="_blank" rel="noopener" class="tx-link">${shortTxid}</a></td>`;
          tbody.appendChild(tr);
        }
        const from = d.total === 0 ? 0 : offset + 1;
        const to = Math.min(offset + TXS_PAGE, d.total);
        info.textContent = `${from}\u2013${to} of ${d.total}`;
        btnPrev.disabled = offset === 0;
        btnNext.disabled = offset + TXS_PAGE >= d.total;
      } catch (e) {
        console.error(`Failed to load ${prefix} transactions`, e);
      }
    }

    btnPrev.addEventListener('click', () => { offset = Math.max(0, offset - TXS_PAGE); load(); });
    btnNext.addEventListener('click', () => { offset += TXS_PAGE; load(); });
    return { load };
  }

  const commitPane = makeTxPane('commit', 'commit');
  const proofPane = makeTxPane('proof', 'proof');

  // --- Initial load + live refresh ---

  async function refreshAll() {
    await Promise.all([
      refreshOverview(),
      refreshEvents(),
      refreshCharts(),
      loadTable(),
      commitPane.load(),
      proofPane.load()
    ]);
    const now = new Date().toLocaleTimeString();
    document.getElementById('last-update').textContent = `Last update: ${now}`;
  }

  // Init payment config once at startup (not on refresh interval)
  initPaymentConfig();

  refreshAll();
  setInterval(refreshAll, REFRESH_MS);
})();
