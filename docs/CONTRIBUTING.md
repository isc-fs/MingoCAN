# Contributing to MingoCAN

How the project is developed day-to-day: toolchain, test layout, CI,
branch conventions, how tracking issues and the roadmap stay in
sync. Read this before opening your first PR; the conventions aren't
obvious from looking at `git log` alone.

The repo holds three shipped surfaces at one version — the **MingoCAN**
desktop app (`apps/can-studio`), the **`can-flasher`** CLI (the root
crate), and the **VS Code extension** (`editor/vscode`). The crate and
binary keep the `can-flasher` name deliberately: renaming them would
break every script and CI job that invokes the tool.

If you just want to *use* it, see [INSTALL.md](INSTALL.md) and
[DESKTOP.md](DESKTOP.md).

---

## Development

### Toolchain

Pinned to the stable channel via `rust-toolchain.toml`; rustup auto-
installs the right version on first `cargo` invocation. Current MSRV
is **1.95**. `rustfmt` and `clippy` ship in the default profile.

### Common commands

```bash
cargo build                              # debug build
cargo build --release                    # optimised build (LTO, strip)
cargo test                               # full suite (lib + integration + doc)
cargo fmt                                # auto-format
cargo clippy --all-targets --all-features -- -D warnings  # same as CI (includes the swd feature)
```

### Test coverage

Three test flavours all run under `cargo test`:

- **Unit tests** in each module's `#[cfg(test)] mod tests { … }` — the
  bulk of the coverage (~90 % of tests). Pure functions, parsers,
  encoders.
- **Integration tests** under `tests/` — CLI-contract files for six
  subcommands (`*_subcommand.rs`), engine/pipeline tests, plus
  `virtual_pipeline.rs` for the end-to-end stack. They spin up the
  `VirtualBus` + `StubDevice` + `Session` and round-trip commands
  through the full pipeline, or spawn the real binary via
  `CARGO_BIN_EXE_can-flasher` for CLI-contract tests.
- **Doc tests** in `///` blocks — currently one example in
  `protocol::commands`.

Hardware-in-the-loop (real CANable / SocketCAN / PCAN / Vector adapters) is
not part of CI; it's covered by the manual smoke-test workflow.

### CI

`.github/workflows/ci.yml` runs on every push to `dev` / `main` and
every PR into them:

- `rustfmt --check`
- `clippy --all-targets --all-features -- -D warnings`
- `build + test` matrix: Linux / macOS / Windows

The app and extension have their own path-filtered workflows:
`can-studio-ci.yml` (frontend + Tauri build) and `editor-ci.yml`
(tsc + vsce package).

Docs-only changes (README / REQUIREMENTS / ARCHITECTURE / ROADMAP /
`docs/**`) skip CI via path filters — no runner minutes for comment
tweaks.

### Upstream DBC drift watch

`.github/workflows/ecu-dbc-drift.yml` runs daily (06:00 UTC) and on
manual dispatch. It diffs the vendored `src/pit_diag/testdata/ecu.dbc`
against the ECU repo's `dev` branch and keeps one `dbc-drift` issue in
sync: opened when they diverge, body updated while the drift persists,
closed automatically once the snapshot is refreshed.

It exists because `tests/ecu_dbc_conformance.rs` has a blind spot by
construction — it checks the decoder against the *vendored snapshot*, so
it can only fail once somebody re-vendors. Nothing in this repo changes
when the ECU wire moves. Six decode drifts accumulated behind that gap
before #528, and `0x708` sat undecoded after it merged upstream.

> **This is a scheduled workflow, so it only runs from the default
> branch.** Merging it to `dev` is not enough — it stays dormant until
> `dev` reaches `main` at the next release. Same applies to any
> `schedule`-triggered workflow added later.

---

## How we work with this repository

### Main branches

```mermaid
%%{init: { 'gitGraph': { 'mainBranchName': 'main', 'showCommitLabel': true }}}%%
gitGraph
    commit id: "v0.4.0-subcommands"
    branch dev
    checkout dev

    branch feat/1
    commit id: "feat work"
    checkout dev
    merge feat/1

    branch fix/1
    commit id: "fix work"
    checkout dev
    merge fix/1

    branch feat/2
    commit id: "feat work"
    checkout dev
    merge feat/2

    checkout main
    merge dev tag: "v1.0.0"
    checkout dev

    branch feat/3
    commit id: "feat work"
    checkout dev
    merge feat/3
```

- `main` only advances when `dev` is merged at a release milestone — every commit on `main` corresponds to a tagged release.
- `dev` accumulates integration from per-branch PRs; nobody commits directly to it.
- Feature branches and fix branches are cut from `dev`, opened as PRs against `dev`, and squash-merged once CI passes.

`main` carries validated, tagged releases (`v0.x.0-…`, culminating
at `v1.0.0` and whatever comes next). `dev` is where feat / fix
branches integrate. Nobody commits directly to either.

#### Branch protection

`main` is protected at the GitHub level — not just by convention:

- **PR-required.** Direct `git push origin main` is rejected by
  the server; every commit on `main` must arrive through a
  merged PR.
- **No force-pushes.** Tagged release commits (`v1.3.1`,
  `v1.3.0`, …) can't be rewritten — by anyone, including repo
  admins (`enforce_admins: true`).
- **No deletion.** The branch can't be deleted from the API or
  the UI.

`dev` is intentionally **not** behind the PR-required gate
because [`release.yml`](../.github/workflows/release.yml)'s
inline `sync-dev` job needs to push to `dev` after a tag-cut
release (fast-forward dev onto main with the github-actions
bot's token; no PR feasible from a workflow run). Force-pushes
and deletion may still be locked down later via a Rulesets bot
bypass if direct pushes start to bite.

**Manual branch cleanup.** `delete_branch_on_merge` is
intentionally **off** at the repo level — see the post-mortem
note below. Use `gh pr merge --delete-branch` (or click "Delete
branch" in the GitHub UI after merge) to clean feat/fix branches
yourself. Long-lived branches (`dev`, `main`) must never be
auto-deleted. **Never** pass `--delete-branch` (or click "Delete
branch") on a `dev → main` release PR — its head branch is `dev`.

> **Why off**: GitHub's `delete_branch_on_merge` flag is
> repo-wide and applies to *every* PR's head branch on merge,
> including the long-lived `dev` branch on a `dev → main`
> release PR. v2.0.0's release uncovered this — when PR #187
> (dev → main) merged with the flag on, GitHub silently deleted
> `dev`, and the inline `sync-dev` job in `release.yml` then
> failed at checkout because the branch it was trying to push to
> no longer existed. dev was restored from main (same SHA, no
> data loss), the flag was disabled, and the lesson stays in the
> repo's setting. There's no per-branch exclusion in classic
> branch protection; if we ever want a "delete except `dev` and
> `main`" rule, that's a Rulesets-tier project.

If you ever hit a "Required pull request is missing" or
"Protected branch update failed" error against `main`, you're
in the right state — open a PR instead.

### Branch naming

```
feat/<issue#>-<short-title>   new functionality for an issue  (feat/613-decode-bin-logs, …)
fix/<issue#>-<short-title>    bug or doc fix for an issue     (fix/560-remove-seal-active-log, …)
feat/<n>-<short-title>        new functionality, no issue     (feat/9-session-lifecycle, …)
fix/<n>-<short-title>         bug or doc fix, no issue        (fix/1-workflow-titled-branches, …)
release-X.Y.Z                 release cut                     (release-3.1.1)
```

- `feat/<issue#>-<slug>` / `fix/<issue#>-<slug>` for work on a GitHub
  issue (use the issue number).
- `feat/<n>-<slug>` / `fix/<n>-<slug>` for work with no issue, where
  `n` is the next per-category counter (run `git branch -a` first;
  `feat` and `fix` count independently, so `feat/2` and `fix/2` can
  coexist).
- `release-X.Y.Z` for release cuts (see
  [Cutting a release](#cutting-a-release)).

The short kebab-case title is mandatory so the purpose is visible at
a glance. `branch-issue.yml` warns that an issue-numbered branch has
the "wrong" counter; that warning is expected and safe to ignore.

### Tracking issues

Every `feat/` or `fix/` branch auto-creates a GitHub Issue on its first push
(release branches do not; via
`.github/workflows/branch-issue.yml`):

- Title: `[feat/N-short-title]` or `[fix/N-short-title]`
- Label: `feat` or `fix`
- Body: populated from the first commit's message

The issue closes automatically when the PR merges into `dev` (via
`.github/workflows/close-on-dev-merge.yml`). Closed issues form the
permanent history of the project — grepping them is how future
contributors see what's been done.

### Roadmap

[`../ROADMAP.md`](../ROADMAP.md) is **auto-generated** from
`.github/roadmap.yaml` by `.github/scripts/render_roadmap.py`. The
workflow runs on every push to `dev` and commits the regenerated
file if anything changed. Branch status badges come from the
tracking-issue state, so closed issues flip `🔜 planned` →
`✅ done` automatically.

Don't hand-edit `ROADMAP.md` — update the YAML instead.

### Typical workflow

```bash
# 1. Make sure dev is current
git checkout dev && git pull origin dev

# 2. Cut a branch (the issue number — or the next feat/fix counter
#    when there is no issue — + a short kebab title)
git checkout -b feat/613-decode-bin-logs

# 3. Work, commit, push
git commit -m "short description"
git push origin feat/613-decode-bin-logs

# 4. Open PR against dev (use `Closes #<issue>` in the body so the
#    tracking issue auto-closes on merge)
gh pr create --base dev --title "..." --body "Closes #NN …"

# 5. Squash-merge after review; the tracking issue closes itself
```

### Cutting a release

**One tag, three surfaces, one Release page.** From v2.0.0 onward
the `can-flasher` CLI, the VS Code extension, and the MingoCAN
desktop app all ship together at the **same version** from a single `v*` tag
(e.g. `v2.0.0`). The retired `editor-v*` and `can-studio-v*` tag
namespaces are not used for new cuts; one tag triggers one
GitHub Release page carrying the CLI binaries, the VSIX, and the
app bundles side-by-side.

**Release branches.** Cut `release-X.Y.Z` from `dev`. It carries the
version bump and `docs/RELEASE_NOTES_vX.Y.Z.md`, and is squash-merged
into `dev`. Then open a `dev → main` PR and merge it as a **merge
commit** (never `--delete-branch`). Tag that merge commit.

When cutting `vX.Y.Z`, bump **all six** version files (seven lines —
`Cargo.lock` has two entries) in the same commit on the release
branch. Five of them are checked by CI; `Cargo.lock` is not, so it is
the one that silently drifts:

| File | Field |
|---|---|
| `Cargo.toml` (root) | `version = "X.Y.Z"` |
| `Cargo.lock` | `can-flasher` **and** `can-studio` package entries' `version = "X.Y.Z"` — **not gated**, bump them by hand (only those two entries) or let `cargo build` do it |
| `editor/vscode/package.json` | `"version": "X.Y.Z"` |
| `apps/can-studio/src-tauri/Cargo.toml` | `version = "X.Y.Z"` |
| `apps/can-studio/package.json` | `"version": "X.Y.Z"` |
| `apps/can-studio/src-tauri/tauri.conf.json` | `"version": "X.Y.Z"` |

Then:

1. PR `release-X.Y.Z` (version bump + `docs/RELEASE_NOTES_vX.Y.Z.md`)
   into `dev`; squash-merge.
2. Open a `dev → main` PR titled `Release vX.Y.Z` and merge it with
   a **merge commit** — never `--delete-branch`.
3. Tag the `dev → main` merge commit with `git tag -a vX.Y.Z -m "…"`
   and push the tag.
4. The consolidated [`release.yml`](../.github/workflows/release.yml)
   triggers. Its `verify-version` gate compares the tag's
   `X.Y.Z` against the five gated files. Any mismatch fails the
   gate by file name, and all build legs skip — retag after
   bumping.

The five-way gate is the descendant of v1.1.0's version-skew
lesson (v1.1.0 binaries reported `can-flasher 0.1.0`; v1.1.1
added the original single-file guard). The current gate catches
the same class of mistake across all three surfaces in lockstep.

5. **Three build legs run in parallel** under the single
   workflow:
   - `cli-build` matrix → 4 binary archives (Linux x86_64 /
     aarch64, macOS aarch64, Windows x86_64)
   - `editor-build` → one `.vsix`
   - `studio-build` matrix → native bundles from the six targets
     configured in `tauri.conf.json` (`dmg`, `app`, `deb`,
     `appimage`, `rpm`, `nsis` — the Windows installer is the NSIS
     `-setup.exe`, not an `.msi`), plus their updater `.sig` files

   All assets (CLI archives, VSIX, app bundles + `.sig`,
   `latest.json`; 17 for v3.1.1) land on **one** GitHub Release
   page, named after the tag.

   `publish-iskapps` then mirrors the app installers + `latest.json`
   to isc-fs/iskapps (the updater's first manifest source; needs the
   `ISKAPPS_TOKEN` secret — see [UPDATES.md](UPDATES.md)).

6. **Dev re-syncs automatically.** The inline `sync-dev` job in
   `release.yml` fast-forwards `dev` to `main` (or creates a
   merge commit if dev has diverged) once all three build legs
   succeed. The standalone `sync-dev-after-release.yml` workflow
   stays as a manual-dispatch recovery handle for the rare case
   where the inline job didn't run.

7. **Set the Release notes from the file.** `release.yml` creates
   the page with a fixed install-snippet body; replace it with
   `gh release edit vX.Y.Z --notes-file docs/RELEASE_NOTES_vX.Y.Z.md`.

---

## Writing new code

A few conventions the codebase already follows — match them when
you add modules:

- **Module docs up top.** Every `pub mod` starts with a `//!`
  block that explains what the module does *and* what it doesn't
  do. Grep `src/` for `//!` to see examples. A reader coming to a
  file cold should understand the scope without leaving the file.
- **`ExitCodeHint` for CLI errors.** Subcommands return
  `anyhow::Error`s; attach an `ExitCodeHint` via `exit_err(hint,
  message)` when you want a specific process exit code. The hint
  sits in the error chain; `main.rs` walks it via `downcast_ref`.
  See `src/cli/mod.rs` for the enum and `src/cli/verify.rs` for an
  example.
- **Integration-test shape.** New subcommands get a
  `tests/<name>_subcommand.rs` file that spawns the real binary via
  `CARGO_BIN_EXE_can-flasher`; engine-level tests go next to the
  engine (`tests/flash_manager.rs`, `tests/virtual_pipeline.rs`).
  Keep subprocess tests for CLI contracts (args, exit codes,
  stdout shape) and in-process tests for behaviour.
- **Don't commit to `dev` or `main` directly.** Everything lands
  via a PR from a `feat/`, `fix/` or `release-` branch.
- **No `Co-Authored-By` trailers.** Commits go out under the
  author's single authorship.

If your change warrants a test in the stub bootloader
(`src/transport/stub_device.rs`), extend it — the stub exists
specifically so integration tests can run without hardware.
