# Add a new kind of agent (worker type)

Today a new kind of agent is a normal change to Decurion. The template is the
**pi worker** (ChatGPT and Grok work), added in commit `4d05b10`. Later, the
add-on system's `[[agent]]` kind is meant to replace this route, and pi is
meant to become a first-party add-on (`docs/adr/0001-add-on-system.md`,
status Proposed, not built). Until then, follow the steps below.

Not to be confused with `crates/decurion-hub/src/workers.rs`: that file is
about *computers* (the PC runner and cloud workers), not kinds of agent.

## What the pi worker added, layer by layer

| Layer | Where | What |
| --- | --- | --- |
| Image | `docker/pi-worker/Dockerfile` | Pinned `PI_VERSION`, `npm install --ignore-scripts`, config folder `/opt/pi-agent` owned by root so the agent cannot change prices or add-ons, offline and telemetry off by env, user `agent` with `AGENT_UID`, `WORKDIR /work`, entrypoint through `node --disable-sigusr1`. |
| Which AI | `Ai` (`Chatgpt`, `Grok`), `Ai::reports_session_cost` in `crates/decurion-proto/src/lib.rs`; `NewThread.ai` | The owner picks it per piece of work (`select#ai` in `board.html`). |
| Who pays | `Account` (`OpenAi`, `Xai`), `Account::for_work`, `Account::is_paid` | Each paid account has its own monthly cap (`spend.rs`). |
| Schema | `crates/decurion-hub/migrations/0041_accounts.sql` | `threads.ai` with a `CHECK` of allowed values. |
| Credential | `Credential::{OpenAi, Xai}` in `crates/decurion-runner/src/lib.rs`: `vars()`, `env_fallback()`, `target()`, `pi_model()` | One key, passed by name. `pi_model()` pins provider and model so a pi upgrade cannot change models or prices unnoticed. |
| Hub lends it | `names()` in `crates/decurion-hub/src/secrets.rs` | For cloud workers, which keep no keys. |
| Box args | `pi_run_args` (runner `lib.rs`) | Calls the shared `box_args` with `PI_IMAGE`, then pi's own flags: `--mode json`, `--provider`, `--model`, `--session-dir`, `--session-id` (checked to be a plain name), `--no-extensions --no-skills --no-prompt-templates --no-approve`. |
| Output | `PiRun` (runner `lib.rs`) | Turns pi's JSON lines into `RunEvent`s, adds up cost, stops at the budget (`over_budget`), and refuses to trust unreadable output (`UNREADABLE_STOP`). |
| Run loop | `run_in_box` and `follow_pi` (runner `main.rs`) | `let pi = cred.pi_model().is_some();` picks args and loop. pi gets its task once on stdin, then stdin closes. |
| Sessions | `session_export_args` (`handover.rs`) | Saves `.pi/sessions` next to `.claude/projects`, so work can move computers. |
| Sites | `sites::BUILT_IN` (`crates/decurion-proto/src/sites.rs`) | `api.openai.com`, `api.x.ai`. |
| Redaction | `redact`, `key_at` (runner `lib.rs`) | OpenAI and xAI key shapes are hidden from events. |
| Shipping | `.github/workflows/release.yml`, `packaging/windows/decurion.iss`, `start-decurion.ps1`, `packaging/unix/decurion-start` (`prepare_image`), `docker/cloud/update.sh` (`PI_IMAGE`), `spike/wincred.py` | The image is built on the owner's machine; if it fails, Claude work still runs. Editing `docker/cloud/*` needs `bash docker/cloud/check-template.sh --fix`. |
| Docs | `README.md` "ChatGPT and Grok" | Owner steps, including setting the provider's own spend limit. |

## Tests to copy

Runner (`cargo test -p decurion-runner --lib <name>`):
`pi_runs_are_sealed_the_same_way_and_get_their_key_by_name`,
`pi_refuses_claude_keys_odd_session_ids_and_comma_paths`,
`each_paid_account_has_its_own_key`, `parses_pi_text_tools_and_result`,
`a_pi_run_that_did_not_settle_has_no_result_or_trusted_cost`,
`budget_is_reached_at_or_past_the_limit_only`,
`unreadable_pi_output_stops_the_run_and_is_not_trusted`,
`openai_and_xai_keys_are_redacted_but_ordinary_words_are_not`,
`test_keys_are_hidden_from_pi_output`.

Hub (`cargo test -p decurion-hub --test api <name>`):
`each_ai_runs_on_its_own_account`, `a_resumed_pi_run_is_budgeted_and_charged_on_its_own`,
`every_paid_account_has_its_own_cap`, `chatgpt_and_grok_work_does_not_wait_on_a_max_outage`.

## Rules for any new kind

- Reuse `box_args`. Never add mounts, devices, capabilities, `--privileged`, host networking or the Docker socket. The sealing test for your kind must assert the same things as `token_is_passed_by_name_and_no_host_paths_are_mounted`.
- Exactly one credential, by name. Never a Claude token or the Anthropic key: those belong only to the official Claude Code CLI image (PLAN.md D1). Add the refusal test, like `pi_refuses_claude_keys_...`.
- Turn off everything the agent can load from `/work` (plugins, skills, project settings, MCP servers). The code in `/work` is untrusted.
- If it spends money, it must report cost per run and the runner must stop it at `budget_usd`. The owner must also set the provider's own monthly limit; say so in the README.
- Pin the agent's version and model in the Dockerfile and the `Credential`, so nothing changes without a commit.
- Bump `RULES_VERSION`.
