# Releasing Decurion

How a new version gets from `main` to the Releases page. Nothing becomes
public until a maintainer presses **Publish** in step 5.

## What the release builds

| Download | For | What it does |
| --- | --- | --- |
| `Decurion-Setup-<version>-x64.exe` | Windows 10 (2004 or later) and 11 | Installs for the current user with no administrator prompt, adds **Decurion** and **Decurion - Save a Claude sign-in** to the Start menu, and offers to install missing helpers (Docker Desktop, Git, Claude Code, Tailscale) with winget. |
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
   GitHub Pages. The first time, turn Pages on: Settings > Pages > Source:
   GitHub Actions.
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
- **Mac and Linux sign-ins.** The runner reads Claude tokens from Windows
  Credential Manager. On Mac and Linux it reads environment variables
  (`DECURION_CLAUDE_TOKEN_A`, `DECURION_CLAUDE_TOKEN_B`,
  `DECURION_ANTHROPIC_API_KEY`) until Keychain and Secret Service support lands.
- **Start at sign-in and automatic updates.** Decurion runs while its window is
  open. A background service and `decurion update` come later.
- **App icon.** The Start menu entries use the PowerShell icon until the
  design work provides one.
