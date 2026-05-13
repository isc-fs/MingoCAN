// App-wide reactive state, using Svelte 5 runes.
//
// Two pieces of state to start:
//
//   - `activeView` — which sidebar entry is showing in the main area.
//   - `selectedAdapter` — the (interface, channel) pair the app is
//     currently scoped to. Tier 0b's flash command will read this;
//     Tier 0c's diagnostics commands too.
//
// Both are deliberately session-only for now. Tier 1.5 (or sooner)
// adds disk persistence via `@tauri-apps/plugin-store` so a restart
// remembers the operator's last choice.

import type { AdapterEntry } from './types';

export type ViewId = 'adapters' | 'flash' | 'diagnostics' | 'liveData';

export const VIEWS: { id: ViewId; label: string; status: 'live' | 'soon' }[] = [
    { id: 'adapters', label: 'Adapters', status: 'live' },
    { id: 'flash', label: 'Flash', status: 'live' },
    { id: 'diagnostics', label: 'Diagnostics', status: 'live' },
    { id: 'liveData', label: 'Live data', status: 'soon' },
];

// Svelte 5 runes can't live at module scope in a `.ts` file (runes
// only work inside `.svelte.ts` or `.svelte` files), so we expose
// plain mutable objects and let the consumers use `$state` /
// `$derived` if they need reactivity around them.
//
// The convention: each store exports a `getX()` reader and
// `setX(value)` setter. Components that need reactivity wrap a
// `$state` around a snapshot, or import `appState` directly from
// inside a `.svelte.ts` module that elevates these to runes.
//
// For Tier 0a the consumers (App.svelte, AdaptersView.svelte) keep
// their own local `$state` and pass things via props — keeps the
// dependency graph obvious. We'll graduate to a proper rune-based
// store the first time three views need to share something.

export interface AppState {
    activeView: ViewId;
    selectedAdapter: AdapterEntry | null;
}

export function defaultAppState(): AppState {
    return {
        activeView: 'adapters',
        selectedAdapter: null,
    };
}
