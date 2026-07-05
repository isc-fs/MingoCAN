# Your first flash

Click **Build & Flash** (or press `Ctrl/Cmd+Alt+F`) to build your firmware and flash it over CAN in one step:

1. Runs `iscFs.buildCommand` (CMake by default — auto-detects `CMakePresets.json`).
2. Finds the built artifact (`iscFs.firmwareArtifact`).
3. Asks which board you're flashing (ECU / AMS / uDV) if `iscFs.nodeId` isn't set.
4. Flashes over CAN, streaming live progress in the status bar.

On success you get a definitive receipt — `Flashed ECU v0.2.0 @293db9c ✓` — and a **readback** check confirms the board is running the commit you expect.

> Daily loop: after the first flash, `Ctrl/Cmd+Alt+R` (**Re-flash last**) repeats it with no build and no prompts. If a flash fails, the error toast offers one-click fixes; **Doctor** diagnoses the environment.
