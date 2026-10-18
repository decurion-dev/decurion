# Releasing Decurion

How a new version gets from `main` to the Releases page. Nothing becomes
public until a maintainer presses **Publish** in step 5.

## What the release builds

| Download | For | What it does |
| --- | --- | --- |
| `Decurion-Setup-<version>-x64.exe` | Windows 10 (2004 or later) and 11 | Installs for the current user with no administrator prompt, adds **Decurion** and **Decurion - Save a Claude sign-in** to the Start menu, can start Decurion when you sign in to Windows, and offers to install missing helpers (Docker Desktop, Git, Claude Code, Tailscale) with winget. |
| `decurion-<version>-macos-universal.tar.gz` | Apple Silicon and Intel Macs | `install.sh` copies the programs to `~/.local/bin`; `decurion-start` runs Decurion. |
| `decurion-<version>-linux-x64.tar.gz`, `-linux-arm64.tar.gz` | Linux | Same as the Mac download. |
| `SHA256SUMS.txt` | Everyone | Fingerprints to check a download was not changed. |

Every download is installed, started and uninstalled on a clean machine by
`.github/workflows/release.yml` before it is attached to the release. The same
checks run on any pull request that changes `packaging/`.

## Steps

1. **Pick the version.** Set `version` under `[workspace.package]` in
   `Cargo.toml`, run `cargo build` so `Cargo.lock` follows, and move the
   `[Unreleased]` notes in `CHANGELOG.md` under a heading for the new version.
   Merge that as one pull request.
2. **Tag it** on `main`: `git tag v0.1.0 && git push origin v0.1.0`. The tag
   must match `Cargo.toml`, or the release workflow stops.
3. **Wait for the release workflow.** It builds every download, tests each one,
   and creates a **draft** release with the files and the changelog notes.
   0.x versions are marked as a pre-release.
4. **Try the draft.** Download the Windows installer from the draft and install
   it on a real PC. Check the board opens.
5. **Publish.** On the Releases page, open the draft and press **Publish
   release**. This is the moment it becomes public.
6. **Help site.** Publishing a release also puts the help pages online with
   GitHub Pages. The first time, turn Pages on (Settings > Pages > Source:
   GitHub Actions), then allow tags to deploy: Settings > Environments >
   github-pages > Deployment branches and tags > add a tag rule `v*`.
   Without that rule GitHub refuses the deploy from a release tag.
7. **winget (optional).** The workflow saves filled-in winget manifests as the
   `winget-manifests` artifact. Submitting them to
   [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) makes
   `winget install Decurion.Decurion` work. That is a public pull request to
   Microsoft's repository, so a maintainer does it by hand.

## Not done yet

- **Code signing.** The Windows installer is not signed, so SmartScreen warns
  ("More info", then "Run anyway"). A code-signing certificate (or Azure
  Trusted Signing) removes the warning. Mac programs get only a local
  signature; Apple notarization needs a paid Apple Developer account.
- **Mac and Linux key storage.** On Windows, keys live in Credential Manager.
  On Mac and Linux, Decurion keeps them in its own `vault` folder, readable
  only by your user, not in Keychain or Secret Service. The pay-per-use key
  there comes only from `DECURION_ANTHROPIC_API_KEY`.
- **Automatic updates.** To update, run the newer installer over the old one.
  Your boards and settings stay. A built-in update check comes later.
- **Start at sign-in on Mac and Linux.** On Windows the installer offers it
  (checked by default). On Mac and Linux, run `decurion-start` yourself.
- **Computer use in a separate virtual machine.** Listed for Phase 5 in the
  plan, not built yet.
