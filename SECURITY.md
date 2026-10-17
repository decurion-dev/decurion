# Security policy

Decurion runs AI coding agents on your computer and holds the credentials they use, so we take security reports seriously. Thank you for helping keep users safe.

## How to report a vulnerability

Please report security problems **privately**, not in a public issue, pull request or discussion.

1. Go to the [Security tab](https://github.com/decurion-dev/decurion/security) of the repository.
2. Click **Report a vulnerability**.
3. Describe the problem, how to reproduce it, and what an attacker could do with it.

This uses GitHub's private vulnerability reporting, so only the maintainers can see your report. Please do not include real tokens, API keys or other secrets in the report; a placeholder is enough.

We aim to acknowledge reports within a few days and will keep you updated while we work on a fix. Once a fix is released we are happy to credit you in the advisory, if you would like.

## What is in scope

We especially want to hear about:

- **Agent sandbox escapes**: an agent in its container reaching the host, other containers, other apps' code, or anything outside its own workspace.
- **Credential leakage**: tokens or API keys held by the hub or runner ending up somewhere they should not, such as inside a container that should not have them, in logs, on the board, or in phone notifications.
- **Approval bypass**: a way for an agent or anyone else to merge, deploy, send outbound messages, or do anything else that needs the owner's approval without it.
- **Spending cap bypass**: a way for pay-per-use work to spend more than the monthly cap Decurion enforces.
- **Phone pairing and sign-in**: pairing a device without the owner's consent, reusing or stealing pairing codes or device sessions, or reaching the board or inbox without signing in.

Other issues that affect the security of Decurion users are welcome too. If you are not sure whether something is in scope, report it privately anyway.

Out of scope: problems in Claude Code, Docker, or other third-party tools themselves (please report those to their maintainers), and setups that turn off protections the docs tell you to keep on, such as exposing the Docker daemon on `tcp://localhost:2375` without TLS.

## Supported versions

While Decurion is in 0.x, only the **latest release** gets security fixes. Please check that you can reproduce the problem on the latest release (or `main`) before reporting.

| Version        | Supported |
| -------------- | --------- |
| Latest release | Yes       |
| Older releases | No        |

## A note on sensitive data

Decurion is a developer tool. It is **not designed to handle protected health information (PHI) or other personal data (PII)**, and it does not offer the safeguards that regulated data requires. Do not point it at systems, repositories or data sets that contain such information.
