<script lang="ts">
    /*
        Data logs — pull the car-data logs off a node's microSD card over
        CAN (#506, firmware spec IFS08-CE-AMS#406).

        Built around the one task that matters in the pits: get the run we
        just did. Opening the tab lists the card; the newest file sits at
        the top with one button; downloads go straight to a remembered
        folder (<root>/<ROLE>/<YYYY-MM-DD>/) with no dialog.

        The card can't delete, so the backend keeps a per-laptop ledger and
        each scan says which files are already safely on this disk. Those
        fold into a collapsed group — but only while the copy really
        exists at the listed size; a deleted copy shows up as "missing",
        never hidden. LOG, IMU and CEL files are shown separately; binary
        logs (.BIN) are decoded to a CSV beside them as they're saved.

        Transfers run at classic-CAN speeds (10–20 kB/s), so a 4 MiB file
        is minutes: the top card turns into a progress panel with rate,
        time left and cancel.
    */
    import { onDestroy, onMount, untrack } from 'svelte';
    import { ask, open as openDialog } from '@tauri-apps/plugin-dialog';
    import type { UnlistenFn } from '@tauri-apps/api/event';

    import { settings } from './settings.svelte';
    import {
        logsScan,
        logsPull,
        logsCancel,
        logsMark,
        logsDefaultRoot,
        logsReveal,
        onPullProgress,
        setLogsTransferActive,
        CANCELLED_MSG,
        CARD_CHANGED_MSG,
        kindOf,
        newestFirst,
        localDate,
        joinPath,
        formatBytes,
        formatDuration,
        formatPulledAt,
        estimate,
        type LogKind,
        type LogsRequest,
        type PullResult,
        type ScanResult,
        type ScannedFile,
    } from './logs';
    import { ROLES } from './provision';
    import NodeIdRolePicker from './NodeIdRolePicker.svelte';
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

    // ---- Target + destination ----

    const node = $derived(settings.logs.nodeId);
    const roleName = $derived(
        ROLES.find((r) => r.nodeId === node)?.name.toUpperCase() ?? null,
    );
    const nodeLabel = $derived(
        node === null
            ? 'no board'
            : `${roleName ?? 'node'} 0x${node.toString(16).padStart(2, '0')}`,
    );
    /** Per-board folder, so ECU / uDV logs never land among the AMS's. */
    const boardFolder = $derived(
        roleName ?? `node-0x${(node ?? 0).toString(16).padStart(2, '0')}`,
    );

    let defaultRoot = $state<string | null>(null);
    const root = $derived(
        settings.logs.rootDir !== '' ? settings.logs.rootDir : defaultRoot,
    );
    const boardDir = $derived(root === null ? null : joinPath(root, boardFolder));
    /** Where the next download lands (the date is the download date —
     *  the card has no clock). */
    function destDir(): string | null {
        if (boardDir === null) return null;
        return settings.logs.dateFolders ? joinPath(boardDir, localDate()) : boardDir;
    }

    function buildRequest(): LogsRequest | null {
        if (!adapterReady || settings.adapter.interface === null) return null;
        return {
            interface: settings.adapter.interface,
            channel:
                settings.adapter.channel.length > 0
                    ? settings.adapter.channel
                    : null,
            bitrate: settings.adapter.bitrate,
            nodeId: settings.logs.nodeId,
            timeoutMs: settings.adapter.timeoutMs,
        };
    }

    // ---- Scan ----

    let scan = $state<ScanResult | null>(null);
    let scanning = $state<boolean>(false);
    let scannedAt = $state<Date | null>(null);
    let scanError = $state<string | null>(null);

    async function rescan(): Promise<void> {
        const request = buildRequest();
        if (request === null || scanning || current !== null) return;
        scanning = true;
        scanError = null;
        try {
            scan = await logsScan(request);
            scannedAt = new Date();
            savedNow = {};
        } catch (err) {
            scanError = err instanceof Error ? err.message : String(err);
            scan = null;
            scannedAt = null;
        } finally {
            scanning = false;
        }
    }

    // List on open, and again when the board changes. Debounced so typing
    // a custom node id doesn't fire a scan per keystroke.
    let scanTimer: ReturnType<typeof setTimeout> | null = null;
    $effect(() => {
        void settings.logs.nodeId;
        void adapterReady;
        untrack(() => {
            scan = null;
            if (scanTimer !== null) clearTimeout(scanTimer);
            scanTimer = setTimeout(() => void rescan(), 300);
        });
    });

    // ---- Grouping for the selected kind (LOG, IMU, CEL…) ----

    /** Rows downloaded since the last scan stay where they were (marked
     *  saved) instead of jumping into the collapsed group under the cursor. */
    let savedNow = $state<Record<number, string>>({});

    const kind = $derived(settings.logs.kind);
    const ofKind = $derived(
        scan === null
            ? []
            : scan.files.filter((f) => kindOf(f.index) === kind).sort(newestFirst),
    );
    const fresh = $derived(
        ofKind.filter(
            (f) => f.status === 'new' || savedNow[f.index] !== undefined,
        ),
    );
    const toDownload = $derived(fresh.filter((f) => savedNow[f.index] === undefined));
    const missing = $derived(ofKind.filter((f) => f.status === 'missing'));
    const downloaded = $derived(
        ofKind.filter(
            (f) => f.status === 'downloaded' && savedNow[f.index] === undefined,
        ),
    );
    const hidden = $derived(ofKind.filter((f) => f.status === 'hidden'));
    /** The top of the card, ignoring files the operator dismissed. */
    const newest = $derived(ofKind.find((f) => f.status !== 'hidden') ?? null);
    const toDownloadBytes = $derived(toDownload.reduce((n, f) => n + f.size, 0));

    function newCount(k: LogKind): number {
        if (scan === null) return 0;
        return scan.files.filter(
            (f) =>
                kindOf(f.index) === k &&
                f.status === 'new' &&
                savedNow[f.index] === undefined,
        ).length;
    }

    // ---- Download queue ----

    let current = $state<ScannedFile | null>(null);
    let queue = $state<ScannedFile[]>([]);
    let queueTotal = $state<number>(0);
    let queueDone = $state<number>(0);
    let stopAfter = $state<boolean>(false);
    let cancelling = $state<boolean>(false);
    let received = $state<number>(0);
    let total = $state<number>(0);
    let samples: { t: number; bytes: number }[] = [];
    let rate = $state<number | null>(null);
    let lastProgressAt = $state<number>(0);
    let now = $state<number>(0);
    let ticker: ReturnType<typeof setInterval> | null = null;

    /** Outcome line under the top card. */
    let notice = $state<{
        tone: 'success' | 'info' | 'warning' | 'danger';
        text: string;
        path?: string;
    } | null>(null);

    let unlisten: UnlistenFn | null = null;
    onPullProgress((p) => {
        if (current === null || p.index !== current.index) return;
        received = p.received;
        if (p.total > 0) total = p.total;
        const t = performance.now();
        lastProgressAt = t;
        samples.push({ t, bytes: p.received });
        // Rate over the last 10 s: steady enough to read, quick to react.
        while (samples.length > 2 && t - samples[0].t > 10_000) samples.shift();
        const span = (t - samples[0].t) / 1000;
        rate = span >= 2 ? (p.received - samples[0].bytes) / span : null;
    }).then((fn) => (unlisten = fn));

    onMount(async () => {
        try {
            defaultRoot = await logsDefaultRoot();
        } catch (err) {
            notice = {
                tone: 'warning',
                text: `No default download folder: ${err instanceof Error ? err.message : err}. Pick one with Change….`,
            };
        }
    });

    // Leaving the view cancels the transfer — the pull holds the one CAN
    // adapter for minutes, and an orphaned one would lock every other view
    // out. App.svelte asks before navigating away while this is running.
    onDestroy(() => {
        unlisten?.();
        if (scanTimer !== null) clearTimeout(scanTimer);
        if (ticker !== null) clearInterval(ticker);
        if (current !== null) void logsCancel();
        setLogsTransferActive(false);
    });

    async function download(list: ScannedFile[]): Promise<void> {
        const request = buildRequest();
        const first = destDir();
        if (request === null || first === null || list.length === 0 || current !== null) return;
        queue = [...list];
        queueTotal = list.length;
        queueDone = 0;
        stopAfter = false;
        notice = null;
        setLogsTransferActive(true);
        now = performance.now();
        ticker = setInterval(() => (now = performance.now()), 1000);
        let relist = false;
        try {
            while (queue.length > 0) {
                const file = queue.shift()!;
                const dir = destDir()!;
                current = file;
                received = 0;
                total = file.size;
                samples = [];
                rate = null;
                lastProgressAt = performance.now();
                try {
                    const res = await logsPull(request, file, dir);
                    file.status = 'downloaded';
                    file.path = res.path;
                    file.csvPath = res.decoded?.path ?? null;
                    file.pulledAt = Math.floor(Date.now() / 1000);
                    // Point "Show" at the spreadsheet when a binary log was
                    // decoded — that's the file people open.
                    savedNow[file.index] = res.decoded?.path ?? res.path;
                    queueDone += 1;
                    notice = savedNotice(file, res);
                } catch (err) {
                    const msg = err instanceof Error ? err.message : String(err);
                    if (msg.includes(CANCELLED_MSG)) {
                        notice = { tone: 'info', text: `Cancelled — ${file.name} was not saved.` };
                    } else if (msg.includes(CARD_CHANGED_MSG)) {
                        notice = {
                            tone: 'warning',
                            text: 'The card changed since it was listed (the AMS restarted?). Listed it again — check the newest file and download again.',
                        };
                        relist = true;
                    } else {
                        notice = { tone: 'danger', text: msg };
                    }
                    break;
                }
                if (stopAfter) break;
            }
        } finally {
            current = null;
            queue = [];
            cancelling = false;
            if (ticker !== null) clearInterval(ticker);
            ticker = null;
            setLogsTransferActive(false);
        }
        if (relist) await rescan();
    }

    function savedNotice(file: ScannedFile, res: PullResult): NonNullable<typeof notice> {
        const path = res.decoded?.path ?? res.path;
        // Both problems can happen on one pull; say both.
        const problems: string[] = [];
        if (res.ledgerError) {
            problems.push(`couldn't record it (${res.ledgerError}), so it will show as new next time`);
        }
        if (res.decodeError) {
            problems.push(`couldn't turn it into a CSV (${res.decodeError}) — the file itself is fine and kept`);
        }
        if (problems.length > 0) {
            return { tone: 'warning', text: `Saved ${file.name}, but ${problems.join('; and ')}.`, path };
        }
        let text = `Saved ${file.name}${res.crcVerified ? ' · CRC verified' : ''}`;
        if (res.decoded) {
            text += ` · decoded to ${baseName(res.decoded.path)} (${res.decoded.records.toLocaleString()} records)`;
            if (res.decoded.tornBytes > 0) text += ' · last partial record dropped (cut short by a power-off)';
        }
        return { tone: 'success', text, path };
    }

    function baseName(path: string): string {
        return path.split(/[\\/]/).pop() ?? path;
    }

    async function cancelNow(): Promise<void> {
        cancelling = true;
        try {
            await logsCancel();
        } catch {
            /* the pull may have finished in the meantime — harmless */
        }
    }

    const pct = $derived(
        total > 0 ? Math.min(100, Math.round((received / total) * 100)) : 0,
    );
    const remainingBytes = $derived(
        Math.max(0, total - received) + queue.reduce((n, f) => n + f.size, 0),
    );
    const stalled = $derived(current !== null && now - lastProgressAt > 5_000);

    // ---- Hide / unhide / forget (this laptop's ledger only) ----

    async function mark(
        action: 'hide' | 'unhide' | 'forget',
        files: ScannedFile[],
    ): Promise<void> {
        if (node === null || files.length === 0) return;
        try {
            await logsMark(action, node, files);
        } catch (err) {
            notice = { tone: 'danger', text: `Couldn't update the download list: ${err instanceof Error ? err.message : err}` };
            return;
        }
        for (const f of files) {
            if (action === 'hide') f.status = 'hidden';
            if (action === 'forget') {
                f.status = 'new';
                f.path = null;
                f.pulledAt = null;
            }
        }
        // An unhidden file goes back to whatever it really is (new,
        // downloaded or missing) — only a scan knows that.
        if (action === 'unhide') {
            for (const f of files) f.status = 'new';
            await rescan();
        }
    }

    async function hideAllNew(): Promise<void> {
        const list = toDownload;
        const ok = await ask(
            `Hide ${list.length} ${kindWord}${list.length === 1 ? '' : 's'} on this laptop? Nothing is deleted from the card or the disk — they move to "Hidden", where you can bring them back.`,
            { title: 'Hide all new files', kind: 'info', okLabel: 'Hide', cancelLabel: 'Keep' },
        );
        if (ok) await mark('hide', list);
    }

    // ---- Folder ----

    async function changeFolder(): Promise<void> {
        const picked = await openDialog({
            directory: true,
            multiple: false,
            defaultPath: root ?? undefined,
        });
        if (typeof picked === 'string') settings.logs.rootDir = picked;
    }

    async function reveal(path: string | null): Promise<void> {
        if (path === null) return;
        try {
            await logsReveal(path);
        } catch (err) {
            notice = { tone: 'info', text: err instanceof Error ? err.message : String(err) };
        }
    }

    async function openFolder(): Promise<void> {
        // The board folder only exists after the first download.
        if (boardDir === null || root === null) return;
        try {
            await logsReveal(boardDir);
        } catch {
            await reveal(root);
        }
    }

    // ---- Focus: the newest-file button takes Enter as soon as it shows ----

    let heroButton = $state<HTMLButtonElement | null>(null);
    $effect(() => {
        heroButton?.focus();
    });

    const KINDS: ReadonlyArray<{ k: LogKind; label: string; word: string }> = [
        { k: 'log', label: 'LOG files', word: 'log' },
        { k: 'imu', label: 'IMU files', word: 'IMU log' },
        { k: 'cel', label: 'CEL files', word: 'cell log' },
        { k: 'ele', label: 'ELE files', word: 'current log' },
    ];

    const kindWord = $derived(KINDS.find((x) => x.k === kind)?.word ?? 'file');
    const listedAt = $derived(
        scannedAt?.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) ?? null,
    );
</script>

<div class="view">
    <header class="view-header">
        <h2>Data logs</h2>
        <p class="muted">
            Pull the car-data logs off a board's microSD card over CAN — no
            card removal. Read-only: nothing on the card is changed.
        </p>
    </header>

    {#if !adapterReady}
        <div class="card placeholder-card">
            <h3>No adapter selected</h3>
            <p class="muted">
                Pick a CAN adapter and channel first — the log transfer uses
                the same diagnostic link as flashing.
            </p>
            <button type="button" class="btn btn-primary" onclick={() => navigateTo('adapters')}>
                Go to Adapters
            </button>
        </div>
    {:else}
        <!-- Where from, where to. -->
        <div class="card card-tight setup">
            <div class="setup-row">
                <span class="label">Logs from</span>
                <!-- Switching boards re-lists the card: not mid-download. -->
                <fieldset class="plain" disabled={current !== null}>
                    <NodeIdRolePicker bind:value={settings.logs.nodeId} />
                </fieldset>
                <span class="spacer"></span>
                <span class="muted small">
                    {#if scanning}
                        checking the card…
                    {:else if listedAt !== null}
                        listed {listedAt}
                    {/if}
                </span>
                <button
                    type="button"
                    class="btn btn-sm"
                    disabled={scanning || current !== null}
                    onclick={rescan}
                >
                    Check again
                </button>
            </div>
            <div class="setup-row">
                <span class="label">Saving to</span>
                <strong class="mono path" title={boardDir ?? ''}>
                    {boardDir === null
                        ? '…'
                        : settings.logs.dateFolders
                          ? joinPath(boardDir, localDate())
                          : boardDir}
                </strong>
                <span class="spacer"></span>
                <label class="check small">
                    <input type="checkbox" bind:checked={settings.logs.dateFolders} />
                    one folder per day
                </label>
                <button type="button" class="btn btn-sm" disabled={root === null} onclick={openFolder}>
                    Open folder
                </button>
                <button
                    type="button"
                    class="btn btn-sm"
                    disabled={current !== null}
                    onclick={changeFolder}
                >
                    Change…
                </button>
                {#if settings.logs.rootDir !== ''}
                    <button
                        type="button"
                        class="btn btn-sm btn-ghost"
                        disabled={current !== null}
                        onclick={() => (settings.logs.rootDir = '')}
                    >
                        Use default
                    </button>
                {/if}
            </div>
        </div>

        <div class="segmented kind-tabs" role="group" aria-label="File type">
            {#each KINDS as { k, label } (k)}
                {@const n = newCount(k)}
                <button
                    type="button"
                    class="seg"
                    class:active={kind === k}
                    aria-pressed={kind === k}
                    onclick={() => (settings.logs.kind = k)}
                >
                    {label}{#if n > 0}<span class="seg-badge">{n} new</span>{/if}
                </button>
            {/each}
        </div>

        {#if scan?.cardChanged}
            <div class="banner banner-warning">
                <strong>This card doesn't match earlier downloads.</strong>
                It was probably reformatted or swapped, so everything is shown
                as new.
            </div>
        {/if}
        {#if scan?.ledgerError}
            <div class="banner banner-warning">
                <strong>Couldn't read the download list</strong>
                ({scan.ledgerError}), so everything is shown as new.
            </div>
        {/if}

        <!-- The top card: newest file, or the transfer in progress. -->
        <section class="card hero">
            {#if current !== null}
                <div class="hero-head">
                    <span class="eyebrow">Downloading</span>
                    <span class="mono">{current.name}</span>
                    {#if queueTotal > 1}
                        <span class="muted small">file {queueDone + 1} of {queueTotal}</span>
                    {/if}
                </div>
                <div class="meter">
                    <div class="meter-fill" style="width: {pct}%"></div>
                </div>
                <p class="progress-line mono small">
                    <span>{pct}%</span>
                    <span>{formatBytes(received)} / {formatBytes(total)}</span>
                    {#if stalled}
                        <span class="warn">waiting for {roleName ?? 'the board'} to answer…</span>
                    {:else if rate !== null && rate > 0}
                        <span>{(rate / 1000).toFixed(1)} kB/s</span>
                        <span>~{formatDuration(remainingBytes / rate)} left{queue.length > 0 ? ' in total' : ''}</span>
                    {:else}
                        <span class="muted">estimating…</span>
                    {/if}
                </p>
                {#if queue.length > 0}
                    <p class="muted small">
                        Next: {queue.slice(0, 3).map((f) => f.name).join(', ')}{queue.length > 3
                            ? ` and ${queue.length - 3} more`
                            : ''}
                    </p>
                {/if}
                <div class="hero-actions">
                    <span class="muted small">Stay on this page — leaving cancels the download.</span>
                    <span class="spacer"></span>
                    {#if queue.length > 0}
                        <button
                            type="button"
                            class="btn"
                            disabled={stopAfter || cancelling}
                            onclick={() => (stopAfter = true)}
                        >
                            {stopAfter ? 'Stopping after this file' : 'Stop after this file'}
                        </button>
                    {/if}
                    <button type="button" class="btn btn-danger" disabled={cancelling} onclick={cancelNow}>
                        {cancelling ? 'Cancelling…' : 'Cancel now'}
                    </button>
                </div>
            {:else if scanning && scan === null}
                <p class="muted">Checking the card on {nodeLabel}…</p>
            {:else if scanError !== null}
                <div class="hero-head">
                    <span class="eyebrow warn">No answer</span>
                </div>
                <p>{scanError}</p>
                <p class="muted small">
                    Is the low-voltage system on and the {roleName ?? 'board'}'s
                    application running? Logs come from the application firmware,
                    not the bootloader.
                </p>
                <div class="hero-actions">
                    <button type="button" class="btn btn-primary" disabled={scanning} onclick={rescan}>
                        Check again
                    </button>
                </div>
            {:else if scan === null}
                <p class="muted">Not listed yet.</p>
                <div class="hero-actions">
                    <button type="button" class="btn btn-primary" disabled={scanning} onclick={rescan}>
                        Check the card
                    </button>
                </div>
            {:else if newest === null}
                <div class="hero-head"><span class="eyebrow">No {kindWord}s on the card</span></div>
                <p class="muted small">
                    The file being written right now appears after the
                    {roleName ?? 'board'} restarts — power-cycle the car, then
                    Check again.
                </p>
            {:else}
                {@const isNew = newest.status === 'new' && savedNow[newest.index] === undefined}
                <div class="hero-head">
                    <span class="eyebrow">Newest {kindWord}</span>
                    <span class="mono hero-name">{newest.name}</span>
                    <span class="muted mono small">{formatBytes(newest.size)}</span>
                    {#if isNew}
                        <span class="badge badge-new">new</span>
                    {:else if newest.status === 'missing'}
                        <span class="badge badge-warn">missing on disk</span>
                    {:else}
                        <span class="badge badge-ok">downloaded {formatPulledAt(newest.pulledAt)}</span>
                    {/if}
                </div>
                <div class="hero-actions">
                    {#if isNew || newest.status === 'missing'}
                        <button
                            type="button"
                            class="btn btn-primary btn-hero"
                            bind:this={heroButton}
                            disabled={root === null}
                            onclick={() => download([newest])}
                        >
                            Download newest · {estimate(newest.size)}
                        </button>
                    {:else}
                        <button type="button" class="btn" onclick={() => reveal(newest.csvPath ?? newest.path)}>
                            Show in folder
                        </button>
                    {/if}
                    {#if toDownload.length > 1 || (toDownload.length === 1 && toDownload[0] !== newest)}
                        <button
                            type="button"
                            class="btn"
                            disabled={root === null}
                            onclick={() => download(toDownload)}
                        >
                            Download all {toDownload.length} new · {formatBytes(toDownloadBytes)} · {estimate(toDownloadBytes)}
                        </button>
                    {/if}
                </div>
                <p class="muted small">
                    {#if toDownload.length === 0}
                        Nothing new on the card.
                    {/if}
                    The file being written right now appears after the
                    {roleName ?? 'board'} restarts — power-cycle the car, then
                    Check again.
                </p>
            {/if}
        </section>

        {#if notice !== null}
            <div class="banner banner-{notice.tone} notice">
                <span>{notice.text}</span>
                {#if notice.path}
                    <button type="button" class="linkish" onclick={() => reveal(notice?.path ?? null)}>
                        Show in folder
                    </button>
                {/if}
            </div>
        {/if}

        {#if scan !== null && ofKind.length > 0}
            {#if fresh.length > 0}
                <section class="card">
                    <div class="card-header">
                        <h3>New on the card ({toDownload.length})</h3>
                        <span class="muted small">newest first</span>
                        <span class="spacer"></span>
                        {#if toDownload.length > 1}
                            <button
                                type="button"
                                class="btn btn-sm btn-ghost"
                                disabled={current !== null}
                                onclick={hideAllNew}
                            >
                                Hide all
                            </button>
                        {/if}
                    </div>
                    <table class="logs-table">
                        <tbody>
                            {#each fresh as f (f.index)}
                                {@const saved = savedNow[f.index]}
                                <tr class:row-active={current?.index === f.index}>
                                    <td class="mono">{f.name}</td>
                                    <td class="num mono">{formatBytes(f.size)}</td>
                                    <td class="muted small">
                                        {saved !== undefined ? 'saved ✓' : estimate(f.size)}
                                    </td>
                                    <td class="actions">
                                        {#if saved !== undefined}
                                            <button type="button" class="btn btn-sm" onclick={() => reveal(saved)}>
                                                Show
                                            </button>
                                        {:else}
                                            <button
                                                type="button"
                                                class="btn btn-sm"
                                                disabled={current !== null || root === null}
                                                onclick={() => download([f])}
                                            >
                                                Download
                                            </button>
                                            <button
                                                type="button"
                                                class="btn btn-sm btn-ghost"
                                                disabled={current?.index === f.index}
                                                title="Move to Hidden on this laptop. Nothing is deleted."
                                                onclick={() => mark('hide', [f])}
                                            >
                                                Hide
                                            </button>
                                        {/if}
                                    </td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                </section>
            {/if}

            {#if missing.length > 0}
                <section class="card card-missing">
                    <div class="card-header">
                        <h3>Missing on disk ({missing.length})</h3>
                        <span class="muted small">downloaded before, but the copy is gone or changed</span>
                    </div>
                    <table class="logs-table">
                        <tbody>
                            {#each missing as f (f.index)}
                                <tr>
                                    <td class="mono">{f.name}</td>
                                    <td class="muted small mono cell-path" title={f.path ?? ''}>was {f.path}</td>
                                    <td class="actions">
                                        <button
                                            type="button"
                                            class="btn btn-sm"
                                            disabled={current !== null || root === null}
                                            onclick={() => download([f])}
                                        >
                                            Download again
                                        </button>
                                        <button
                                            type="button"
                                            class="btn btn-sm btn-ghost"
                                            title="Stop tracking the old copy. The file shows as new."
                                            onclick={() => mark('forget', [f])}
                                        >
                                            Forget
                                        </button>
                                    </td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                </section>
            {/if}

            {#if downloaded.length > 0}
                <details class="card fold">
                    <summary>
                        <h3>Already downloaded ({downloaded.length})</h3>
                        <span class="muted small">on this laptop, at the listed size</span>
                    </summary>
                    <table class="logs-table">
                        <tbody>
                            {#each downloaded as f (f.index)}
                                <tr>
                                    <td class="mono">{f.name}</td>
                                    <td class="num mono">{formatBytes(f.size)}</td>
                                    <td class="muted small">{formatPulledAt(f.pulledAt)}</td>
                                    <td class="actions">
                                        <button type="button" class="btn btn-sm" onclick={() => reveal(f.csvPath ?? f.path)}>
                                            Show
                                        </button>
                                        <button
                                            type="button"
                                            class="btn btn-sm btn-ghost"
                                            disabled={current !== null || root === null}
                                            onclick={() => download([f])}
                                        >
                                            Download again
                                        </button>
                                    </td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                </details>
            {/if}

            {#if hidden.length > 0}
                <details class="card fold">
                    <summary>
                        <h3>Hidden ({hidden.length})</h3>
                        <span class="muted small">dismissed on this laptop — not necessarily downloaded</span>
                    </summary>
                    <div class="fold-actions">
                        <button
                            type="button"
                            class="btn btn-sm btn-ghost"
                            disabled={scanning || current !== null}
                            onclick={() => mark('unhide', hidden)}
                        >
                            Unhide all
                        </button>
                    </div>
                    <table class="logs-table">
                        <tbody>
                            {#each hidden as f (f.index)}
                                <tr>
                                    <td class="mono">{f.name}</td>
                                    <td class="num mono">{formatBytes(f.size)}</td>
                                    <td class="actions">
                                        <button
                                            type="button"
                                            class="btn btn-sm"
                                            disabled={scanning || current !== null}
                                            onclick={() => mark('unhide', [f])}
                                        >
                                            Unhide
                                        </button>
                                    </td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                </details>
            {/if}
        {/if}
    {/if}
</div>

<style>
    .setup {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
    }
    .setup-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
        font-size: var(--text-sm);
    }
    .label {
        color: var(--text-muted);
        min-width: 5.5rem;
    }
    .spacer {
        flex: 1;
    }
    .plain {
        border: none;
        margin: 0;
        padding: 0;
        min-width: 0;
    }
    .path {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        max-width: 32rem;
    }
    .check {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        color: var(--text-secondary);
    }

    /* File-type tabs — same segmented language as NodeIdRolePicker. */
    .segmented {
        display: inline-flex;
        gap: 2px;
        padding: 2px;
        background: var(--bg);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        align-self: flex-start;
    }
    .seg {
        appearance: none;
        border: none;
        background: transparent;
        color: var(--text-muted);
        font: inherit;
        font-size: var(--text-sm);
        padding: var(--space-1) var(--space-4);
        border-radius: calc(var(--radius-md) - 2px);
        cursor: pointer;
    }
    .seg:hover {
        color: var(--text);
    }
    .seg.active {
        background: var(--accent);
        color: var(--accent-contrast, #fff);
        font-weight: 600;
    }
    .seg-badge {
        margin-left: var(--space-2);
        font-size: var(--text-xs);
        opacity: 0.8;
    }

    .hero {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        border-color: var(--accent);
    }
    .hero p {
        margin: 0;
    }
    .hero-head {
        display: flex;
        align-items: baseline;
        gap: var(--space-3);
        flex-wrap: wrap;
    }
    .eyebrow {
        font-size: var(--text-xs);
        text-transform: uppercase;
        letter-spacing: 0.06em;
        color: var(--text-muted);
        font-weight: 600;
    }
    .hero-name {
        font-size: var(--text-xl);
        font-weight: 600;
    }
    .hero-actions {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
    }
    /* The one action this page exists for: the filled primary button,
       just bigger. Colours come from .btn-primary — overriding them here
       would put accent text on the accent fill. */
    .btn-hero {
        font-size: var(--text-lg);
        padding: var(--space-3) var(--space-5);
    }
    .badge {
        font-size: var(--text-xs);
        padding: 1px var(--space-2);
        border-radius: 999px;
        border: 1px solid currentColor;
    }
    .badge-new {
        color: var(--accent);
    }
    .badge-ok {
        color: var(--success);
    }
    .badge-warn {
        color: var(--warning);
    }
    .warn {
        color: var(--warning);
    }
    .progress-line {
        display: flex;
        gap: var(--space-4);
        flex-wrap: wrap;
    }
    .notice {
        display: flex;
        gap: var(--space-3);
        align-items: center;
    }

    .placeholder-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        align-items: flex-start;
    }
    .placeholder-card h3,
    .placeholder-card p {
        margin: 0;
    }
    .linkish {
        appearance: none;
        border: none;
        background: none;
        padding: 0;
        font: inherit;
        color: var(--accent);
        text-decoration: underline;
        cursor: pointer;
    }

    .card-header {
        justify-content: flex-start;
        gap: var(--space-3);
    }
    .card-missing {
        border-color: var(--warning);
    }
    .fold summary {
        display: flex;
        align-items: baseline;
        gap: var(--space-3);
        cursor: pointer;
        list-style: none;
    }
    .fold summary::-webkit-details-marker {
        display: none;
    }
    .fold summary::before {
        content: '▸';
        color: var(--text-muted);
    }
    .fold[open] summary::before {
        content: '▾';
    }
    .fold summary h3 {
        margin: 0;
        font-size: var(--text-base);
    }
    .fold[open] summary {
        margin-bottom: var(--space-2);
    }
    .fold-actions {
        display: flex;
        justify-content: flex-end;
    }

    .logs-table {
        width: 100%;
        border-collapse: collapse;
        font-size: var(--text-sm);
    }
    .logs-table td {
        padding: var(--space-2) var(--space-3);
        border-bottom: 1px solid var(--border);
    }
    .logs-table tr:last-child td {
        border-bottom: none;
    }
    /* Long paths truncate instead of pushing the buttons off-screen;
       the full path is in the tooltip. */
    .logs-table .cell-path {
        max-width: 0;
        width: 100%;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }
    .logs-table .num {
        text-align: right;
    }
    .logs-table .actions {
        text-align: right;
        white-space: nowrap;
    }
    .logs-table .actions .btn + .btn {
        margin-left: var(--space-2);
    }
    .row-active td {
        background: var(--accent-soft);
    }

    /* Left-origin progress bar, reused from the pedal meters. */
    .meter {
        position: relative;
        height: 10px;
        border-radius: var(--radius-sm, 4px);
        background: var(--bg);
        border: 1px solid var(--border);
        overflow: hidden;
    }
    .meter-fill {
        height: 100%;
        background: var(--accent);
        transition: width 120ms linear;
    }
</style>
