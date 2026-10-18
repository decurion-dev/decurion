# Fix a bug

1. **Reproduce it with a failing test first.** Write the test, run it, see it fail for the reason in the report. Only then change code.
2. Make the smallest change that makes it pass. Keep the walls in `AGENTS.md` as they are; a fix never loosens one.
3. Run the full checks from `AGENTS.md`. Your box has the Rust toolchain and C compiler they need; say in your summary any you could not run (`check-template.sh`, `cargo deny`, `mdbook`).
4. In your final summary: what was wrong, the test that shows it, and what you changed. Add a `### Fixed` line under `[Unreleased]` in `CHANGELOG.md` if the owner could notice.

## Where tests live

| Kind | Where | Helpers to reuse |
| --- | --- | --- |
| Hub API, end to end in memory | `crates/decurion-hub/tests/api.rs` | `hub()`, `call()`, `seed()`, `claim()`, `finish()`, `app_with()`, `work()`, `mv()`, `event()`, `state()` |
| Goals, coordinator, checker | `crates/decurion-hub/tests/goals.rs` | `hub()`, `app()`, `helper(&r, kind, text, page)`, `view()`, `approved_one_piece()` |
| PC and cloud, moving work, saved copies | `crates/decurion-hub/tests/places.rs` | `hub()` (temp data folder), `app_repo()` (a real git repo), `join()`, `claim()` |
| Gatekeeper (pay-per-use proxy) | `crates/decurion-runner/tests/gatekeeper.rs` | `rig()` |
| Gate (website allowlist) | `crates/decurion-gate/tests/gate.rs` | `gate()`, `echo_site()`, `exchange()`, `blocked()` |
| Unit tests | `#[cfg(test)] mod tests` at the bottom of: runner `lib.rs`, `main.rs`, `handover.rs`, `pricing.rs`, `identity.rs`; hub `coordinator.rs`, `checkpoints.rs`; proto `lib.rs`, `goal.rs`, `sites.rs`; gate `lib.rs`, `addr.rs`, `head.rs`; vault `lib.rs` | |

Most runner logic sits in `crates/decurion-runner/src/lib.rs` precisely so it
can be tested without Docker or a hub. If the bug is in `main.rs`, move the
pure part into `lib.rs` and test it there.

Tests are named as plain sentences of what must be true
(`a_token_dies_with_its_run`, `capped_pay_per_use_work_does_not_hold_up_max_work`). Name yours the same way.
CI runs tests on Windows too: build paths with `Path`/`join`, and do not
assume `/` or a Unix shell.

## Running one test

```sh
cargo test -p decurion-hub --test api only_the_owner_chooses_an_apps_websites
cargo test -p decurion-hub --test goals a_goal_becomes_reviewed_work
cargo test -p decurion-hub --lib titles_are_the_first_line_clipped
cargo test -p decurion-runner --lib pi_runs_are_sealed_the_same_way_and_get_their_key_by_name
cargo test -p decurion-runner --bin decurion-runner leak_check_reads_the_agents_commits
cargo test -p decurion-gate --test gate ip_addresses_are_refused
cargo test -p decurion-proto
```

Add `-- --exact` to match one name only, `-- --nocapture` to see output.

## Crash-safety rules in the code

A runner, the PC or the hub can stop at any moment. The code assumes it will.

- **Save first, show second.** Every run event is stored in SQLite before the board sees it (`add_event`). The runner posts `RunEvent::Started` *before* `docker run`; if the hub cannot be told, the run does not start (`run_in_box`).
- **One transaction per decision.** Read-then-write store code opens `begin_with("BEGIN IMMEDIATE")` and does the check and the write together. State moves go through `checked_move`, which writes the audit row in the same transaction. The checker's notes and the send-back are one transaction (`finish_helper`).
- **Receipts make taps idempotent.** An owner action carries a receipt; `receipt_result` returns the first answer for a resend instead of acting twice (`a_resent_receipt_gets_the_first_answer`). Internal sends use fixed receipts too (`review-{job_id}`).
- **Effectful vs idempotent steps.** `safe_to_redo` (`crates/decurion-proto/src/lib.rs`) lists the tools that only look (`Read`, `Glob`, `Grep`, ...). A step cut off mid-way is saved as `Interrupted` (`interrupted_step` in `store.rs`); on resume, the agent is told which step it was and, unless it only looked, to check what happened before repeating it (`prompt_for`). Tests: `a_cut_off_step_is_redone_only_when_it_only_looked`, `a_resumed_run_hears_which_step_was_cut_off`.
- **Messages are acked only when read.** Owner messages are marked delivered only after Claude shows it read them (`Acks`, `messages_are_acked_only_after_claude_shows_it_read_them`).
- **Restarts requeue.** `POST /api/runner/hello` puts the runner's running work and helper jobs back in the queue (`pause_runner_threads`, `pause_runner_helpers`).
- **Money errs high.** A run with no trusted cost report counts its whole set-aside amount if its container started (`spend.rs`, `close_job`; `a_started_box_without_a_trusted_cost_is_charged_its_budget`).
- **Fail closed.** If a check cannot run (leak scan, commit count, unreadable pi output), the work does not go to review (`outcome`, `UNREADABLE_STOP`).

Keep to these when you fix something. A fix that makes a step happen twice
after a crash, or shows something before it is saved, is a new bug.
