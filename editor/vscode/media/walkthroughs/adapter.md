# Pick your CAN adapter

The extension talks to your boards through a CAN adapter — a CANable/SLCAN dongle, a PEAK PCAN, a Vector VN-series, or a Linux SocketCAN interface.

Click **Select adapter…** to scan what's plugged in and choose one. Your pick is saved to `iscFs.interface` + `iscFs.channel` (machine-scoped, so it won't sync to other machines).

> On a fresh setup the extension already offers to auto-pick a single detected adapter. If you have several, this is where you choose.

Once an adapter is selected, the status-bar pill switches from **no adapter** to `interface: channel` — and turns red if that adapter later drops off the bus.
