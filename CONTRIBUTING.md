# Contributing to Decurion

Thanks for helping. Decurion is a small project, so a little process goes a long way. This page covers how to propose a change, what we check before merging, and how releases happen.

By taking part you agree to follow our [Code of Conduct](CODE_OF_CONDUCT.md). Found a security problem? Please do not open an issue; follow [SECURITY.md](SECURITY.md) instead.

## Proposing a change

- **Small fixes** (typos, clear bugs, small docs improvements): open a pull request straight away.
- **Anything bigger** (a new feature, a change in behavior, a new dependency, anything touching agent sandboxing, credentials, approvals or spending): open an issue first so we can agree on the approach before you spend time on code.
- **Hard-to-reverse choices** (storage formats, the hub and runner protocol, security boundaries, public interfaces): write a short Architecture Decision Record in [`docs/adr/`](docs/adr/) as part of the PR. Copy the shape of an existing ADR: context, decision, consequences.

## Branches, commits and pull requests

- Branch from `main` and keep branches short-lived. Rebase on `main` if it moves ahead.
- Use [Conventional Commits](https://www.conventionalcommits.org/) for commit messages and PR titles, for example `feat(hub): show spend per app` or `fix(runner): stop orphaned containers on start`. Common types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `ci`.
- One change per pull request. If you find yourself writing "and also" in the description, it is probably two PRs.
- We squash merge, so the PR title becomes the commit on `main`. Make it a good Conventional Commit.
- Fill in the pull request template: what was true before, what is true after, how you tested it, and the linked issue.

## Checks that must pass

CI runs these on every pull request. Run them locally before you push:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

We also check dependency licenses, duplicate crates and sources with [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) using [`deny.toml`](deny.toml):

```sh
cargo deny check
```

If you add a dependency with a license that is not on the allow list, say so in the PR and explain why it is needed.

## Rust style

- Stable Rust only. No nightly features.
- Libraries (such as `decurion-proto`) define their errors with `thiserror`. Binaries (`decurion-hub`, `decurion-runner`) use `anyhow` and add context to errors as they bubble up.
- No `unwrap()` outside tests. Clippy's `unwrap_used` lint is denied for the workspace. Handle the error, or use `expect("why this cannot fail")` only where it truly cannot fail.
- Log with `tracing`, not `println!`. Never log tokens, keys or other secrets.
- `unsafe` code is forbidden across the workspace.
- Keep user-facing text plain and friendly. Decurion is meant to be usable by people who are not developers.

## AI-written pull requests

Pull requests written with the help of AI coding agents (including ones run through Decurion itself) are welcome. They are held to exactly the same bar as any other PR: a human is responsible for the change, the checks must pass, and a maintainer reviews it like any other contribution. Please make sure you understand and can explain every line you submit.

Agents start at [AGENTS.md](AGENTS.md) (Claude Code reads it through [CLAUDE.md](CLAUDE.md)), which links the how-tos in [`docs/agents/`](docs/agents/).

## License and sign-off

Decurion is licensed under the [Apache License 2.0](LICENSE). By contributing, you agree that your contribution is licensed under the same license (inbound = outbound), as described in section 5 of the license.

We recommend signing off your commits with the [Developer Certificate of Origin](https://developercertificate.org/), which certifies that you wrote the change or otherwise have the right to submit it:

```sh
git commit -s
```

This adds a `Signed-off-by: Your Name <you@example.com>` line to the commit message.

## How releases happen

Releases are cut by a maintainer:

1. Bump `version` under `[workspace.package]` in the root `Cargo.toml` (we follow [Semantic Versioning](https://semver.org/); during 0.x, minor versions may include breaking changes).
2. Move the `[Unreleased]` entries in [`CHANGELOG.md`](CHANGELOG.md) under a new version heading with today's date.
3. Merge that change to `main`, then tag the merge commit `vX.Y.Z` and push the tag.
4. The release workflow builds the installers and attaches them to a **draft** GitHub release.
5. A maintainer checks the draft, edits the notes if needed, and publishes it.

The full checklist, including the help site and winget, is in [RELEASING.md](RELEASING.md).

## Questions

Open an [issue](https://github.com/decurion-dev/decurion/issues) if anything here is unclear. Improving this page is a great first contribution.
