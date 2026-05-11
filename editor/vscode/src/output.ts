// Single shared output channel for every shell-out and event log
// the extension produces. Visible to operators under
// `View → Output → ISC CAN`.
//
// Every operation that touches hardware (build, flash, discover, …)
// writes both the argv it ran and the stdout/stderr it observed,
// uninterpreted, to this channel. The point is a deterministic
// record at hardware-test time when a button-click misbehaves and
// the operator needs to know exactly what was executed.

import * as vscode from 'vscode';

let channel: vscode.OutputChannel | undefined;

export function getOutputChannel(): vscode.OutputChannel {
    if (channel === undefined) {
        channel = vscode.window.createOutputChannel('ISC CAN');
    }
    return channel;
}

/**
 * Reveal the channel without stealing focus from the active editor.
 * Used at the start of long-running operations so operators can
 * watch progress without losing their place.
 */
export function showOutputChannel(): void {
    getOutputChannel().show(true);
}
