# v2.4.2 — `cf config nvm write --reset` + USAGE.md positional-args fix

A CLI-led patch release. VS Code extension and Studio bundles
ship unchanged; their version moves only with the lockstep.

## Highlights

### `--reset` flag on `cf config nvm write`

Boot-only NVM keys like `BL_NVM_KEY_NODE_ID = 0x0001` only take
effect on the next bootloader start. Without `--reset` the
operator had to power-cycle the board manually (or shell out a
separate `send-raw` reboot frame), which defeated the
over-the-wire-provisioning appeal of the new NVM-override
workflow.

`--reset` is opt-in so a routine flash-write-counter bump never
surprises anyone with a reboot. When set, the host:

1. Sends `CMD_NVM_WRITE`, awaits ACK.
2. Fires `CMD_RESET[Bootloader]` as fire-and-forget — the
   bootloader reboots before sending an ACK, so any error from
   the send call is trace-logged and dropped.
3. Disconnects.

Mode `Bootloader` keeps the chip in the BL after reset rather
than auto-jumping to the app — operator stays in a known state
for the verifying `cf discover` / `cf config nvm read`.

Typical provisioning flow now looks like:

```bash
cf --node-id 0x1 config nvm write 0x0001 0x02 --reset
# Wrote 1 byte(s) to NVM key 0x0001.
# Sent CMD_RESET [Bootloader]; the device is now rebooting and
# will re-resolve NVM keys on next boot.
# Run `can-flasher discover` to confirm the new state.

cf discover     # now shows the board at node 0x02
```

### `docs/USAGE.md` positional-args fix

`config nvm read` / `write` / `erase` always took positional
args; the documented `--key 0x0001 --value 0xDEADBEEF` form
never existed. Swept to the actual positional shape.

## Compatibility

- **CLI**: install the new binary and the new flag is available.
- **VS Code extension** and **Studio bundles**: functionally
  unchanged; lockstep version bump only.

## Closes

- gh #231 tasks (1) and (3) — `--reset` flag + USAGE.md fix.
  The `node-id` named-alias task (2) is intentionally deferred
  to a follow-up release.
