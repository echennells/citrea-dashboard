// Shared chart configuration — single source of truth for app.js and chart.html
window.CitreaCharts = (function () {
  var fmt = function (n) { return n == null ? '\u2014' : Number(n).toLocaleString(); };
  var fmtSats = function (n) {
    return n == null ? '\u2014' : (n >= 1e8
      ? (n / 1e8).toFixed(4) + ' BTC'
      : Number(n).toLocaleString() + ' sats');
  };

  function getThemeColors() {
    var isDark = document.documentElement.getAttribute('data-theme') === 'dark';
    return {
      grid: isDark ? '#252a38' : '#e5e7eb',
      label: isDark ? '#8892a4' : '#64748b',
      tooltip: isDark ? 'dark' : 'light',
      font: "'Outfit', sans-serif"
    };
  }

  function getLaunchAnnotation(showPrelaunch, launchBatchIndex) {
    if (!showPrelaunch || !launchBatchIndex) return {};
    return {
      annotations: {
        xaxis: [{
          x: launchBatchIndex,
          borderColor: '#10b981',
          strokeDashArray: 4,
          label: {
            text: 'Launch',
            orientation: 'horizontal',
            style: { color: '#fff', background: '#10b981', fontSize: '10px' }
          }
        }]
      }
    };
  }

  /**
   * Build ApexCharts options for a simple area or bar chart.
   * @param {string} type - 'area' or 'bar'
   * @param {string} label - y-axis / series label
   * @param {string} color - hex color
   * @param {Array} xyData - [{x, y}, ...]
   * @param {object} [opts]
   * @param {boolean} [opts.logScale]
   * @param {number}  [opts.height=260]
   * @param {boolean} [opts.showPrelaunch]
   * @param {number}  [opts.launchBatchIndex]
   */
  function makeChartOpts(type, label, color, xyData, opts) {
    opts = opts || {};
    var logScale = opts.logScale || false;
    var height = opts.height || 260;
    var annotation = getLaunchAnnotation(opts.showPrelaunch, opts.launchBatchIndex);
    var isBar = type === 'bar';
    var tc = getThemeColors();
    var config = {
      chart: {
        type: isBar ? 'bar' : 'area',
        height: height,
        fontFamily: tc.font,
        toolbar: { show: false },
        animations: { enabled: true, easing: 'easeinout', speed: 400 }
      },
      series: [{ name: label, data: xyData }],
      xaxis: {
        type: 'numeric',
        title: { text: 'Batch Index', style: { fontSize: '11px', color: tc.label } },
        labels: {
          style: { fontSize: '10px', colors: tc.label },
          formatter: function (v) { return v != null ? Math.round(v).toLocaleString() : ''; }
        },
        tickAmount: 8
      },
      yaxis: {
        logarithmic: !!logScale,
        min: opts.yMin != null ? opts.yMin : undefined,
        max: opts.yMax != null ? opts.yMax : undefined,
        title: { text: label, style: { fontSize: '11px', color: tc.label } },
        labels: {
          style: { fontSize: '10px', colors: tc.label },
          formatter: opts.yFormatter || function (v) { return v != null ? Number(v).toLocaleString() : ''; }
        }
      },
      colors: [color],
      fill: isBar
        ? { type: 'solid', opacity: 0.85 }
        : {
          type: 'gradient',
          gradient: { shadeIntensity: 1, opacityFrom: 0.45, opacityTo: 0.05, stops: [0, 100] }
        },
      stroke: isBar ? { width: 0 } : { curve: 'smooth', width: 2 },
      dataLabels: { enabled: false },
      grid: {
        borderColor: tc.grid,
        strokeDashArray: 3,
        xaxis: { lines: { show: false } }
      },
      tooltip: {
        theme: tc.tooltip,
        y: { formatter: function (v) { return fmt(v); } }
      },
      plotOptions: isBar ? { bar: { borderRadius: 3, columnWidth: '60%' } } : {}
    };
    if (annotation.annotations) config.annotations = annotation.annotations;
    return config;
  }

  /**
   * Build ApexCharts options for the stacked commitment/proof fees chart.
   * @param {Array} data - [{x, commitment_fee, proof_fee}, ...]
   * @param {object} [opts]
   * @param {boolean} [opts.logScale]
   * @param {number}  [opts.height=260]
   * @param {boolean} [opts.showPrelaunch]
   * @param {number}  [opts.launchBatchIndex]
   */
  function makeFeesChartOpts(data, opts) {
    opts = opts || {};
    var logScale = opts.logScale || false;
    var height = opts.height || 260;
    var annotation = getLaunchAnnotation(opts.showPrelaunch, opts.launchBatchIndex);
    var commitData = data.map(function (d) { return { x: d.x, y: d.commitment_fee || 0 }; });
    var proofData = data.map(function (d) { return { x: d.x, y: d.proof_fee || 0 }; });
    var tc = getThemeColors();
    var config = {
      chart: {
        type: 'bar',
        height: height,
        stacked: true,
        fontFamily: tc.font,
        toolbar: { show: false },
        animations: { enabled: false }
      },
      series: [
        { name: 'Proof Fee', data: proofData },
        { name: 'Commit Fee', data: commitData }
      ],
      colors: ['#f59e0b', '#3b82f6'],
      xaxis: {
        type: 'numeric',
        title: { text: 'Batch Index', style: { fontSize: '11px', color: tc.label } },
        labels: {
          style: { fontSize: '10px', colors: tc.label },
          formatter: function (v) { return v != null ? Math.round(v).toLocaleString() : ''; }
        },
        tickAmount: 8
      },
      yaxis: {
        logarithmic: !!logScale,
        title: { text: 'L1 Fee (sats)', style: { fontSize: '11px', color: tc.label } },
        labels: {
          style: { fontSize: '10px', colors: tc.label },
          formatter: function (v) { return v != null ? Number(v).toLocaleString() : ''; }
        }
      },
      fill: { type: 'solid', opacity: 0.85 },
      stroke: { width: 0 },
      dataLabels: { enabled: false },
      grid: {
        borderColor: tc.grid,
        strokeDashArray: 3,
        xaxis: { lines: { show: false } }
      },
      tooltip: {
        theme: tc.tooltip,
        y: { formatter: function (v) { return fmt(v) + ' sats'; } }
      },
      plotOptions: { bar: { borderRadius: 3, columnWidth: '60%' } },
      legend: {
        position: 'top',
        horizontalAlign: 'right',
        fontSize: '11px',
        labels: { colors: tc.label }
      }
    };
    if (annotation.annotations) config.annotations = annotation.annotations;
    return config;
  }

  return { fmt: fmt, fmtSats: fmtSats, getLaunchAnnotation: getLaunchAnnotation, makeChartOpts: makeChartOpts, makeFeesChartOpts: makeFeesChartOpts, getThemeColors: getThemeColors };
})();
