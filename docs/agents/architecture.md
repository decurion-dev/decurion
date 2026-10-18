# How a goal flows through Decurion

Read this before any change that crosses the hub, the runner and a box.
Owner-level description: `docs/coordinator.md`. Types: `crates/decurion-proto/src/goal.rs`.

## The path of one goal

1. **Owner chat.** `goals.html` posts `POST /api/goals` (`goals::add` in `crates/decurion-hub/src/goals.rs`), which calls `Store::add_goal` in `crates/decurion-hub/src/coordinator.rs`. That queues a coordinator turn (`queue_turn`) as a row in `helper_jobs`.
2. **Coordinator.** The runner's `helper_loop` (`crates/decurion-runner/src/main.rs`) claims it with `POST /api/runner/helper/claim` (`Store::claim_helper`), and starts it in a box with `helper_run_args`: the app's code mounted read-only (`goal_copy`), no `Bash`/`Edit`/`Write`/web tools (`COORDINATOR_DISALLOWED`). It only returns text. `Store::finish_helper` reads the last JSON object (`last_json_object::<CoordinatorReply>` then `validate`); a bad shape is asked again once. The reply holds `say`, `questions`, an optional `plan`, and `remember` (notes, `save_notes`).
3. **Owner answers** with `POST /api/goals/{id}/answer` or `/say` (`goal_answer`, `goal_say`); each queues another turn.
4. **Design page.** When a reply carries a plan, the goal moves to `PlanReady` and a `design` helper job is queued. It draws the plan as an HTML page into `/out` (`read_page` in runner `lib.rs`), stored in `goal_pages` and served sandboxed by `goals::page` (`PAGE_CSP`).
5. **One approval.** `POST /api/goals/{id}/approve {plan_message_id}` (`Store::approve_plan`) refuses anything but the latest plan, then inserts one `threads` row and one `goal_pieces` row per piece, in one transaction. The goal moves to `Working`.
6. **Runner claims.** The runner polls `POST /api/runner/claim` (`Store::claim` in `store.rs`), which picks queued work for that place, sets money aside for paid accounts (moving capped work to Needs you), and returns a `Claim` (thread, app, account, budget, messages, test-key names, checkpoint).
7. **Box.** `run_in_box` (runner `main.rs`) builds the copy from the hub's base bundle (`GET /api/runner/threads/{id}/base`, `checkpoints.rs` `base_bundle`; the start commit is fixed once in `threads.base_commit`), starts the box's gate (`start_site_gate`, `gate_args`), then the agent with `run_args` (Claude) or `pi_run_args` (ChatGPT, Grok). The prompt goes in on stdin (`prompt_for`).
8. **Events.** Agent output lines become `RunEvent`s (`parse_line_hiding`, or `PiRun` for pi) and are posted to `POST /api/threads/{id}/events` (`add_event`). The hub saves each one in `events` before the board shows it. Owner messages reach a running agent through `messages.rs` and the runner's `Conversation`.
9. **Outcome.** `outcome()` in runner `main.rs` decides the state: In review only if the agent finished, left new commits (`new_commits`, fetched into the runner-owned bare repo `review_repo`), and no test key leaked (`leaked_keys`). Otherwise Failed, Queued (Max limit) or Needs you (budget). The runner moves the thread with `POST /api/threads/{id}/move`.
10. **Review helper.** `move_thread` in `lib.rs` calls `Store::piece_thread_moved`; a piece entering In review gets a `review` job (`queue_review`). The checker runs in a throwaway copy made from the review repo (`branch_copy`), never the builder's folder. Its `ReviewVerdict` sets the piece to `passed`, `fixing` (notes sent back as a follow-up message, at most `MAX_REVIEW_ROUNDS` = 2), or `needs_a_look`.
11. **Report.** When every piece is settled, `maybe_report` queues a report turn; the goal moves to `ForReview` and the phone gets a notice.
12. **Accept sends it to GitHub; the owner merges there.** Accept moves the thread to Done, and in the same transaction (`request_move` in `store.rs`) `github::want_send` records the send: `threads.gh_state = 'waiting'`, the branch `decurion/<app>-<n>-<title words>` (`send_branch`) and the accepted commit (the latest checkpoint's `head`). The `move_thread` handler then spawns `github::Sender::send`, one send at a time. It marks the thread `sending`, reads the work from the saved copy into the hub's bare copy of the app (`fetch_saved_code`), checks it is the accepted commit, and runs a plain `git push origin <commit>:refs/heads/<branch>` in the app's own folder, with that bare copy lent as objects (`GIT_ALTERNATE_OBJECT_DIRECTORIES`), the owner's own git sign-in and every prompt off (`push`, `push_args`). For a github.com remote it finds or opens the pull request through GitHub's API when a token is available (Settings, `Vault::get_github`, else `gh auth token`); otherwise it stores a one-tap compare link (`compare_url`). The end is recorded as `sent` or `failed`, with plain words in `gh_error` and an audit row by actor `hub`; the phone gets a notice that opens the pull request (`Pusher::notify_to`). At start, `github::resume` sends anything left `waiting` or `sending`; that is safe because a push of the same commit changes nothing and the pull request is looked for before one is opened. A failed send keeps the work Done; the owner taps Try again (`POST /api/threads/{id}/github`, `retry_send`). The by-hand route still works: the branch is in `<data>/review/<app>-<n>.git` (see `docs/help/src/review-and-merge.md`). Nothing merges.

Thread states and allowed moves: `ThreadState::can_move_to` in `crates/decurion-proto/src/lib.rs`. Review states: `ReviewState` in `goal.rs`.

## Where state lives

- Hub data folder: `DECURION_DATA`, else `%LOCALAPPDATA%\decurion` or `$HOME/decurion` (`data_dir` in hub `main.rs`). `hub.db` (SQLite), `keys.json` (owner and runner keys), `saved-work/` (bundles and checkpoints), `vault/` off Windows.
- Schema: `crates/decurion-hub/migrations/*.sql`, applied by `Store::open`. Main tables: `apps`, `threads`, `events`, `audit` (append-only), `runs` (money), `receipts`, `messages`, `goals`, `goal_messages`, `goal_pieces`, `helper_jobs`, `goal_pages`, `app_notes`, `devices`, `runners`, `checkpoints`, `test_keys`, `account_usage`, `owner_keys` (last four of the owner's GitHub token). Sending to GitHub lives in `threads.gh_*` (`0044_github.sql`).
- Runner data folder (same rule, `data_dir` in runner `main.rs`): `workspaces/` (each thread's copy, mounted at `/work`), `review/` (runner-owned bare repos).
- Secrets: Windows Credential Manager (`decurion/claude-profile-A`, `decurion/anthropic-api`, `decurion/openai-api`, `decurion/xai-api`, `decurion/app-<id>/test-key/<NAME>`, and `decurion/github` for the owner's optional GitHub token, read only by the hub), or `DECURION_*` env vars off Windows (`Credential::env_fallback`).

## The screens

- Board `/` (`board.html`): columns by state, the composer, Settings (apps, websites, computers, accounts, test keys, phones).
- Phone inbox `/inbox` (`inbox.html`), paired by QR code (`devices.rs`), notices by Web Push (`push.rs`, `sw.js`).
- Goals `/goals` (`goals.html`). Add cloud worker `/cloud` (`cloud.html`).
- All are plain files served by `page()` in `lib.rs`; they call `/api/...` with the device cookie.

## The cloud runner

Same runner binary, run on a rented server (`docker/cloud/`, `identity.rs`). It signs up with a one-time code (`POST /runner/join`, `workers.rs`), keeps no credentials (the hub lends one per run, `POST /api/runner/threads/{id}/credential`, `secrets.rs`), and reaches only the routes in `cloud_reach`. Moving work saves a checkpoint (code bundle + session, `checkpoints.rs`, `handover.rs`) that the other computer rebuilds from. A runner with a different `RULES_VERSION` is refused and the cloud worker rebuilds itself to the hub's commit (`docker/cloud/update.sh`).

## Tests that walk this path

`a_goal_becomes_reviewed_work` in `crates/decurion-hub/tests/goals.rs` (whole goal), `accepting_work_sends_it_to_github_as_a_branch_and_a_pull_request` in `tests/github.rs` (step 12), `work_saved_on_one_computer_continues_on_another` in `tests/places.rs` (moves), `claim_hands_each_thread_out_once` in `tests/api.rs`.
