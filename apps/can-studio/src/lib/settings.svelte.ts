// Persistent settings store, backed by `@tauri-apps/plugin-store`.
//
// Module-scope Svelte 5 reactivity: every view imports the `settings`
// object directly, reads properties, and writes back. A debounced
// $effect catches every change and flushes to disk. No prop drilling.
//
// Disk file lives at the OS-specific app-config dir (resolved by the
// Tauri plugin — `~/Library/Application Support/com.iscracingteam.can-studio/`
// on macOS, `%APPDATA%/com.iscracingteam.can-studio/` on Windows,
// `~/.config/com.iscracingteam.can-studio/` on Linux).

import { load, type Store } from '@tauri-apps/plugin-store';

import type { InterfaceType } from './types';

// ---- Schema ----

export interface Settings {
    adapter: AdapterSettings;
    flash: FlashSettings;
    liveData: LiveDataSettings;
}

export interface AdapterSettings {
    /** `null` means "no adapter selected yet". */
    interface: InterfaceType | null;
    channel: string;
    /** Cosmetic label cached from the picker so the active row in
     *  the Adapters view can show "VN1610 Channel 1" instead of
     *  just the channel index after a restart. */
    label: string;
    bitrate: number;
    /** 0..=15 (4-bit). `null` means broadcast. */
    nodeId: number | null;
    timeoutMs: number;
}

export interface FlashSettings {
    artifactPath: string;
    buildCommand: string;
    buildCwd: string;
    diff: boolean;
    dryRun: boolean;
    verifyAfter: boolean;
    finalCommit: boolean;
    jump: boolean;
}

export interface LiveDataSettings {
    rateHz: number;
    windowSeconds: number;
}

// ---- Defaults ----

export function defaultSettings(): Settings {
    return {
        adapter: {
            interface: null,
            channel: '',
            label: '',
            bitrate: 500_000,
            nodeId: 0x3,
            timeoutMs: 500,
        },
        flash: {
            artifactPath: '',
            buildCommand: 'cmake --build build',
            buildCwd: '',
            diff: true,
            dryRun: false,
            verifyAfter: true,
            finalCommit: true,
            jump: true,
        },
        liveData: {
            rateHz: 10,
            windowSeconds: 60,
        },
    };
}

// ---- Reactive state + persistence ----

/** Single source of truth. Mutate fields directly; changes auto-save. */
export const settings = $state<Settings>(defaultSettings());

const STORE_FILE = 'settings.json';
const STORE_KEY = 'all';
const SAVE_DEBOUNCE_MS = 250;

let store: Store | null = null;
let loaded = false;
let saveTimer: ReturnType<typeof setTimeout> | null = null;

/**
 * Load settings from disk into the reactive `settings` object.
 * Idempotent — safe to call multiple times; subsequent calls no-op.
 * Returns the same Promise across concurrent invocations.
 */
let loadPromise: Promise<void> | null = null;
export function loadSettings(): Promise<void> {
    if (loadPromise !== null) return loadPromise;
    loadPromise = (async () => {
        store = await load(STORE_FILE);
        const stored = await store.get<Partial<Settings>>(STORE_KEY);
        if (stored !== undefined && stored !== null) {
            mergeInto(settings, stored);
        }
        loaded = true;
    })();
    return loadPromise;
}

/**
 * Flush `settings` to disk. Called by the autosave effect after a
 * debounce; can also be invoked explicitly (e.g. just before exit).
 */
export async function saveSettings(): Promise<void> {
    if (!loaded || store === null) return;
    await store.set(STORE_KEY, settings);
    await store.save();
}

function scheduleSave(): void {
    if (!loaded) return;
    if (saveTimer !== null) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
        saveTimer = null;
        void saveSettings();
    }, SAVE_DEBOUNCE_MS);
}

/**
 * Wire up an effect at the app root that observes every field on
 * the settings object and triggers a debounced save. Must be called
 * inside a Svelte effect context (e.g. App.svelte's onMount).
 */
export function registerAutosaveEffect(): void {
    $effect(() => {
        // Touch every leaf so Svelte 5 tracks them as dependencies.
        // JSON.stringify visits every reactive property, marking them
        // as read, so any subsequent mutation re-triggers the effect.
        JSON.stringify(settings);
        scheduleSave();
    });
}

// ---- Helpers ----

/**
 * Recursively assign known fields from `partial` into `target`. We
 * never want a stored value's missing key to clobber a fresh default;
 * a stale schema on disk shouldn't break a newer Studio version.
 */
function mergeInto<T extends object>(target: T, source: Partial<T>): void {
    for (const key in source) {
        const incoming = source[key];
        if (incoming === undefined || incoming === null) continue;
        const current = (target as Record<string, unknown>)[key];
        if (
            typeof incoming === 'object' &&
            !Array.isArray(incoming) &&
            typeof current === 'object' &&
            current !== null &&
            !Array.isArray(current)
        ) {
            mergeInto(current as object, incoming as object);
        } else {
            (target as Record<string, unknown>)[key] = incoming;
        }
    }
}
