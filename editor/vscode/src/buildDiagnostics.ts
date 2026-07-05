// Compile errors → Problems panel.
//
// `runBuildStep` streams the build's stdout/stderr to the output
// channel, where compile errors scroll past unclickable. This module
// parses gcc/clang/arm-none-eabi diagnostics out of that same stream and
// publishes them to a `DiagnosticCollection`, so they land in the
// Problems panel with go-to-source — without swapping the build over to a
// `vscode.Task` (which would give up the streaming + cancellation the
// flash pipeline relies on).

import * as path from 'path';
import * as vscode from 'vscode';

let collection: vscode.DiagnosticCollection | undefined;

/** Create the shared collection and tie its lifetime to the extension.
 *  Called once from `activate`. */
export function initBuildDiagnostics(context: vscode.ExtensionContext): void {
    if (collection === undefined) {
        collection = vscode.languages.createDiagnosticCollection('iscFs-build');
        context.subscriptions.push(collection);
    }
}

/** Drop all build diagnostics — called at the start of every build so a
 *  clean run clears the previous run's errors. No-op before init. */
export function clearBuildDiagnostics(): void {
    collection?.clear();
}

// gcc / clang / arm-none-eabi-gcc diagnostic line:
//   src/main.c:42:5: error: 'foo' undeclared (first use in this function)
//   src/main.c:42: warning: ...            (column optional)
// The leading path may be relative (to the build cwd) or absolute.
const DIAG_RE =
    /^\s*(.+?):(\d+)(?::(\d+))?:\s*(fatal error|error|warning|note|info):\s*(.*)$/;

interface ParsedDiag {
    file: string;
    line: number;
    col: number | null;
    severity: vscode.DiagnosticSeverity;
    message: string;
}

function severityOf(kind: string): vscode.DiagnosticSeverity {
    switch (kind) {
        case 'fatal error':
        case 'error':
            return vscode.DiagnosticSeverity.Error;
        case 'warning':
            return vscode.DiagnosticSeverity.Warning;
        default:
            return vscode.DiagnosticSeverity.Information;
    }
}

/** Parse one build-output line into a diagnostic, or `null` if it isn't
 *  a compiler diagnostic. Exported for unit testing. */
export function parseBuildLine(line: string): ParsedDiag | null {
    const m = DIAG_RE.exec(line);
    if (m === null) {
        return null;
    }
    const [, file, lineStr, colStr, kind, message] = m;
    const lineNo = Number.parseInt(lineStr, 10);
    if (!Number.isFinite(lineNo)) {
        return null;
    }
    // Guard against false positives like "note: In file included from
    // x:1" where the "file" is really prose — require the captured path
    // to look like a filename (has an extension or a path separator).
    if (!/[/\\]/.test(file) && !/\.\w+$/.test(file)) {
        return null;
    }
    return {
        file,
        line: lineNo,
        col: colStr !== undefined ? Number.parseInt(colStr, 10) : null,
        severity: severityOf(kind),
        message,
    };
}

/**
 * Parse the full set of build-output lines and publish per-file
 * diagnostics to the Problems panel. Relative paths resolve against
 * `cwd` (the build working directory). Returns the number of error-level
 * diagnostics, so the caller can tailor the failure toast ("build failed
 * — 3 errors in Problems"). No-op before init.
 */
export function publishBuildDiagnostics(lines: readonly string[], cwd: string): number {
    if (collection === undefined) {
        return 0;
    }
    const byFile = new Map<string, vscode.Diagnostic[]>();
    let errorCount = 0;

    for (const raw of lines) {
        const parsed = parseBuildLine(raw);
        if (parsed === null) {
            continue;
        }
        const abs = path.isAbsolute(parsed.file)
            ? parsed.file
            : path.resolve(cwd, parsed.file);
        // Editor lines/cols are 0-based; compilers report 1-based.
        const zeroLine = Math.max(0, parsed.line - 1);
        const zeroCol = parsed.col !== null ? Math.max(0, parsed.col - 1) : 0;
        const range = new vscode.Range(zeroLine, zeroCol, zeroLine, Number.MAX_SAFE_INTEGER);
        const diag = new vscode.Diagnostic(range, parsed.message, parsed.severity);
        diag.source = 'build';
        if (parsed.severity === vscode.DiagnosticSeverity.Error) {
            errorCount += 1;
        }
        const list = byFile.get(abs);
        if (list === undefined) {
            byFile.set(abs, [diag]);
        } else {
            list.push(diag);
        }
    }

    collection.clear();
    for (const [file, diags] of byFile) {
        collection.set(vscode.Uri.file(file), diags);
    }
    return errorCount;
}
