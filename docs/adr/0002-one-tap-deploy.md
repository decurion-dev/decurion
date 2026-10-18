# ADR 0002: One-tap deploy

- **Status:** Accepted. A first version was built on 2026-10-18; see **As built** below, which wins where it differs from the rest of this page.
- **Date:** 2026-10-18
- **Deciders:** the owner (final say), maintainers
- **Phase:** C3, after the first real run on the owner's PC. Parts that need that run are marked **Settle after the first run**.

## In plain words

Today Decurion stops at the pull request. You merge on GitHub, and putting the new version live is still up to you. One-tap deploy finishes that last step for apps that hold no patient or personal data.

**What it looks like.** You merge a pull request on GitHub. A moment later your phone shows:

> **Ready to deploy Shop**
>
> 3 changes since the live version: Fix login, New price list, Faster search.
> Goes to: shop.example.com (Netlify)
>
> **Deploy** · Not now

You tap **Deploy** once. Decurion does the whole release and tells you when it is live, with a link. That one tap is the ask. There is no second "are you sure", no typing the app's name, no chain of screens.

**What keeps it safe.**

- Only apps you have marked "holds no patient or personal data" ever get a Deploy button. Apps with patient data never do.
- Nothing deploys without your tap. There is no automatic deploy, and no setting to turn one on.
- Only merged work goes live: the exact commit on your main branch that the card showed you.
- The steps for deploying an app are written down once, in the app, and you approve them once. If they change, the button waits until you look at the change.
- The key that deploys an app can deploy only that app. It stays in this PC's password store. No agent ever sees it.
- If something goes wrong, the live site stays as it was where the host allows it, you see why in plain words, and you can tap **Try again**. If a release is live but bad, **Go back** returns to the last good one, also with one tap.

## As built (2026-10-18)

The owner asked for this to be built ahead of the first real run, starting with the EMR, and decided these changes from the design below:

- **Apps with patient data may deploy.** The EMR holds patient data, and the owner chose Yes to letting its Deploy button release it (2026-10-18). There is no "no patient data" mark that blocks deploys. What still holds is that Decurion never reads an app's data: it starts the app's own release and keeps what the steps print, which is the app's script's job to keep free of patient details.
- **The steps live in the app's rules, not in the repo.** The owner saves them in Settings, on the app's own page (`can_deploy`, `deploy_steps`, `deploy_site` on `apps`, owned by the per-app rules work). They are plain commands, one per line, run without a shell (`decurion_proto::deploy::parse_steps`). There is no `.decurion/deploy.toml`.
- **They run on the owner's PC, in a clean copy, with the PC's own sign-ins.** Each deploy runs in the hub's own copy of the repo (`<data>/saved-work/deploys/app-N`) checked out at the exact merged commit, never in the owner's folder and never in an agent box. The steps use the PC's own sign-ins (for the EMR, the AWS command line's), so there is no per-app deploy key in the vault. The hub's own keys are kept out of the steps' surroundings.
- **Changed steps are shown, not approved separately.** Files the steps name (such as `scripts/deploy-prod.mjs`) are hashed at each release. When one changed since the last release, the card names it and the button says "Deploy new steps", which shows the steps before the one tap that runs them. The run refuses if the file changed again after the card was drawn. Agent work that touches those files is held for the owner like a protected part (`protected.rs`).
- **Not built yet:** Go back to the last good release, provider APIs (Netlify, Vercel and the rest), and checking the site after the release (the EMR's own script checks it).

Code: `crates/decurion-hub/src/deploy.rs` (cards, the run, routes), `migrations/0105_deploys.sql` (`deploys`, `deploy_audit`), the Today strip (`today.rs`), and the deploy box on the board and the phone (`ui.js`). Help: `docs/help/src/deploy-your-app.md`. Tests: `crates/decurion-hub/tests/deploy.rs`.

## Context

The owner asked for this on the plan page: "One-tap deploy, later. For apps with no patient data, a single Deploy tap after merge that does the whole release. It still asks every time, as the safety rules say, but only once. I'd design this after the first real run on your PC." And elsewhere: "The point of this is one click not a bunch of asks."

Rules that bind this design:

- **Hard stops (PLAN.md D7, AGENTS.md wall 8).** Production deploy is a hard stop. Today it has no code path at all. A new path is allowed only if it "stops for the owner every time". This design adds exactly one stop per deploy, the Deploy tap, and no other new asks.
- **No PHI or PII (wall 10, ADR 0001 section 14).** Decurion never handles health or personal data. A deploy of an app that holds such data would make Decurion part of handling it, so those apps are left out entirely.
- **Merge is always the owner's (wall 6).** Decurion opens a pull request at most (`crates/decurion-hub/src/github.rs`). Deploy starts only after the owner has merged on GitHub, and it never merges.
- **Credentials by name, one per run (wall 4); push only in the hub (wall 5).** Boxes hold no GitHub key. The owner's GitHub token only opens and watches pull requests.
- **Voice never approves (D6).** A Deploy tap is a tap, never a spoken yes.

What exists today that this builds on: the hub watches the pull requests it opened until they are merged or closed (`pr_watches`, migration `0066_pr_watch.sql`); owner taps carry a `receipt` and are acted on once (`receipt_result`, `save_receipt` in `store.rs`); the vault keeps per-app secrets by name (`Vault::put`, `Vault::get` in `crates/decurion-vault`); boxes are sealed by `box_args` and reach out only through the gate; the protected-parts hold reads `.decurion/protected-paths` from the commit the work started from (`protected.rs`); and ADR 0001 already lists `deploy` as a hard-stop effect no add-on can skip.

## Decision

### 1. Which apps qualify

- A new per-app setting, **No patient or personal data**, off by default. Only the owner can turn it on, from the app's settings on the board, with one tap that carries a receipt. The screen says plainly what the owner is stating: "This app holds no patient records or other personal information, and it never sends Decurion any."
- It is the owner's statement, not something Decurion checks or guesses. No agent, coordinator, checker, add-on or runner key can set it. The route is owner-only (`forbidden` for everyone else) and is not in `cloud_reach`. Nothing in the repo can set it either: there is no file or field an agent could edit to turn it on.
- An app without the mark gets no Deploy button, no deploy recipe screen and no deploy key, and the hub refuses a deploy for it whatever the request says. The check is in the hub's deploy handler, not only in the page.
- Turning the mark off removes the button at once and withdraws any waiting "Ready to deploy" card. Turning it on or off writes an audit row.
- Stored as a new column on `apps` in a new migration, the next free number above the highest file.

### 2. The deploy recipe, approved once per app

- Each app that wants deploys keeps one file in its repo: `.decurion/deploy.toml`. It names one provider from a closed list (`vercel`, `netlify`, `fly`, `cloudflare-pages`, or `command` for a fixed command), the provider's project or site name, the address the release goes to, and an optional page to check after release. A `command` recipe names its command and every script file it runs. Unknown keys refuse the recipe, because Decurion cannot show the owner a step it does not understand (as ADR 0001 does for permissions).
- The owner approves it once, on one screen in plain sentences: "Deploys Shop to shop.example.com through Netlify, using your saved key "shop-deploy"." The hub stores the SHA-256 of the recipe and of every file it names, with who approved it and when.
- The recipe is always read from the merged commit on the default branch, never from a work branch.
- **If the recipe changes.** The hub treats `.decurion/deploy.toml`, and every file a recipe names, as a protected part for every app, even if the app's own `.decurion/protected-paths` does not list it (a built-in pattern in `protected.rs`). So agent work that touches it is held for the owner at review. At deploy time the hub checks the hashes again. If anything differs from what was approved, there is no Deploy button. The card says "The deploy steps for Shop changed", shows only what changed, and offers one **Approve** tap. Until then nothing deploys, and the old approval is not used for the new steps. This is the same rule as an add-on update in ADR 0001 section 6.

### 3. The one tap

- When the pull request watch sees a merge on an app that qualifies and has an approved recipe, the hub saves a deploy row (`waiting`) bound to one exact commit: the merge commit GitHub reports for that pull request on the default branch. The inbox and the board show **Ready to deploy <app>**, what is in it compared with the live version (changes merged outside Decurion are listed too, marked as such), and where it goes.
- One **Deploy** tap, with a receipt, does the whole release. A resend of the same receipt gets the first result. The tap carries the commit the card showed; if the default branch has moved on, the hub refuses with "A newer change was merged. Check the card again." and the card refreshes, the same way `approve_plan` is bound to the exact plan shown.
- More merges before the tap do not stack up asks: the one card moves to the newest merged commit. "Not now" hides the card until the next merge.
- Every deploy asks, exactly once. There is no automatic deploy, no deploy on a timer, no "always deploy this app" setting, and no confirmation after the tap. The coordinator, agents, add-ons, voice and runner keys cannot start a deploy.

### 4. Where it runs, and with what key

- **Never in an agent box.** Agent boxes keep their one credential and no deploy key.
- **One deploy key per app**, kept in the vault in that app's scope under a reserved name (`deploy`), which is never lent to an agent run as a test key (like the reserved `claude-profile-*` names in ADR 0001). The owner makes the key at the provider, scoped to that one project where the provider allows it (for example a Fly deploy token for one app, or a Cloudflare token limited to Pages on one account). The owner's GitHub token is never used for deploys: it keeps doing only what it does today.
- **Provider recipes:** the hub asks the provider's own API to build and release that exact commit, with the key in a request header, never on a command line or in a log. No code from the repo runs on the owner's PC.
- **Command recipes, or providers that need a local build:** two sealed boxes, one after the other. A build box with no key at all and the normal built-in sites, which turns the commit into files. Then a deploy box from a Decurion image pinned by digest, which gets the built files, the key by name only (`-e NAME`), and a gate allowlist of only the recipe's target hosts. Both use the same walls as `box_args` (a new `deploy_args` with tests next to the wall tests), so this bumps `RULES_VERSION`.

### 5. What the owner sees

- **Progress** on the card: Getting the code, Building, Sending to <host>, Checking the site.
- **Success:** "Shop is live", with an **Open site** link, and a phone notice. The deploy is saved as the app's last good release.
- **Failure:** what happened in plain words and what to do next ("Netlify did not accept the deploy key. Save a new one in Shop's settings, then tap Try again."), and the log for anyone who wants it. Where the host releases all at once, the live site is unchanged. **Try again** is one tap for the same commit.
- **Go back:** on the app, **Go back to the last good release** returns to the previous live commit, using the provider's own rollback where there is one, or a fresh deploy of that commit with the recipe approved for it. It is one tap with a receipt, and it asks once like any deploy.
- **Crash safe:** the deploy is marked `deploying` before anything is sent. A hub that stops halfway asks the provider what happened on restart ("Interrupted, checking") instead of sending again blindly.
- Every step writes an audit row: mark set or cleared, recipe approved, deploy tapped, started, live, failed, gone back, and each use of the deploy key (name only). This needs the general audit table ADR 0001 section 12 also asks for.

### 6. What it does not do

- Never merges, and never deploys work that is not merged on the default branch.
- Never runs for an app without the owner's no-patient-data mark.
- Never touches production data. Recipes have no step for database changes or data copies. An app whose release needs one is not eligible in this version.
- Never deploys without the owner's tap, and never asks more than once per deploy.
- Never raises or skips the spend cap. A deploy runs no AI model and uses no agent money. Provider charges are outside Decurion's meter; the owner sets limits at the provider.
- Never hands the deploy key to an agent, and never uses the owner's GitHub token or Claude sign-in for it.

## Consequences

**Good**

- The last step of the owner's day becomes one tap after the merge, which is what the owner asked for.
- The hard stop stays real: one deploy, one tap, bound to one commit. Walls do the rest: the eligibility mark, a recipe approved once and held as a protected part, one scoped key per app, and sealed boxes with a one-host allowlist.
- Going back is as easy as going forward, so a bad release costs one more tap, not an evening.

**Costs and risks**

- The no-patient-data mark is only as good as the owner's word. Decurion cannot check it, and says so on the screen.
- Provider APIs differ and change. Each provider in the closed list is code to keep up, so the list should start small.
- Some providers cannot scope a key to one project. Then the key reaches more than one site, and the setup screen must say so.
- A merge of a change made outside Decurion also goes live with the next tap. The card lists those changes, but the owner may not read them.
- New routes, a new table, a vault name rule and, for command recipes, a new kind of sealed box, each of which touches a protected part of Decurion.

## Alternatives considered

- **Deploy automatically on merge.** Fewest taps, but production deploy is a hard stop (D7). Rejected.
- **A chain of confirmations** ("Are you sure?", type the app's name). The owner ruled it out: "one click not a bunch of asks". Rejected.
- **Deploy from the agent's box, or let an agent run the deploy command.** Puts a production key next to code the agent wrote. Rejected.
- **Trigger a GitHub Actions deploy workflow.** Needs a broader GitHub token and puts the deploy key in GitHub's secrets, out of Decurion's sight. Possible later, as a provider, if the owner wants it.
- **Let the coordinator or checker decide which apps hold personal data.** That is a statement only the owner can make. Rejected.

## Open questions

**Settle after the first run** on the owner's PC:

1. Which providers the owner's real apps use, so the first version builds only those.
2. Whether builds happen at the provider (simpler, no box) or in the build box (one way for every provider).
3. What "Checking the site" checks: the page loads, or a page the recipe names, and how long to wait.
4. Whether a waiting card should expire, or stay until the next merge.
5. Preview and production: whether some apps want a preview release first, and if so whether that is a second tap (it would be a second ask, so likely not).
6. How Go back works for a provider with no rollback of its own.
7. Docker Desktop on Windows for the build and deploy boxes, and how long a deploy takes on the owner's PC.
8. Whether the deploy row and card should live with the thread (one piece of work) or the app (one release can hold many pieces).
