# MingoCAN auto-update

ISC MingoCAN checks for a newer release on launch and can download +
install it in place, then relaunch. It uses the official
[`tauri-plugin-updater`](https://v2.tauri.app/plugin/updater/).

## How it works

1. On launch (and from **Settings → Updates → Check for updates**) the
   app fetches the update manifest from the endpoint in
   `apps/can-studio/src-tauri/tauri.conf.json` →
   `plugins.updater.endpoints`:

   ```
   https://github.com/isc-fs/can-flasher/releases/latest/download/latest.json
   ```

2. `latest.json` (published per release by CI) lists the latest version
   plus a per-platform signed bundle URL. If it's newer than the running
   version, a banner appears at the top of the window.

3. **Install & restart** downloads the bundle, verifies its signature
   against the public key baked into `tauri.conf.json`
   (`plugins.updater.pubkey`), installs it, and relaunches.

The check is best-effort: offline, no manifest yet, or a missing/invalid
pubkey simply shows nothing — the app never blocks or errors on it.

Platform self-update support:

| OS | Updater bundle | Notes |
|----|----------------|-------|
| macOS | `.app.tar.gz` (from the `.dmg`) | unsigned — see caveat below |
| Windows | `.nsis.zip` (from the `.exe`) | |
| Linux | `.AppImage.tar.gz` | **AppImage only** — the `.deb`/`.rpm` are owned by apt/dnf and can't self-update; run the AppImage build to get auto-update on Linux |

## One-time activation (required before the next release tag)

The updater signs every bundle with a **minisign keypair** (separate
from OS code-signing). The private key lives in CI secrets and must
never be committed. Until this is done, the in-app check no-ops.

1. **Generate the keypair** (writes a password-protected private key):

   ```sh
   cd apps/can-studio
   npm run tauri signer generate -- -w ~/.tauri/mingocan-updater.key
   ```

   It prints a **public key** and writes the private key to that path.

2. **Add two GitHub Actions repo secrets** (Settings → Secrets and
   variables → Actions):
   - `TAURI_SIGNING_PRIVATE_KEY` — the full contents of
     `~/.tauri/mingocan-updater.key`
   - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — the password you set

3. **Paste the public key** into `apps/can-studio/src-tauri/tauri.conf.json`:

   ```json
   "plugins": {
     "updater": {
       "endpoints": ["…"],
       "pubkey": "<paste the public key here>"
     }
   }
   ```

   (It currently holds the placeholder
   `REPLACE_WITH_TAURI_SIGNER_GENERATE_PUBLIC_KEY`.)

After this, the next `v*` tag builds signed updater artifacts and
uploads `latest.json` to the release automatically
(`bundle.createUpdaterArtifacts: true` + the `TAURI_SIGNING_*` env in
`.github/workflows/release.yml`).

> **Hard gate:** once `createUpdaterArtifacts` is on, a release build
> *fails* if the signing secret is absent. Set the secrets before
> cutting the next tag.

## Caveat — unsigned macOS

The app is not Apple-code-signed (it ships ad-hoc, `signingIdentity:
"-"`). Auto-update still works, but after the relaunch macOS Gatekeeper
may re-prompt "unidentified developer" — the same friction as the
initial install. Proper OS code-signing + notarization is a separate,
larger effort.

## End-to-end verification (post-activation)

1. Cut a test tag; confirm the release has `latest.json` + `*.sig`
   artifacts attached alongside the installers.
2. Install an older build, launch it → the update banner should appear.
3. Click **Install & restart** → the app relaunches on the new version.
