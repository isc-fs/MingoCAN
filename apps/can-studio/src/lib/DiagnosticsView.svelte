<!--
    Diagnostics view — wraps the three one-shot diagnose commands.
    Two panels stacked vertically:
      1. Session health (refresh button + key/value table)
      2. DTCs (refresh + clear buttons + entry table)

    Adapter and timing settings inherit from the selected adapter
    + sensible defaults. No persistence yet (lands when settings
    storage does).
-->
<script lang="ts">
    import {
        clearDtcs,
        formatUptime,
        getHealth,
        readDtcs,
        severityClass,
        type DiagnoseRequest,
        type DtcSnapshot,
        type HealthSnapshot,
    } from './diagnose';
    import type { AdapterEntry } from './types';

    interface Props {
        selectedAdapter: AdapterEntry | null;
    }

    const { selectedAdapter }: Props = $props();

    let health = $state<HealthSnapshot | null>(null);
    let dtcs = $state<DtcSnapshot[]>([]);
    let healthLoading = $state<boolean>(false);
    let dtcsLoading = $state<boolean>(false);
    let clearing = $state<boolean>(false);
    let healthError = $state<string | null>(null);
    let dtcsError = $state<string | null>(null);
    let dtcsRead = $state<boolean>(false);

    function buildRequest(): DiagnoseRequest | null {
        if (selectedAdapter === null) return null;
        return {
            interface: selectedAdapter.interface,
            channel:
                selectedAdapter.channel.length > 0
                    ? selectedAdapter.channel
                    : null,
            bitrate: 500_000,
            nodeId: 0x3,
            timeoutMs: 500,
        };
    }

    async function refreshHealth(): Promise<void> {
        const request = buildRequest();
        if (request === null) return;
        healthLoading = true;
        healthError = null;
        try {
            health = await getHealth(request);
        } catch (err) {
            healthError = err instanceof Error ? err.message : String(err);
        } finally {
            healthLoading = false;
        }
    }

    async function refreshDtcs(): Promise<void> {
        const request = buildRequest();
        if (request === null) return;
        dtcsLoading = true;
        dtcsError = null;
        try {
            dtcs = await readDtcs(request);
            dtcsRead = true;
        } catch (err) {
            dtcsError = err instanceof Error ? err.message : String(err);
        } finally {
            dtcsLoading = false;
        }
    }

    async function doClearDtcs(): Promise<void> {
        const request = buildRequest();
        if (request === null) return;
        // Native confirm() works inside a Tauri webview; for a more
        // polished modal we'd reach for @tauri-apps/plugin-dialog.
        // Good enough for v0.
        const ok = confirm(
            'Clear every DTC entry on the device? This cannot be undone.',
        );
        if (!ok) return;

        clearing = true;
        dtcsError = null;
        try {
            await clearDtcs(request);
            dtcs = [];
            dtcsRead = true;
        } catch (err) {
            dtcsError = err instanceof Error ? err.message : String(err);
        } finally {
            clearing = false;
        }
    }

    function fmtHex(value: number, width: number): string {
        return `0x${value.toString(16).toUpperCase().padStart(width, '0')}`;
    }
</script>

<div class="view">
    <header>
        <h2>Diagnostics</h2>
        <p class="muted">
            Session health + DTC table. One-shot calls against the configured
            adapter; nothing streams in the background.
        </p>
    </header>

    {#if selectedAdapter === null}
        <div class="warning">
            <strong>No adapter selected.</strong> Pick one in the
            <em>Adapters</em> view first.
        </div>
    {/if}

    <!-- Session health -->
    <section class="card">
        <header>
            <h3>Session health</h3>
            <button
                type="button"
                onclick={refreshHealth}
                disabled={healthLoading || selectedAdapter === null}
            >
                {healthLoading ? '…' : '⟳'}
                Refresh
            </button>
        </header>

        {#if healthError !== null}
            <div class="error">{healthError}</div>
        {/if}

        {#if health !== null}
            <table>
                <tbody>
                    <tr>
                        <th>Uptime</th>
                        <td>
                            {health.uptimeSeconds}s
                            <span class="muted">({formatUptime(health.uptimeSeconds)})</span>
                        </td>
                    </tr>
                    <tr>
                        <th>Reset cause</th>
                        <td>{health.resetCause}</td>
                    </tr>
                    <tr>
                        <th>Session active</th>
                        <td>{health.sessionActive ? 'yes' : 'no'}</td>
                    </tr>
                    <tr>
                        <th>Valid app</th>
                        <td>{health.validAppPresent ? 'yes' : 'no'}</td>
                    </tr>
                    <tr>
                        <th>WRP protected</th>
                        <td>{health.wrpProtected ? 'yes' : 'no'}</td>
                    </tr>
                    <tr>
                        <th>Flash writes</th>
                        <td>{health.flashWriteCount}</td>
                    </tr>
                    <tr>
                        <th>DTC count</th>
                        <td>{health.dtcCount}</td>
                    </tr>
                    <tr>
                        <th>Last DTC code</th>
                        <td><code>{fmtHex(health.lastDtcCode, 4)}</code></td>
                    </tr>
                    <tr>
                        <th>Raw flags</th>
                        <td><code>{fmtHex(health.rawFlags, 8)}</code></td>
                    </tr>
                </tbody>
            </table>
        {:else if !healthLoading && healthError === null}
            <p class="muted small">Click Refresh to fetch the latest snapshot.</p>
        {/if}
    </section>

    <!-- DTCs -->
    <section class="card">
        <header>
            <h3>Diagnostic Trouble Codes</h3>
            <div class="actions">
                <button
                    type="button"
                    onclick={refreshDtcs}
                    disabled={dtcsLoading || selectedAdapter === null}
                >
                    {dtcsLoading ? '…' : '⟳'}
                    Read DTCs
                </button>
                <button
                    type="button"
                    class="danger"
                    onclick={doClearDtcs}
                    disabled={clearing || selectedAdapter === null}
                >
                    {clearing ? '…' : '✕'}
                    Clear DTCs
                </button>
            </div>
        </header>

        {#if dtcsError !== null}
            <div class="error">{dtcsError}</div>
        {/if}

        {#if dtcsRead && dtcs.length === 0 && dtcsError === null}
            <p class="muted small">No DTCs logged. ✓</p>
        {:else if dtcs.length > 0}
            <table class="dtc-table">
                <thead>
                    <tr>
                        <th>Code</th>
                        <th>Severity</th>
                        <th>Count</th>
                        <th>First seen</th>
                        <th>Last seen</th>
                        <th>Context</th>
                    </tr>
                </thead>
                <tbody>
                    {#each dtcs as entry (entry.code)}
                        <tr>
                            <td><code>{fmtHex(entry.code, 4)}</code></td>
                            <td>
                                <span class="sev {severityClass(entry.severity)}">
                                    {entry.severity}
                                </span>
                            </td>
                            <td>{entry.occurrenceCount}</td>
                            <td>{entry.firstSeenUptimeSeconds}s</td>
                            <td>{entry.lastSeenUptimeSeconds}s</td>
                            <td><code>{fmtHex(entry.contextData, 8)}</code></td>
                        </tr>
                    {/each}
                </tbody>
            </table>
        {:else if !dtcsLoading && dtcsError === null}
            <p class="muted small">Click Read DTCs to fetch the current table.</p>
        {/if}
    </section>
</div>

<style>
    .view {
        display: flex;
        flex-direction: column;
        gap: 16px;
        padding: 24px 28px;
        overflow: auto;
    }
    h2 { margin: 0; font-size: 1.3rem; }
    .muted { color: var(--text-muted); }
    header .muted { margin: 4px 0 0; font-size: 0.9rem; }
    .small { font-size: 0.85rem; }

    .warning {
        padding: 10px 14px;
        border: 1px solid var(--accent);
        background: rgba(242, 178, 51, 0.08);
        border-radius: 6px;
    }
    .card {
        padding: 16px 18px;
        border: 1px solid var(--border);
        border-radius: 8px;
        background: var(--surface);
    }
    .card header {
        display: flex;
        align-items: center;
        justify-content: space-between;
        margin-bottom: 12px;
    }
    .card h3 { margin: 0; font-size: 1rem; }
    .card .actions { display: flex; gap: 8px; }

    button {
        appearance: none;
        background: var(--bg);
        color: var(--text);
        border: 1px solid var(--border);
        font: inherit;
        padding: 6px 12px;
        border-radius: 5px;
        cursor: pointer;
        font-size: 0.85rem;
    }
    button:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
    button:disabled { opacity: 0.5; cursor: not-allowed; }
    button.danger:hover:not(:disabled) { border-color: var(--error); color: var(--error); }

    .error {
        padding: 10px 14px;
        border: 1px solid var(--error);
        color: var(--error);
        border-radius: 6px;
        background: rgba(255, 115, 115, 0.08);
        margin-bottom: 10px;
        font-size: 0.85rem;
    }

    table {
        width: 100%;
        border-collapse: collapse;
        font-size: 0.85rem;
    }
    th, td {
        text-align: left;
        padding: 6px 10px;
        border-bottom: 1px solid var(--border);
    }
    tbody tr:last-child td { border-bottom: none; }
    th {
        color: var(--text-muted);
        font-weight: 500;
        font-size: 0.75rem;
        text-transform: uppercase;
        letter-spacing: 0.04em;
    }
    code { font-family: var(--font-mono); }

    .dtc-table thead th {
        border-bottom: 1px solid var(--border);
    }

    .sev {
        font-size: 0.7rem;
        padding: 1px 8px;
        border-radius: 3px;
        text-transform: uppercase;
        letter-spacing: 0.05em;
        font-weight: 600;
        border: 1px solid;
    }
    .sev-info {
        color: #4cc9f0;
        border-color: rgba(76, 201, 240, 0.4);
        background: rgba(76, 201, 240, 0.08);
    }
    .sev-warn {
        color: #ffd166;
        border-color: rgba(255, 209, 102, 0.4);
        background: rgba(255, 209, 102, 0.08);
    }
    .sev-error {
        color: var(--error);
        border-color: rgba(255, 115, 115, 0.4);
        background: rgba(255, 115, 115, 0.08);
    }
    .sev-unknown {
        color: var(--text-muted);
        border-color: var(--border);
    }
</style>
