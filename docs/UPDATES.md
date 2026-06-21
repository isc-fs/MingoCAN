# MingoCAN auto-update

ISC MingoCAN checks for a newer release on launch and can download +
install it in place, then relaunch. It uses the official
[`tauri-plugin-updater`](https://v2.tauri.app/plugin/updater/).

## How it works

1. On launch (and from **Settings → Updates → Check for updates**) the
   app fetches the update manifest from the endpoints in
   `apps/can-studio/src-tauri/tauri.conf.json` →
   `plugins.updater.endpoints` (tried in order):

   ```
   https://raw.githubusercontent.com/isc-fs/iskapps/main/mingocan/latest.json
   https://github.com/isc-fs/can-flasher/releases/latest/download/latest.json
   ```

   The first is the **iskApps** "pit garage" ([isc-fs/iskapps](https://github.com/isc-fs/iskapps)) —
   the team's public download + auto-update channel shared with the
   other desktop apps (Wario Charger etc.). The second is the
   can-flasher release, kept during the transition so installs built
   before the iskApps endpoint existed keep updating.

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

## iskApps mirror — `ISKAPPS_TOKEN` secret (required)

The `publish-iskapps` job in `release.yml` re-hosts each release's
Studio installers to [isc-fs/iskapps](https://github.com/isc-fs/iskapps)
as a `mingocan-vX.Y.Z` release and commits `mingocan/latest.json`
(the same manifest, with its `url`s rewritten to the iskApps assets —
signatures unchanged, so the baked-in pubkey still verifies).

Actions' built-in `GITHUB_TOKEN` is scoped to can-flasher and **can't
write to iskApps**, so this needs a cross-repo token:

- Create a fine-grained PAT (or GitHub App installation token) with
  **`contents: write`** on `isc-fs/iskapps`.
- Add it to can-flasher as the repo secret **`ISKAPPS_TOKEN`**.

When the secret is absent the job no-ops (logs a warning, exits 0), so
the release still succeeds — only the iskApps mirror is skipped.

## One-time activation (required before the next release tag)

The updater signs every bundle with a **minisign keypair** (separate
from OS code-signing). The private key lives in CI secrets and must
never be committed.

> **Status: configured for v2.5.0.** The keypair was generated, the
> two secrets below are set on the repo, and the public key is in
> `tauri.conf.json`. The steps here are kept for **key rotation** or
> re-setup — you don't need to run them for a normal release.

1. **Generate the keypair** (writes a private key; `-p ''` = no
   password, `--ci` = non-interactive):

   ```sh
   cd apps/can-studio
   npx --yes @tauri-apps/cli@^2 signer generate -w ~/.tauri/mingocan-updater.key -p '' --ci
   ```

   It writes the private key to that path and the **public key** to
   `…key.pub`.

2. **Set two GitHub Actions repo secrets:**

   ```sh
   gh secret set TAURI_SIGNING_PRIVATE_KEY --repo isc-fs/can-flasher < ~/.tauri/mingocan-updater.key
   printf '' | gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --repo isc-fs/can-flasher   # empty password
   ```

3. **Put the public key** (`cat ~/.tauri/mingocan-updater.key.pub`)
   into `apps/can-studio/src-tauri/tauri.conf.json` →
   `plugins.updater.pubkey`.

Every `v*` tag then builds signed updater artifacts and uploads
`latest.json` to the release automatically
(`bundle.createUpdaterArtifacts: true` + the `TAURI_SIGNING_*` env in
`.github/workflows/release.yml`).

> **Hard gate:** once `createUpdaterArtifacts` is on, a release build
> *fails* if the signing secret is absent. (Satisfied for v2.5.0.) If
> you rotate the key, update both the secret and the `pubkey`.

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
