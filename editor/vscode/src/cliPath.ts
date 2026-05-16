// Resolves `can-flasher`'s absolute path on the host so the
// extension keeps working when VS Code is launched from
// Finder / Dock / launchd — environments where `PATH` rarely
// includes the user-local install dirs (`~/.local/bin`,
// `~/.cargo/bin`, etc.) that a terminal shell would.
//
// The default `iscFs.canFlasherPath` setting is the bare string
// `"can-flasher"`, which `child_process.spawn` resolves via the
// inherited PATH. On macOS that PATH is launchd's reduced one
// (`/usr/bin:/bin:/usr/sbin:/sbin`) unless VS Code was launched
// from the command line, which is the minority case for most
// operators. The fallback is a quick existsSync probe of a
// curated list of well-known install locations.
//
// Three precedence rules:
//
//  1. **Operator customised the setting** — they typed an absolute
//     path or a custom binary name. Honour it verbatim, even if
//     it doesn't exist (so the failure message points at *their*
//     path rather than a different one we found).
//  2. **Default setting + well-known path exists** — return the
//     first hit, ordered by how operators usually install it.
//  3. **Nothing found** — return the bare `"can-flasher"` so
//     spawn fails with ENOENT and the UX layer can surface a
//     helpful "install the CLI" prompt.

import { existsSync, statSync } from 'fs';
import { homedir, platform } from 'os';
import { join } from 'path';

/** The default value of `iscFs.canFlasherPath` in package.json. */
export const DEFAULT_BARE_NAME = 'can-flasher';

/**
 * Module-scoped cache so we don't re-probe the filesystem on
 * every command invocation. The first call after a settings
 * change re-probes (callers pass the fresh config value).
 */
let cached: { configured: string; resolved: string } | null = null;

/**
 * Pick the binary path to hand to `spawn`. See module-level
 * comment for the precedence rules.
 */
export function resolveCanFlasherPath(configured: string): string {
    if (cached !== null && cached.configured === configured) {
        return cached.resolved;
    }
    const resolved = resolveUncached(configured);
    cached = { configured, resolved };
    return resolved;
}

/**
 * Reset the cache. Call this when the operator changes the
 * `iscFs.canFlasherPath` setting so the next spawn sees the
 * fresh value rather than a stale resolution.
 */
export function clearCanFlasherPathCache(): void {
    cached = null;
}

function resolveUncached(configured: string): string {
    // Rule 1: operator pinned a custom value. An absolute path,
    // a path containing a separator, or a basename different
    // from our default — all signal intent.
    if (configured !== DEFAULT_BARE_NAME) {
        return configured;
    }

    // Rule 2: probe well-known paths in order of likelihood.
    const exe = exeName();
    for (const dir of wellKnownDirs()) {
        const full = join(dir, exe);
        if (isExecutableFile(full)) {
            return full;
        }
    }

    // Rule 3: nothing found. Let spawn ENOENT.
    return configured;
}

function exeName(): string {
    return platform() === 'win32' ? 'can-flasher.exe' : 'can-flasher';
}

/**
 * Ordered list of directories that commonly hold `can-flasher`
 * on each platform. Probed in order; first hit wins.
 *
 * macOS / Linux ordering rationale:
 * - `/usr/local/bin` first because it's the canonical "system-
 *   wide install" on macOS, and it's also in launchd's default
 *   PATH (so the extension would already find it without us).
 *   Listing it here protects against operators who launched
 *   VS Code from a shell that *unset* `/usr/local/bin`.
 * - `/opt/homebrew/bin` for Apple Silicon Homebrew installs.
 * - `~/.local/bin` is the [XDG-recommended](https://specifications.freedesktop.org/basedir-spec/basedir-spec-latest.html)
 *   user-local prefix, used by `pipx`, manual installs, etc.
 * - `~/.cargo/bin` for `cargo install --git ...` users.
 *
 * Windows: the release pipeline produces a self-contained zip
 * the operator extracts manually, so we check a couple of
 * conventional spots people land it.
 */
function wellKnownDirs(): string[] {
    const home = homedir();
    if (platform() === 'win32') {
        const localAppData = process.env.LOCALAPPDATA ?? join(home, 'AppData', 'Local');
        return [
            join(localAppData, 'Programs', 'can-flasher'),
            'C:\\Program Files\\can-flasher',
            join(home, '.cargo', 'bin'),
        ];
    }
    // macOS + Linux share the same install conventions.
    return [
        '/usr/local/bin',
        '/opt/homebrew/bin',
        join(home, '.local', 'bin'),
        join(home, '.cargo', 'bin'),
    ];
}

function isExecutableFile(path: string): boolean {
    try {
        // `existsSync` follows symlinks; that's what we want — a
        // package-manager-managed symlink into a Cellar should
        // resolve and count as found.
        if (!existsSync(path)) return false;
        const s = statSync(path);
        return s.isFile();
    } catch {
        return false;
    }
}
