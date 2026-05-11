/* eslint-disable no-undef */
// ISC CAN live-data webview client.
//
// Runs inside the WebviewPanel sandbox. Sets up a single Chart.js
// line chart with two datasets (frames RX/sec and frames TX/sec —
// computed from the absolute counters the CLI emits), receives
// snapshot messages from the extension host, and posts back start /
// stop button events.
//
// Counters and pill indicators are plain DOM updates; the chart is
// the only fancy thing here.

(() => {
    'use strict';

    const vscode = acquireVsCodeApi();

    // ---- DOM handles ----

    const startBtn = document.getElementById('start');
    const stopBtn = document.getElementById('stop');
    const clearBtn = document.getElementById('clear');
    const statusEl = document.getElementById('status');
    const indicators = document.getElementById('indicators');
    const counters = document.getElementById('counters');
    const canvas = document.getElementById('chart');

    // ---- Chart setup ----

    // v4 UMD bundles ship the registerables but don't pre-call
    // Chart.register. Doing it ourselves keeps us future-proof
    // against minor build-bundle changes.
    if (typeof Chart.register === 'function' && Chart.registerables !== undefined) {
        Chart.register(...Chart.registerables);
    }

    const RX_COLOUR = 'rgba(76, 175, 80, 0.9)';
    const TX_COLOUR = 'rgba(33, 150, 243, 0.9)';

    const chart = new Chart(canvas.getContext('2d'), {
        type: 'line',
        data: {
            datasets: [
                {
                    label: 'frames/s RX',
                    data: [],
                    borderColor: RX_COLOUR,
                    backgroundColor: 'transparent',
                    borderWidth: 2,
                    pointRadius: 0,
                    tension: 0.2,
                },
                {
                    label: 'frames/s TX',
                    data: [],
                    borderColor: TX_COLOUR,
                    backgroundColor: 'transparent',
                    borderWidth: 2,
                    pointRadius: 0,
                    tension: 0.2,
                },
            ],
        },
        options: {
            responsive: true,
            maintainAspectRatio: false,
            animation: false,
            parsing: false,
            interaction: { mode: 'nearest', intersect: false },
            plugins: {
                legend: {
                    position: 'top',
                    labels: { color: cssVar('--vscode-foreground', '#ccc') },
                },
                tooltip: {
                    callbacks: {
                        title: (items) =>
                            items.length > 0 ? `t = ${items[0].parsed.x.toFixed(1)} s` : '',
                    },
                },
            },
            scales: {
                x: {
                    type: 'linear',
                    title: { display: true, text: 'uptime (s)' },
                    ticks: { color: cssVar('--vscode-descriptionForeground', '#888') },
                    grid: { color: cssVar('--vscode-panel-border', '#444') },
                },
                y: {
                    type: 'linear',
                    title: { display: true, text: 'frames / s' },
                    beginAtZero: true,
                    ticks: { color: cssVar('--vscode-descriptionForeground', '#888') },
                    grid: { color: cssVar('--vscode-panel-border', '#444') },
                },
            },
        },
    });

    // ---- State ----

    /** Sliding-window size in seconds; updated from host on `config`. */
    let windowSeconds = 60;
    /** Most recent snapshot — used to compute deltas for rate datasets. */
    let prevSnapshot = null;

    // ---- Button wiring ----

    startBtn.addEventListener('click', () => vscode.postMessage({ type: 'start' }));
    stopBtn.addEventListener('click', () => vscode.postMessage({ type: 'stop' }));
    clearBtn.addEventListener('click', () => {
        prevSnapshot = null;
        chart.data.datasets.forEach((ds) => (ds.data.length = 0));
        chart.update('none');
    });

    // ---- Message handling (host → webview) ----

    window.addEventListener('message', (event) => {
        const msg = event.data;
        if (msg === undefined || msg === null || typeof msg.type !== 'string') {
            return;
        }
        switch (msg.type) {
            case 'config':
                if (typeof msg.windowSeconds === 'number') {
                    windowSeconds = msg.windowSeconds;
                }
                break;
            case 'status':
                applyStatus(msg.status, msg.message);
                break;
            case 'snapshot':
                applySnapshot(msg.data);
                break;
            case 'reset':
                prevSnapshot = null;
                chart.data.datasets.forEach((ds) => (ds.data.length = 0));
                chart.update('none');
                break;
        }
    });

    vscode.postMessage({ type: 'ready' });

    // ---- Rendering helpers ----

    function applyStatus(status, message) {
        statusEl.classList.remove('idle', 'running', 'stopped', 'error');
        statusEl.classList.add(status);
        statusEl.textContent = message ?? capitalise(status);
        const running = status === 'running';
        startBtn.disabled = running;
        stopBtn.disabled = !running;
    }

    function applySnapshot(s) {
        renderIndicators(s);
        renderCounters(s);
        updateChart(s);
    }

    function renderIndicators(s) {
        const items = [
            { label: 'session active', on: s.session_active },
            { label: 'valid app', on: s.valid_app_present },
            { label: 'WRP', on: s.wrp_protected },
            { label: 'log stream', on: s.log_streaming },
            { label: 'live-data stream', on: s.livedata_streaming },
        ];
        indicators.innerHTML = '';
        for (const item of items) {
            const pill = document.createElement('span');
            pill.className = `pill ${item.on ? 'on' : 'off'}`;
            pill.textContent = item.label;
            indicators.appendChild(pill);
        }
    }

    function renderCounters(s) {
        const cells = [
            ['Uptime', formatMs(s.uptime_ms)],
            ['Session age', formatMs(s.session_age_ms)],
            ['DTCs', s.dtc_count],
            ['NACKs sent', s.nacks_sent],
            ['Last DTC code', `0x${hex(s.last_dtc_code, 4)}`],
            ['Last opcode', `0x${hex(s.last_opcode, 2)}`],
            ['Last flash addr', `0x${hex(s.last_flash_addr, 8)}`],
            ['ISO-TP RX bytes', s.isotp_rx_progress],
        ];
        counters.innerHTML = '';
        for (const [label, value] of cells) {
            const cell = document.createElement('div');
            cell.className = 'counter';
            const lbl = document.createElement('div');
            lbl.className = 'label';
            lbl.textContent = label;
            const val = document.createElement('div');
            val.className = 'value';
            val.textContent = String(value);
            cell.appendChild(lbl);
            cell.appendChild(val);
            counters.appendChild(cell);
        }
    }

    function updateChart(curr) {
        if (prevSnapshot !== null) {
            const dt = (curr.uptime_ms - prevSnapshot.uptime_ms) / 1000;
            if (dt > 0) {
                const rxRate = (curr.frames_rx - prevSnapshot.frames_rx) / dt;
                const txRate = (curr.frames_tx - prevSnapshot.frames_tx) / dt;
                const t = curr.uptime_ms / 1000;
                chart.data.datasets[0].data.push({ x: t, y: Math.max(0, rxRate) });
                chart.data.datasets[1].data.push({ x: t, y: Math.max(0, txRate) });

                // Prune points older than the sliding window.
                const cutoff = t - windowSeconds;
                for (const ds of chart.data.datasets) {
                    while (ds.data.length > 0 && ds.data[0].x < cutoff) {
                        ds.data.shift();
                    }
                }
                chart.update('none');
            }
        }
        prevSnapshot = curr;
    }

    // ---- Formatting ----

    function formatMs(ms) {
        const total = Math.floor(ms / 1000);
        const h = Math.floor(total / 3600);
        const m = Math.floor((total % 3600) / 60);
        const sec = total % 60;
        if (h > 0) {
            return `${h}h${pad2(m)}m${pad2(sec)}s`;
        }
        if (m > 0) {
            return `${m}m${pad2(sec)}s`;
        }
        return `${sec}.${String(ms % 1000).padStart(3, '0').slice(0, 1)}s`;
    }

    function hex(n, w) {
        return n.toString(16).toUpperCase().padStart(w, '0');
    }

    function pad2(n) {
        return String(n).padStart(2, '0');
    }

    function capitalise(s) {
        return s.charAt(0).toUpperCase() + s.slice(1);
    }

    function cssVar(name, fallback) {
        const value = getComputedStyle(document.documentElement).getPropertyValue(name);
        return value.trim().length > 0 ? value.trim() : fallback;
    }
})();
