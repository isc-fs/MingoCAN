// Version-matched `can-flasher` binary management.
//
// The extension ships at the SAME version as the can-flasher CLI —
// the release pipeline's `verify-version` gate enforces lockstep
// across all five source files. But the extension shells out to a
// can-flasher *binary*, and historically trusted whatever was on the
// host PATH. That drifts: an operator running the desktop app on
// v2.5.2 could still have a v2.3.0 CLI on PATH, and the extension
// would silently use it — hitting bugs the CLI fixed releases ago
// (e.g. the ELF load-address fix that made flashing an .elf fail with
// a "segment extends past BL_APP_END" sector error).
//
// This module closes that gap two ways:
//   1. download-on-demand — fetch the CLI binary matching THIS
//      extension's version from the matching GitHub release, cache it
//      under the extension's global storage, and prefer it over PATH
//      (see `setManagedCliPath` in cliPath.ts).
//   2. skew detection — `cliVersion()` lets the activation path warn
//      when the resolved binary's version disagrees with ours; this
//      covers operator-pinned paths and the offline fallback to PATH.
//
// Pure Node built-ins + the platform `tar` — no npm runtime deps, so
// the VSIX stays lean (`vsce package --no-dependencies`).

import { execFile, spawn } from 'child_process';
import {
    chmodSync,
    copyFileSync,
    createWriteStream,
    existsSync,
    mkdirSync,
    rmSync,
} from 'fs';
import { get as httpsGet } from 'https';
import { join } from 'path';
import * as vscode from 'vscode';

import { getOutputChannel } from './output';

const REPO = 'isc-fs/can-flasher';

/**
 * Map the host to the release target triple + archive extension.
 * Returns `null` for platforms the release pipeline doesn't build
 * (e.g. Intel macOS), where the caller falls back to PATH + a skew
 * warning rather than a confusing 404.
 */
function hostTarget(): { triple: string; ext: 'tar.gz' | 'zip' } | null {
    const p = process.platform;
    const a = process.arch;
    if (p === 'darwin' && a === 'arm64') {
        return { triple: 'aarch64-apple-darwin', ext: 'tar.gz' };
    }
    if (p === 'linux' && a === 'x64') {
        return { triple: 'x86_64-unknown-linux-gnu', ext: 'tar.gz' };
    }
    if (p === 'linux' && a === 'arm64') {
        return { triple: 'aarch64-unknown-linux-gnu', ext: 'tar.gz' };
    }
    if (p === 'win32' && a === 'x64') {
        return { triple: 'x86_64-pc-windows-msvc', ext: 'zip' };
    }
    return null;
}

function exeName(): string {
    return process.platform === 'win32' ? 'can-flasher.exe' : 'can-flasher';
}

function managedDir(context: vscode.ExtensionContext, version: string): string {
    return join(context.globalStorageUri.fsPath, 'cli', version);
}

/** Absolute path where a version-matched managed binary would live. */
export function managedBinaryPath(
    context: vscode.ExtensionContext,
    version: string,
): string {
    return join(managedDir(context, version), exeName());
}

/**
 * Read `<cliPath> --version` → e.g. `"2.5.2"`, or `null` if it can't
 * run or the output doesn't parse. Used for skew detection.
 */
export function cliVersion(cliPath: string): Promise<string | null> {
    return new Promise((resolve) => {
        execFile(cliPath, ['--version'], { timeout: 5_000 }, (err, stdout) => {
            if (err) {
                resolve(null);
                return;
            }
            const m = /(\d+\.\d+\.\d+)/.exec(stdout);
            resolve(m === null ? null : m[1]);
        });
    });
}

/**
 * Ensure a `can-flasher` binary whose version == `version` exists in
 * the extension's managed storage, downloading + extracting it from
 * the matching GitHub release if missing. Returns the absolute path,
 * or `null` if the platform is unsupported / the download or
 * extraction failed (caller falls back to PATH).
 *
 * Idempotent: a cached binary from a previous session short-circuits
 * the download.
 */
export async function ensureManagedCli(
    context: vscode.ExtensionContext,
    version: string,
): Promise<string | null> {
    const out = getOutputChannel();
    const dest = managedBinaryPath(context, version);
    if (existsSync(dest)) {
        return dest;
    }

    const target = hostTarget();
    if (target === null) {
        out.appendLine(
            `[cli] no prebuilt can-flasher for ${process.platform}/${process.arch}; using PATH`,
        );
        return null;
    }

    // Archives are named with the `v` prefix because the release
    // pipeline stages them from `github.ref_name` (e.g. `v2.5.3`).
    const stem = `can-flasher-v${version}-${target.triple}`;
    const asset = `${stem}.${target.ext}`;
    const url = `https://github.com/${REPO}/releases/download/v${version}/${asset}`;
    const dir = managedDir(context, version);
    mkdirSync(dir, { recursive: true });
    const archivePath = join(dir, asset);

    try {
        out.appendLine(`[cli] downloading ${url}`);
        await download(url, archivePath);
        out.appendLine(`[cli] extracting ${asset}`);
        await extract(archivePath, dir);
        // The archive stages the binary under `<stem>/<exe>`; hoist it
        // to `<dir>/<exe>` so the cached path is stable.
        const staged = join(dir, stem, exeName());
        if (!existsSync(staged)) {
            out.appendLine(`[cli] extracted archive missing ${staged}`);
            return null;
        }
        copyFileSync(staged, dest);
        if (process.platform !== 'win32') {
            chmodSync(dest, 0o755);
        }
        // Best-effort cleanup of the archive + staging dir.
        try {
            rmSync(archivePath, { force: true });
            rmSync(join(dir, stem), { recursive: true, force: true });
        } catch {
            // Leftover scratch files are harmless.
        }
        out.appendLine(`[cli] managed can-flasher ${version} ready at ${dest}`);
        return dest;
    } catch (err) {
        out.appendLine(
            `[cli] download/extract failed: ${err instanceof Error ? err.message : String(err)}`,
        );
        // Don't leave a half-written binary that would look valid.
        try {
            rmSync(dest, { force: true });
        } catch {
            // ignore
        }
        return null;
    }
}

/**
 * HTTPS GET to `dest`, following redirects (GitHub release-asset URLs
 * 302 to a CDN host). Rejects on non-200 terminal status or transport
 * error. Uses the `https` built-in to avoid any dependency on a global
 * `fetch` (whose availability varies across the VS Code Node runtime)
 * and to keep types unambiguous under `lib: ["ES2022"]`.
 */
function download(url: string, dest: string, redirects = 5): Promise<void> {
    return new Promise((resolve, reject) => {
        if (redirects < 0) {
            reject(new Error('too many redirects'));
            return;
        }
        const req = httpsGet(
            url,
            { headers: { 'User-Agent': 'isc-mingocan-vscode' } },
            (res) => {
                const status = res.statusCode ?? 0;
                if (status >= 300 && status < 400 && res.headers.location) {
                    res.resume();
                    download(res.headers.location, dest, redirects - 1).then(
                        resolve,
                        reject,
                    );
                    return;
                }
                if (status !== 200) {
                    res.resume();
                    reject(new Error(`HTTP ${status} for ${url}`));
                    return;
                }
                const file = createWriteStream(dest);
                res.pipe(file);
                file.on('finish', () => file.close((err) => (err ? reject(err) : resolve())));
                file.on('error', reject);
            },
        );
        req.on('error', reject);
    });
}

/**
 * Extract a `.tar.gz` or `.zip` via the platform `tar`. bsdtar ships
 * on macOS, Linux, and Windows 10+ and handles both formats with
 * `-xf`, so we avoid bundling an archive library.
 */
function extract(archivePath: string, destDir: string): Promise<void> {
    return new Promise((resolve, reject) => {
        const child = spawn('tar', ['-xf', archivePath, '-C', destDir], {
            stdio: 'ignore',
        });
        child.on('error', reject);
        child.on('close', (code) =>
            code === 0 ? resolve() : reject(new Error(`tar exited with code ${code}`)),
        );
    });
}
