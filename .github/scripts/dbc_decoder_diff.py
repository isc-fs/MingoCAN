#!/usr/bin/env python3
"""Compare the decoder-relevant subset of two ECU DBC files.

`ecu-dbc-drift.yml` needs to answer one question precisely: does an
upstream DBC change affect `src/pit_diag/ecu.rs`, or is the vendored
snapshot merely stale?

The first cut of that check grepped the unified diff for lines matching
`PitDiag_`. That silently answered "no" for the most common and most
dangerous case — a signal added to a message we already decode. When
`0x704 PitDiag_health` gained `cal_status`, the changed line was

    + SG_ cal_status : 44|2@1+ (1,0) [0|0] "enum" Vector__XXX

which contains no `PitDiag_` substring at all; the `BO_ 1796
PitDiag_health` header was unchanged context. The grep found zero
matches and the drift issue told its reader the decoder was unaffected,
when in fact the conformance test was about to fail on it.

So parse instead of grep. This extracts exactly what
`tests/ecu_dbc_conformance.rs` asserts — the `PitDiag_*` messages, their
IDs and DLCs, every signal's bit layout, and the `VAL_` enum tables
attached to them — and compares those structures. Anything outside that
subset (`VCU_*`, `ACU_*`, the new `PitCal_*` frames) is real drift worth
re-vendoring for, but cannot break the decoder, and is reported as such.

Usage:
    dbc_decoder_diff.py --vendored A.dbc --upstream B.dbc [--summary-out F]

Prints `affects_decoder=true|false` on stdout. With --summary-out,
writes a markdown bullet list of the decoder-relevant changes.
"""
from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass, field

# `BO_ <id> <Name>: <dlc> <transmitter>`
BO_RE = re.compile(r"^BO_\s+(\d+)\s+([A-Za-z_0-9]+)\s*:\s*(\d+)\s+(\S+)")
# ` SG_ <name> : <start>|<size>@<order><sign> (<factor>,<offset>) [min|max] "<unit>" <recv>`
SG_RE = re.compile(
    r"^\s*SG_\s+([A-Za-z_0-9]+)\s*:\s*(\d+)\|(\d+)@([01])([+-])"
)
# `VAL_ <msg id> <signal> <val> "<name>" ... ;`
VAL_RE = re.compile(r'^VAL_\s+(\d+)\s+([A-Za-z_0-9]+)\s+(.*?);\s*$')
VAL_PAIR_RE = re.compile(r'(-?\d+)\s+"([^"]*)"')

# The prefix that marks a message as part of the decoder's contract.
DECODER_PREFIX = "PitDiag_"


@dataclass
class Message:
    name: str
    msg_id: int
    dlc: int
    # signal name -> (start_bit, size, big_endian, signed)
    signals: dict[str, tuple[int, int, bool, bool]] = field(default_factory=dict)
    # signal name -> {raw value: label}
    vals: dict[str, dict[int, str]] = field(default_factory=dict)


def parse(path: str) -> dict[str, Message]:
    """Parse a DBC into {message name: Message}. Tolerant by design —
    anything it cannot recognise is skipped rather than raising, because
    a parse error here would fail the drift job for an unrelated reason."""
    messages: dict[str, Message] = {}
    by_id: dict[int, Message] = {}
    current: Message | None = None

    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            bo = BO_RE.match(line)
            if bo:
                msg = Message(
                    name=bo.group(2), msg_id=int(bo.group(1)), dlc=int(bo.group(3))
                )
                messages[msg.name] = msg
                by_id[msg.msg_id] = msg
                current = msg
                continue

            if line.startswith("BO_") or not line.strip():
                # A BO_ we failed to parse, or a blank line: either way the
                # current message's signal block has ended.
                current = None

            sg = SG_RE.match(line)
            if sg and current is not None:
                current.signals[sg.group(1)] = (
                    int(sg.group(2)),          # start bit
                    int(sg.group(3)),          # size
                    sg.group(4) == "0",        # @0 = big-endian (Motorola)
                    sg.group(5) == "-",        # signed
                )
                continue

            val = VAL_RE.match(line)
            if val:
                target = by_id.get(int(val.group(1)))
                if target is not None:
                    target.vals[val.group(2)] = {
                        int(raw): label
                        for raw, label in VAL_PAIR_RE.findall(val.group(3))
                    }

    return messages


def decoder_subset(messages: dict[str, Message]) -> dict[str, Message]:
    return {n: m for n, m in messages.items() if n.startswith(DECODER_PREFIX)}


def describe(vendored: dict[str, Message], upstream: dict[str, Message]) -> list[str]:
    """Human-readable list of decoder-relevant changes, or empty."""
    out: list[str] = []
    old, new = decoder_subset(vendored), decoder_subset(upstream)

    for name in sorted(set(new) - set(old)):
        m = new[name]
        out.append(f"**{name}** added — `0x{m.msg_id:03X}`, DLC {m.dlc}, "
                   f"{len(m.signals)} signals. The decoder drops it entirely today.")
    for name in sorted(set(old) - set(new)):
        out.append(f"**{name}** removed — the decoder still expects it at "
                   f"`0x{old[name].msg_id:03X}`.")

    for name in sorted(set(old) & set(new)):
        a, b = old[name], new[name]
        if a.msg_id != b.msg_id:
            out.append(f"**{name}** moved — `0x{a.msg_id:03X}` → `0x{b.msg_id:03X}`.")
        if a.dlc != b.dlc:
            out.append(f"**{name}** DLC {a.dlc} → {b.dlc} — `decode_frame`'s "
                       f"length guard needs revisiting.")

        for sig in sorted(set(b.signals) - set(a.signals)):
            start, size, be, signed = b.signals[sig]
            out.append(f"**{name}.{sig}** added — bit {start}, {size} wide"
                       f"{', big-endian' if be else ''}{', signed' if signed else ''}. "
                       f"Not decoded.")
        for sig in sorted(set(a.signals) - set(b.signals)):
            out.append(f"**{name}.{sig}** removed — the decoder still reads it.")
        for sig in sorted(set(a.signals) & set(b.signals)):
            if a.signals[sig] != b.signals[sig]:
                out.append(f"**{name}.{sig}** layout changed — "
                           f"`{a.signals[sig]}` → `{b.signals[sig]}` "
                           f"(start, size, big_endian, signed).")

        for sig in sorted(set(a.vals) | set(b.vals)):
            if a.vals.get(sig, {}) != b.vals.get(sig, {}):
                out.append(f"**{name}.{sig}** enum table changed — the decoder's "
                           f"namer must be updated in lockstep.")

    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--vendored", required=True)
    ap.add_argument("--upstream", required=True)
    ap.add_argument("--summary-out")
    args = ap.parse_args()

    changes = describe(parse(args.vendored), parse(args.upstream))

    if args.summary_out:
        with open(args.summary_out, "w", encoding="utf-8") as fh:
            fh.write("\n".join(f"- {c}" for c in changes))

    print(f"affects_decoder={'true' if changes else 'false'}")
    for c in changes:
        print(f"  {c}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
