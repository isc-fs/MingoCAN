// Parse `CMakePresets.json` at the workspace root and synthesise
// the one-liner build command that matches it. STM32CubeMX-
// generated CMake projects ship a `CMakePresets.json` that pins
// the arm-none-eabi toolchain file, the generator, and the
// `binaryDir` — `cmake -B build -S .` without `--preset` ignores
// all of that and either errors out or builds for the host. This
// detector lets the Flash button "just work" on those projects.
//
// Strategy:
//
//   - If `CMakePresets.json` exists, pick the first
//     `buildPresets[]` entry (or the first `configurePresets[]`
//     entry if there are no build presets) and synthesise:
//       cmake --preset <configurePreset> && cmake --build --preset <buildPreset>
//     Falls back to `cmake --build build/<configurePreset>` when
//     the file has configure-only presets.
//   - When no presets file exists, return `null` so the caller
//     uses its own default (generic `cmake -B build -S . &&
//     cmake --build build`).
//
// Mirrors the same shape the Studio app's `read_cmake_presets`
// Tauri command implements — see `apps/can-studio/src-tauri/src/flash.rs`.
//
// Spec reference: https://cmake.org/cmake/help/latest/manual/cmake-presets.7.html

import { readFileSync, existsSync } from 'fs';
import { join } from 'path';

export interface DetectedPreset {
    /** Best build-command synthesis for this preset. */
    command: string;
    /**
     * Glob-friendly hint for `iscFs.firmwareArtifact` derived from
     * `configurePresets[].binaryDir`. Null when we can't recover
     * the path (e.g. `binaryDir` uses unsupported variables).
     */
    artifactGlobHint: string | null;
}

/**
 * Detect a CMakePresets.json at the workspace root and return the
 * preset-aware build command we should use, or `null` to fall back
 * to the generic default.
 */
export function detectCmakePreset(cwd: string): DetectedPreset | null {
    const path = join(cwd, 'CMakePresets.json');
    if (!existsSync(path)) {
        return null;
    }
    let raw: string;
    try {
        raw = readFileSync(path, 'utf8');
    } catch {
        return null;
    }
    let json: unknown;
    try {
        json = JSON.parse(raw);
    } catch {
        // CMakePresets.json is mandated to be strict JSON, but if
        // a project's file is broken we'd rather fall back than fail.
        return null;
    }
    if (typeof json !== 'object' || json === null) {
        return null;
    }
    const obj = json as Record<string, unknown>;

    const configurePresets = arrayOfObjects(obj.configurePresets);
    const buildPresets = arrayOfObjects(obj.buildPresets);

    // Prefer a build preset if one's declared. CMake's contract is
    // that buildPresets[].configurePreset references a configurePresets
    // entry, so picking the first one gives us both halves of the
    // build command.
    if (buildPresets.length > 0) {
        const buildPreset = buildPresets[0];
        const buildName = stringOr(buildPreset.name, null);
        const configureName = stringOr(buildPreset.configurePreset, null);
        if (buildName !== null && configureName !== null) {
            return {
                command: `cmake --preset ${shellQuote(configureName)} && cmake --build --preset ${shellQuote(buildName)}`,
                artifactGlobHint: artifactHintFor(configureName, configurePresets),
            };
        }
    }

    // No build preset — fall back to the first configure preset.
    if (configurePresets.length > 0) {
        const cp = configurePresets[0];
        const configureName = stringOr(cp.name, null);
        if (configureName !== null) {
            // Recover binaryDir to drive the build step's path.
            const binaryDir = resolveBinaryDir(configureName, configurePresets);
            const buildPath =
                binaryDir !== null
                    ? `build/${configureName}` // mirrored when binaryDir uses ${presetName}
                    : 'build';
            return {
                command: `cmake --preset ${shellQuote(configureName)} && cmake --build ${shellQuote(buildPath)}`,
                artifactGlobHint: artifactHintFor(configureName, configurePresets),
            };
        }
    }

    return null;
}

// ---- helpers ----

function arrayOfObjects(value: unknown): Record<string, unknown>[] {
    if (!Array.isArray(value)) return [];
    return value.filter(
        (v): v is Record<string, unknown> => typeof v === 'object' && v !== null,
    );
}

function stringOr(value: unknown, fallback: string | null): string | null {
    return typeof value === 'string' && value.length > 0 ? value : fallback;
}

/**
 * Synthesise a workspace-relative glob that's likely to match the
 * preset's output. We only support the common
 * `${sourceDir}/build/${presetName}` pattern; anything fancier
 * returns null and the caller falls back to the default firmware
 * glob declared by `DEFAULT_FIRMWARE_GLOB` in `config.ts`.
 */
function artifactHintFor(
    configureName: string,
    configurePresets: Record<string, unknown>[],
): string | null {
    const binaryDir = resolveBinaryDir(configureName, configurePresets);
    if (binaryDir === null) return null;
    // Substitute the two variables we recognise; bail on anything else.
    let expanded = binaryDir.replace(/\$\{presetName\}/g, configureName);
    if (expanded.startsWith('${sourceDir}/')) {
        expanded = expanded.slice('${sourceDir}/'.length);
    } else if (expanded.includes('${sourceDir}')) {
        // sourceDir referenced somewhere weird; bail.
        return null;
    }
    if (expanded.includes('${')) return null; // unsupported variable
    return `${expanded}/**/*.{elf,hex,bin}`;
}

/**
 * Walk the `inherits` chain to recover the binaryDir for a configure
 * preset. Returns the raw string (still containing `${presetName}`
 * etc.) or null if not present anywhere in the inheritance chain.
 */
function resolveBinaryDir(
    name: string,
    configurePresets: Record<string, unknown>[],
): string | null {
    const byName = new Map<string, Record<string, unknown>>();
    for (const p of configurePresets) {
        const n = stringOr(p.name, null);
        if (n !== null) byName.set(n, p);
    }
    const seen = new Set<string>();
    let current: string | null = name;
    while (current !== null && !seen.has(current)) {
        seen.add(current);
        const preset = byName.get(current);
        if (preset === undefined) return null;
        const dir = stringOr(preset.binaryDir, null);
        if (dir !== null) return dir;
        // Walk inherits (string or array of strings); first hit wins.
        const inherits = preset.inherits;
        if (typeof inherits === 'string') {
            current = inherits;
        } else if (Array.isArray(inherits) && typeof inherits[0] === 'string') {
            current = inherits[0];
        } else {
            current = null;
        }
    }
    return null;
}

/**
 * Conservative shell quoter for the synthesised command. Preset
 * names are usually plain identifiers (`Debug`, `Release`), but
 * the spec doesn't forbid spaces or punctuation — wrap anything
 * non-trivial in single quotes.
 */
function shellQuote(s: string): string {
    if (/^[A-Za-z0-9_./@:+=-]+$/.test(s)) return s;
    return `'${s.replace(/'/g, "'\\''")}'`;
}
