# ADR 0003: Rules for each app

- **Status:** Accepted
- **Date:** 2026-10-18
- **Deciders:** the owner (final say), maintainers

## In plain words

Each app has its own page in Settings (Settings, Apps, then the app) that says what Decurion may do for it. Two buttons set it in one tap:

- **PR only.** Decurion pushes the work and opens a pull request. You, or the app's reviewer, merge on GitHub. Nothing deploys. Every app starts here.
- **Full control.** On top of that, finished pull requests get a **Merge** button, and merged work gets a **Deploy** button that runs the steps you typed for the app.

Each part is also its own switch, so an app can have a mix. Two more switches: whether a goal's piece that passes its check goes to GitHub by itself (on, as before) or waits for your Accept, and whether the app's live version holds patient data (on until you say otherwise), which tells every agent on it to use made-up data only.

## Context

The owner runs apps with different owners and reviewers. The PV MediSpa EMR is the owner's own: they want Decurion to merge and deploy it. The Be Safe Healthcare app is reviewed and merged by Param, so Decurion should stop at the pull request. Until now the rules were the same for every app, and Decurion never merged anything.

## Decision

- **Where the rules live.** Columns on `apps` (`0095_app_rules.sql`), the shared type `AppRules` on `App` (`decurion-proto`), changed only by the owner through `POST /api/apps/{id}/rules` (owner-only, with a receipt, not in `cloud_reach`). Nothing in an app's repo and no agent's answer can set them. Defaults are PR only, checked work sent by itself, patient data on.
- **Merge (wall 6 changes).** Before: Decurion never merged. Now: it merges only on the owner's Merge tap (`crates/decurion-hub/src/merge.rs`), one tap per pull request, only for an app whose rules allow it, only a pull request Decurion opened, only the exact commit Decurion last pushed, and only when GitHub reports it clean (checks passed, required reviews in, no conflict). That keeps PLAN.md D7: the hard stop is the tap, and nothing merges by itself.
- **Deploy.** The rule and the steps are stored here (`AppRules::deploy_steps`, read with `decurion_proto::deploy`); the Deploy button, its card and its run belong to the one-tap deploy work (ADR 0002), which reads this rule instead of adding its own switch. The owner chose to let the EMR deploy from Decurion even though it holds patient data: Decurion only starts the app's own deploy steps and never reads patient records.
- **Patient data.** A statement by the owner, not something Decurion checks. When on, every agent run and helper on the app is told to use made-up data only and never to touch live data (`PATIENT_DATA` in the runner). Wall 10 is unchanged: Decurion itself never handles such data.

## What stays the same for every app

The sealed box, one credential per run, the website allowlist, pushing only in the hub and never to the default branch, the spend cap, and the protected-parts hold. None of these became a per-app switch: each one protects the owner's PC and accounts, not just one app.

## Consequences

- An app at Full control merges with one tap instead of a trip to GitHub. A merge needs the owner's GitHub token to have Contents: Read and write on that repo.
- Turning a rule on never acts by itself: it only shows a button.
- Two more owner-only routes and one more protected file (`merge.rs`, in `.decurion/protected-paths`).
