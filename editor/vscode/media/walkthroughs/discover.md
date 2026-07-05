# Find a board on the bus

With an adapter selected, click **Discover devices** to broadcast on the bus and list every board currently in its **bootloader**.

Each result shows its node-id, firmware version, and git-hash. The team scheme is:

| Board | node-id |
| ----- | ------- |
| ECU   | `0x1`   |
| AMS   | `0x2`   |
| uDV   | `0x3`   |

Results also appear in the **ISC MingoCAN › Devices** view in the activity bar, where you can right-click a board to flash it directly.

> Nothing found? Make sure the board is powered and in its bootloader, the adapter is on the bus, and the bitrate matches (`iscFs.bitrate`, default 500 kbit/s).
