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
    import { ask } from '@tauri-apps/plugin-dialog';

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
    import { settings } from './settings.svelte';
    import type { ViewId } from './stores';

    interface Props {
        navigateTo: (id: ViewId) => void;
    }
    const { navigateTo }: Props = $props();

    const adapterReady = $derived(
        settings.adapter.interface !== null &&
            (settings.adapter.interface === 'virtual' ||
                settings.adapter.channel.length > 0),
    );

    let health = $state<HealthSnapshot | null>(null);
    let dtcs = $state<DtcSnapshot[]>([]);
    let healthLoading = $state<boolean>(false);
    let dtcsLoading = $state<boolean>(false);
    let clearing = $state<boolean>(false);
    let healthError = $state<string | null>(null);
    let dtcsError = $state<string | null>(null);
    let dtcsRead = $state<boolean>(false);

    function buildRequest(): DiagnoseRequest | null {
        if (!adapterReady || settings.adapter.interface === null) return null;
        return {
            interface: settings.adapter.interface,
            channel:
                settings.adapter.channel.length > 0
                    ? settings.adapter.channel
                    : null,
            bitrate: settings.adapter.bitrate,
            nodeId: settings.adapter.nodeId,
            timeoutMs: settings.adapter.timeoutMs,
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
        const ok = await ask(
            'Clear every DTC entry on the device? This cannot be undone.',
            {
                title: 'Clear DTCs',
                kind: 'warning',
                okLabel: 'Clear',
                cancelLabel: 'Cancel',
            },
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
        <div>
            <h2>Board health</h2>
            <p class="muted">
                Session health + DTC table. One-shot calls against the
                configured adapter; nothing streams in the background.
            </p>
        </div>
    </header>

    {#if !adapterReady}
        <div class="banner banner-warning gate">
            <span>
                <strong>No adapter selected.</strong> Diagnostics needs an
                adapter to reach the board.
            </span>
            <button
                type="button"
                class="btn btn-sm gate-action"
                onclick={() => navigateTo('adapters')}
            >
                Choose adapter →
            </button>
        </div>
    {/if}

    <!-- Session health -->
    <section class="card">
        <div class="card-header">
            <h3>Session health</h3>
            <button
                type="button"
                class="btn btn-sm"
                onclick={refreshHealth}
                disabled={healthLoading || !adapterReady}
            >
                {healthLoading ? '…' : '⟳'}
                Refresh
            </button>
        </div>

        {#if healthError !== null}
            <div class="banner banner-danger">{healthError}</div>
        {/if}

        {#if health !== null}
            <table class="kv-table">
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
        <div class="card-header">
            <h3>Diagnostic Trouble Codes</h3>
            <div class="card-actions">
                <button
                    type="button"
                    class="btn btn-sm"
                    onclick={refreshDtcs}
                    disabled={dtcsLoading || !adapterReady}
                >
                    {dtcsLoading ? '…' : '⟳'}
                    Read DTCs
                </button>
                <button
                    type="button"
                    class="btn btn-sm btn-danger"
                    onclick={doClearDtcs}
                    disabled={clearing || !adapterReady}
                >
                    {clearing ? '…' : '✕'}
                    Clear DTCs
                </button>
            </div>
        </div>

        {#if dtcsError !== null}
            <div class="banner banner-danger">{dtcsError}</div>
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
    /* All view chrome (.view, .card, .card-header, .banner-*, .btn,
       .muted, .small) comes from app.css. Local styles cover the
       two tables + the per-severity DTC pills, which are view-
       specific shapes the design system doesn't ship. */

    .card-actions {
        display: flex;
        gap: var(--space-2);
    }

    /* Key/value table for session-health card — flat two-column
       layout, no head row. Labels in --text-muted, values in --text. */
    .kv-table {
        width: 100%;
        border-collapse: collapse;
        font-size: var(--text-sm);
    }
    .kv-table th,
    .kv-table td {
        text-align: left;
        padding: var(--space-2) var(--space-3);
        border-bottom: 1px solid var(--border);
    }
    .kv-table tbody tr:last-child td,
    .kv-table tbody tr:last-child th {
        border-bottom: none;
    }
    .kv-table th {
        color: var(--text-muted);
        font-weight: 500;
        width: 40%;
    }

    /* DTC table — full thead. */
    .dtc-table {
        width: 100%;
        border-collapse: collapse;
        font-size: var(--text-sm);
    }
    .dtc-table th,
    .dtc-table td {
        text-align: left;
        padding: var(--space-2) var(--space-3);
        border-bottom: 1px solid var(--border);
    }
    .dtc-table tbody tr:last-child td {
        border-bottom: none;
    }
    .dtc-table thead th {
        color: var(--text-muted);
        font-weight: 500;
        font-size: var(--text-xs);
    }

    code {
        font-family: var(--font-mono);
    }

    /* Severity tag — outline-only, sized down to match table density.
       Color encodes severity rather than a soft-background fill. */
    .sev {
        font-size: var(--text-xs);
        padding: 1px var(--space-2);
        border-radius: var(--radius-pill);
        font-weight: 500;
        border: 1px solid var(--border);
        color: var(--text-secondary);
    }
    .sev-info {
        color: var(--info);
        border-color: var(--info);
    }
    .sev-warn {
        color: var(--warning);
        border-color: var(--warning);
    }
    .sev-error {
        color: var(--danger);
        border-color: var(--danger);
    }
    .sev-unknown {
        color: var(--text-muted);
    }
</style>
