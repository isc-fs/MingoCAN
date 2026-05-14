// Vite configuration for the ISC CAN Studio frontend.
//
// Tauri 2 launches `vite` as a child process during `tauri dev` and
// expects the dev server on port 5173 with HMR working. For
// production builds, Tauri sets TAURI_PLATFORM env vars before
// invoking `npm run build`; the relevant Vite knobs below honour
// them so the bundle targets the right platform's WebView.

import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async () => ({
    plugins: [svelte()],
    // Prevent Vite from obscuring rust-side errors in the terminal.
    clearScreen: false,
    server: {
        port: 5173,
        strictPort: true,
        host: host !== undefined ? host : false,
        hmr:
            host !== undefined
                ? { protocol: 'ws', host, port: 5174 }
                : undefined,
        // Tauri's dev mode polls these — leaving them on the
        // defaults means OK on Mac/Linux/Windows.
        watch: { ignored: ['**/src-tauri/**'] },
    },
    // Tauri WebView targets are roughly:
    //   - Edge WebView2 on Windows (modern Chromium)
    //   - WKWebView on macOS (Safari)
    //   - WebKitGTK on Linux (Safari-ish)
    // The intersection is essentially ES2022; we set it explicitly so
    // accidental top-level-await regressions don't slip through.
    build: {
        target:
            process.env.TAURI_PLATFORM === 'windows'
                ? 'chrome105'
                : 'safari14',
        // Don't minify for debug builds — keeps stack traces useful
        // when the WebView surfaces a runtime error.
        minify: process.env.TAURI_DEBUG === 'true' ? false : 'esbuild',
        sourcemap: process.env.TAURI_DEBUG === 'true',
        outDir: 'dist',
        emptyOutDir: true,
    },
}));
