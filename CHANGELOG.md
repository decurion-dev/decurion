# Changelog

All notable changes to Decurion are recorded here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html). While Decurion is in 0.x, a minor version may include breaking changes.

## [Unreleased]

### Added

- Installers and a release workflow that builds them into a draft GitHub release.
- Built-in help pages that explain each part of Decurion in plain language.
- A design for add-ons, so new kinds of agents and tools can plug in later.
- **Talk to a running agent.** Change its course now, give it a follow-up for when it finishes, or add a note, from the board or the phone. Messages wait safely with the hub until the agent has read them.
- **Tasks within tasks.** Work can belong to a bigger task. Stopping a task stops its smaller tasks first, unless they were set to keep going on their own.
- **Receipts.** Every tap and message carries a receipt number, so a resend after a dropped connection never acts twice, and a tap from an out-of-date screen is ignored.
- **Interrupted, checking.** After a crash the agent is told which step was cut off. A step that only looked at things is redone; one that changed something is checked first, never blindly repeated.
- **ChatGPT and Grok as workers.** Choose Claude, ChatGPT or Grok when you add work. ChatGPT and Grok run through pi, an open-source coding agent, in the same sealed box, and stop at their run's budget.
- **Usage meters and a cap per account.** The board shows how much of each Max plan's week and five hours is used, and what each paid account (Claude pay-per-use, ChatGPT, Grok) spent this month against its own cap.
- **A cap per app.** Each app can have its own monthly cap across all paid AIs, coordinator and review work included.
- **Take over the test browser.** Watch an agent's test browser live from the board or your phone, take control, and hand it back. The browser now runs in a box of its own beside the agent's.
- **Saved website sign-ins.** Sign in once to a site an app needs (a staging admin, a test dashboard) in a sign-in window. Decurion keeps the sign-in encrypted on your PC, never the password, gives it only to that app's runs, and puts a card in your inbox when it needs renewing.
- Contribution guide, security policy, code of conduct, issue and pull request templates, and a cargo-deny configuration for dependency license and source checks.

## [0.1.0] - Unreleased

The first version: Claude Code agents running in sealed containers on your PC, on one board, with an approvals inbox on your phone. Windows first.

### Added

- **Sealed agent containers.** Each piece of work runs Claude Code in its own Docker container with its own copy of the code. The only credential inside is the Claude token for that app's profile (A or B), kept in Windows Credential Manager. GitHub, AWS and other keys never enter the container, and the host's `~/.claude` folder is never mounted.
- **The board.** The hub keeps apps and work in SQLite, allows only valid state moves, keeps an append-only audit log, and serves a board page. Work moves to *In review* only when the agent has actually committed to its branch, counted in a runner-owned bare repository so git never runs on the agent's own config. Agents cannot move their own cards.
- **Phone inbox.** Pair a phone by scanning a one-use QR code that expires after 30 minutes. The `/inbox` page lists only what needs you (In review, Needs you, Failed) with plain explanations and Accept, Send back, Reject and Retry buttons. Signed-in devices survive hub restarts and can be removed from the board.
- **Phone notifications.** Encrypted Web Push alerts whenever work needs you, carrying only a short line and a link to the inbox.
- **Crash-safe restarts.** Every run event is saved before the board shows it. After a crash the runner requeues its in-flight work and resumes the same Claude Code session, and a restarting runner stops containers a dead runner left behind.
- **Monthly pay-per-use spending cap.** Apps can run on an Anthropic API key instead of a Max account. Decurion sets money aside before each pay-per-use run, passes a budget to Claude Code, and stops pay-per-use work at its monthly cap (default $100), moving it to *Needs you* with a phone alert. The board shows this month's spend against the cap and what Max runs were worth.

[Unreleased]: https://github.com/decurion-dev/decurion/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/decurion-dev/decurion/releases/tag/v0.1.0
