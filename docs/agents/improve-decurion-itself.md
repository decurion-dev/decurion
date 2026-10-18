# Improve Decurion itself

Decurion can be asked to fix or improve Decurion. There is no special mode:
the owner adds Decurion's own repo as an app, and the work goes the normal
way. Owner-facing version: `docs/help/src/improve-decurion-itself.md`.

## How the owner adds it

1. They need a **git clone** of `https://github.com/decurion-dev/decurion` on the PC. The installed program folder (for example `%LOCALAPPDATA%\Programs\Decurion`) is not a git repository and will not work.
2. Board, **Settings**, **Apps**, **Add an app**: Name (for example `decurion`), Repo path (the clone's full path), Claude account (Max plan A or B), optional pay-per-use. The form posts `POST /api/apps` (`add_app` in `crates/decurion-hub/src/lib.rs`, owner-only), which calls `Store::add_app` in `store.rs`: `check_app_name` (1-40 letters, digits, `-`, `_`), `check_repo_path` (absolute, no commas or quotes), `clean_sites`.
3. Each piece of work starts from the latest commit of the branch the clone is on (`base_bundle` in `checkpoints.rs`, fixed once per piece in `threads.base_commit`). Uncommitted changes in the clone are not included.

## How the work goes

1. The owner gives a goal. The coordinator reads the code read-only and proposes a plan; the owner approves it once (`approve_plan`).
2. Each piece is built by its own agent in its own sealed box, on its own branch (`decurion/thread-<n>`), in a copy under the runner's `workspaces/` folder. The owner's clone is never touched.
3. When the agent finishes with commits, the checker reviews a separate copy taken from the runner-owned review repo (`branch_copy`), and passes it, sends it back with notes (at most 2 rounds), or marks it **needs a look**.
4. The owner reviews and merges, by pulling the branch from `<data>/review/<app>-<n>.git` into their clone (`docs/help/src/review-and-merge.md`). Decurion never merges or pushes. If the change goes to GitHub as a pull request, CI (`.github/workflows/`) runs the checks there.

**The running hub never changes itself.** The hub and runner keep running the
programs they were started from. A merged change runs only after the owner
updates: installs a new release, or rebuilds and restarts from their clone
(`cargo run -p decurion-hub`, `cargo run -p decurion-runner`). Installed
copies rebuild agent images at start only when a Dockerfile's hash changed
(`prepare_image` in `packaging/unix/decurion-start`, `Initialize-Image` in
`packaging/windows/start-decurion.ps1`). A cloud worker rebuilds itself to the
hub's commit (`HUB_COMMIT`, from `crates/decurion-hub/build.rs`) using
`docker/cloud/update.sh`, which downloads that commit from GitHub, or the
newest `main` if the commit is not on GitHub. So a change that exists only in
the owner's clone does not reach the cloud worker; if it also changed
`RULES_VERSION`, the hub refuses that worker until the versions match.

## What you can and cannot do in the box

- Your box is the same as for any app: the walls in `AGENTS.md` apply to you. You cannot reach the owner's PC, the hub, Docker or GitHub, and you hold one credential.
- **Tooling gap.** The Claude worker image (`docker/claude-worker/Dockerfile`, `node:22-bookworm-slim` plus git, ca-certificates and ripgrep) installs no Rust toolchain, and Docker is not available in the box. `static.rust-lang.org` is not on `sites::BUILT_IN`. Run `cargo --version` first. If the checks cannot run, do not try to get around the box: keep the change small, write the tests anyway, and say plainly in your final summary which checks you could not run, so the owner (or CI) runs them.
- Write the change as you would for any contributor (`CONTRIBUTING.md`): one change, a Conventional Commit message, tests, `CHANGELOG.md` and help pages when the owner will notice.

## Protected parts

An app's repo may contain `.decurion/protected-paths`: glob patterns, one per
line, `#` starts a comment, gitignore-like matching (`*` within one folder
level, `**` across levels, a trailing `/` for a whole folder). The exact rules
(also `?`, case-insensitive matching, no `!` exceptions) are in the `//!`
header of `crates/decurion-hub/src/protected.rs` (`LIST_FILE`, `parse`).

- The hub reads this file **from the commit the work started from** (the piece's base commit), never from the work branch, so an agent cannot unprotect anything by editing it.
- If a piece changes any file that matches, or changes `.decurion/protected-paths` itself, the hub holds the piece as **needs a look** for the owner, with the list of protected files touched, **even when the checker passed it**.
- Decurion's own repo ships such a file at `.decurion/protected-paths`, listing the files that hold its walls. Read it before you plan a change.

A hold is not a failure: the owner decides. Do not split, rename or move
files to avoid a match, and do not edit `.decurion/protected-paths` to get a
task through. If the task truly needs a protected file changed, change only
what is needed and say in your final summary which protected files you
touched and why.

## Never loosen a wall to get a task done

If a task seems to need any wall in `AGENTS.md` weakened (box flags, the
allowlist, credentials, the spend cap, a hard stop, the owner's merge, the
Claude rules), **stop**. Do not make that change. Say in your final summary
what the task needed, which wall stood in the way, and what the owner could
decide instead.
