//! Pieces of the runner that do not need Docker or a hub, kept here so they
//! can be tested directly.

use decurion_proto::{Account, Profile, RunEvent};
use serde_json::Value;
use std::path::Path;

pub mod gatekeeper;

pub const IMAGE: &str = "decurion/claude-worker:dev";
/// The image for ChatGPT and Grok work: pi, an open-source coding agent that
/// talks to many providers (`docker/pi-worker`).
pub const PI_IMAGE: &str = "decurion/pi-worker:dev";

/// Host environment variables the docker CLI itself needs. Nothing else from
/// the runner's environment reaches the docker process, and `docker run` only
/// forwards the variables named with `-e`.
pub const DOCKER_CLI_ENV: &[&str] = &[
    "PATH",
    "SystemRoot",
    "SystemDrive",
    "USERPROFILE",
    "HOME",
    "LOCALAPPDATA",
    "APPDATA",
    "ProgramData",
    "ProgramFiles",
    "TEMP",
    "TMP",
    "DOCKER_HOST",
    "DOCKER_CONTEXT",
    "DOCKER_CONFIG",
];

/// The one credential an agent gets: its Max profile's Claude token, a
/// per-run token for the gatekeeper (which holds the pay-per-use Anthropic key
/// on the host), or the OpenAI or xAI key for pi. Never two, and never the
/// Anthropic API key itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    Max(Profile),
    PayPerUse,
    OpenAi,
    Xai,
}

impl Credential {
    pub fn for_account(account: Account) -> Credential {
        match account {
            Account::A => Credential::Max(Profile::A),
            Account::B => Credential::Max(Profile::B),
            Account::Api => Credential::PayPerUse,
            Account::OpenAi => Credential::OpenAi,
            Account::Xai => Credential::Xai,
        }
    }

    /// The environment variables the container gets for it.
    pub fn vars(self) -> &'static [&'static str] {
        match self {
            Credential::Max(_) => &["CLAUDE_CODE_OAUTH_TOKEN"],
            Credential::PayPerUse => &["ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL"],
            Credential::OpenAi => &["OPENAI_API_KEY"],
            Credential::Xai => &["XAI_API_KEY"],
        }
    }

    /// Where it is read from on machines without Credential Manager.
    pub fn env_fallback(self) -> String {
        match self {
            Credential::Max(p) => format!("DECURION_CLAUDE_TOKEN_{}", p.as_str()),
            Credential::PayPerUse => "DECURION_ANTHROPIC_API_KEY".to_owned(),
            Credential::OpenAi => "DECURION_OPENAI_API_KEY".to_owned(),
            Credential::Xai => "DECURION_XAI_API_KEY".to_owned(),
        }
    }

    /// Where it is kept in Windows Credential Manager.
    pub fn target(self) -> String {
        match self {
            Credential::Max(p) => format!("decurion/claude-profile-{}", p.as_str()),
            Credential::PayPerUse => "decurion/anthropic-api".to_owned(),
            Credential::OpenAi => "decurion/openai-api".to_owned(),
            Credential::Xai => "decurion/xai-api".to_owned(),
        }
    }

    /// For work pi does: its provider and model. These are pi's own defaults
    /// for each provider in the pinned version, named here so that upgrading
    /// pi cannot switch models (and prices) unnoticed.
    pub fn pi_model(self) -> Option<(&'static str, &'static str)> {
        match self {
            Credential::OpenAi => Some(("openai", "gpt-5.5")),
            Credential::Xai => Some(("xai", "grok-4.7")),
            Credential::Max(_) | Credential::PayPerUse => None,
        }
    }
}

pub fn container_name(thread_id: i64) -> String {
    format!("decurion-thread-{thread_id}")
}

pub fn home_volume(thread_id: i64) -> String {
    format!("decurion-home-{thread_id}")
}

const RULES: &str = "You are working in your own copy of the code on its own branch. \
Commit your work to this branch as you go. Do not push and do not open pull requests; \
Decurion does that after review.";

pub fn prompt_for(task: &str, resume: bool) -> String {
    if resume {
        "Your container was restarted. Continue the same task from where it stopped. \
Check `git status` and `git log` first and do not redo finished steps."
            .to_owned()
    } else {
        format!("{task}\n\n{RULES}")
    }
}

/// The `docker run` arguments every agent gets, up to and including the
/// image. Credentials are passed by name only, so they never appear on a
/// command line, and the task goes in on stdin (`-i`), so a long task cannot
/// hit the Windows command-line limit.
fn sealed_args(
    thread_id: i64,
    worktree: &Path,
    cred: Credential,
    image: &str,
) -> anyhow::Result<Vec<String>> {
    let src = worktree.display().to_string();
    // docker reads --mount as comma-separated fields.
    if src.contains([',', '"']) {
        anyhow::bail!("workspace path {src} contains a comma or quote");
    }
    let mut a = [
        "run",
        "-i",
        "--rm",
        "--name",
        &container_name(thread_id),
        "--mount",
        &format!("type=bind,src={src},dst=/work"),
        "--mount",
        &format!("type=volume,src={},dst=/home/agent", home_volume(thread_id)),
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--memory",
        "4g",
        "--pids-limit",
        "1024",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect::<Vec<_>>();
    for v in cred.vars() {
        a.extend(["-e".to_owned(), (*v).to_owned()]);
    }
    a.extend(
        [
            "-e",
            "GIT_CONFIG_COUNT=1",
            "-e",
            "GIT_CONFIG_KEY_0=safe.directory",
            "-e",
            "GIT_CONFIG_VALUE_0=/work",
            "-e",
            "GIT_AUTHOR_NAME=decurion-agent",
            "-e",
            "GIT_AUTHOR_EMAIL=agent@decurion.invalid",
            "-e",
            "GIT_COMMITTER_NAME=decurion-agent",
            "-e",
            "GIT_COMMITTER_EMAIL=agent@decurion.invalid",
            image,
        ]
        .map(str::to_owned),
    );
    Ok(a)
}

/// Arguments for `docker run` of Claude Code.
pub fn run_args(
    thread_id: i64,
    worktree: &Path,
    session_id: &str,
    resume: bool,
    cred: Credential,
    budget_usd: Option<f64>,
) -> anyhow::Result<Vec<String>> {
    let mut a = sealed_args(thread_id, worktree, cred, IMAGE)?;
    a.extend(
        [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
        ]
        .map(str::to_owned),
    );
    a.push(if resume { "--resume" } else { "--session-id" }.into());
    a.push(session_id.into());
    if let Some(b) = budget_usd {
        // Claude Code stops itself at this amount; the hub set it aside.
        a.push("--max-budget-usd".into());
        a.push(format!("{b:.2}"));
    }
    Ok(a)
}

/// Where pi keeps its sessions: in the thread's own volume, so a resumed run
/// finds the conversation so far.
const PI_SESSIONS: &str = "/home/agent/.pi/sessions";

/// Arguments for `docker run` of pi, for ChatGPT or Grok work. pi asks no
/// permission questions and has no spending limit of its own; the runner
/// stops it at the budget (see [`PiRun`]). `--session-id` opens the saved
/// session with that id or starts it, so a first run and a resumed one take
/// the same arguments. pi loads no add-ons, skills or project settings, so
/// files in the work's folder cannot load code into pi or change what it
/// reports.
pub fn pi_run_args(
    thread_id: i64,
    worktree: &Path,
    session_id: &str,
    cred: Credential,
) -> anyhow::Result<Vec<String>> {
    let Some((provider, model)) = cred.pi_model() else {
        anyhow::bail!("{} is not a key pi uses", cred.target());
    };
    // pi's rule for session ids; the hub's are UUIDs.
    let ok_id = session_id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && session_id.ends_with(|c: char| c.is_ascii_alphanumeric())
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !ok_id {
        anyhow::bail!("session id {session_id:?} is not one pi accepts");
    }
    let mut a = sealed_args(thread_id, worktree, cred, PI_IMAGE)?;
    a.extend(
        [
            "--mode",
            "json",
            "--provider",
            provider,
            "--model",
            model,
            "--session-dir",
            PI_SESSIONS,
            "--session-id",
            session_id,
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-approve",
        ]
        .map(String::from),
    );
    Ok(a)
}

/// Whether a run has used up its budget. `None` is a flat-rate run, which
/// has none.
pub fn over_budget(spent_usd: f64, budget_usd: Option<f64>) -> bool {
    budget_usd.is_some_and(|b| spent_usd >= b)
}

/// Whether a result's text is Claude saying the Max account is out of usage,
/// and when it resets if it said (the older form ends in `|<unix seconds>`).
pub fn usage_limit(text: &str) -> Option<Option<i64>> {
    let t = text.to_ascii_lowercase();
    // Claude's own wording for a Max plan running out. Generic API errors
    // ("rate limit reached", "output token limit reached") and the API's
    // spend-limit message are not this.
    let hit = [
        "claude ai usage limit reached",
        "claude usage limit reached",
        "hit your limit",
        "5-hour limit reached",
        "weekly limit reached",
        "out of extra usage",
    ]
    .iter()
    .any(|p| t.contains(p));
    if !hit {
        return None;
    }
    let epoch = text
        .rsplit_once('|')
        .and_then(|(_, tail)| tail.trim().parse::<i64>().ok())
        .filter(|e| *e > 1_000_000_000);
    Some(epoch)
}

/// What a run reports when its Max account ran out of usage.
pub const LIMIT_STOP: &str =
    "The Max account is out of usage. This waits and restarts on its own when it resets.";

/// What a run that used up its spending limit reports.
pub const BUDGET_STOP: &str =
    "Stopped at this run's spending limit. Continue it to give it more of this month's budget.";

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Shortest key, after its prefix, that counts as an OpenAI or xAI key. Real
/// ones are far longer; this keeps words like `task-list` or `xai-docs` intact.
const MIN_KEY_BODY: usize = 20;

/// Length of the key at the start of `rest`, if one starts there. `prev` is
/// the character before it.
fn key_at(rest: &str, prev: Option<char>) -> Option<usize> {
    let len = rest.find(|c| !is_key_char(c)).unwrap_or(rest.len());
    // Anthropic keys, and the gatekeeper's per-run tokens while they may be
    // live, wherever they appear.
    if rest.starts_with("sk-ant-") || rest.starts_with("decurion-run-") {
        return Some(len);
    }
    // OpenAI keys (`sk-`, `sk-proj-`, ...) and xAI keys (`xai-`) only count at
    // the start of a word, so "risk-..." or "taxai-..." are left alone.
    if prev.is_some_and(is_key_char) {
        return None;
    }
    ["sk-", "xai-"]
        .iter()
        .find(|p| rest.starts_with(**p))
        .filter(|p| len - p.len() >= MIN_KEY_BODY)
        .map(|_| len)
}

/// Hides anything shaped like an Anthropic, OpenAI or xAI key, in case an
/// agent is tricked into echoing its own credential into its output.
pub fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev = None;
    let mut i = 0;
    while let Some(c) = s[i..].chars().next() {
        if let Some(len) = key_at(&s[i..], prev) {
            out.push_str("[redacted]");
            prev = s[i..i + len].chars().next_back();
            i += len;
        } else {
            out.push(c);
            prev = Some(c);
            i += c.len_utf8();
        }
    }
    out
}

fn clip(s: &str, n: usize) -> String {
    let s = redact(s.trim());
    let s = s.as_str();
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// Turns one line of Claude Code `stream-json` output into board events.
pub fn parse_line(line: &str) -> Vec<RunEvent> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![];
    };
    match v["type"].as_str() {
        Some("assistant") => v["message"]["content"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| match p["type"].as_str() {
                        Some("text") => {
                            p["text"]
                                .as_str()
                                .filter(|t| !t.trim().is_empty())
                                .map(|t| RunEvent::Message {
                                    text: clip(t, 2000),
                                })
                        }
                        Some("tool_use") => {
                            let input = &p["input"];
                            let summary = ["command", "file_path", "pattern", "url", "description"]
                                .iter()
                                .find_map(|k| input[*k].as_str())
                                .unwrap_or("");
                            Some(RunEvent::Tool {
                                name: clip(p["name"].as_str().unwrap_or("tool"), 64),
                                summary: clip(summary, 200),
                            })
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Some("result") => {
            let u = &v["usage"];
            // Cached input is most of the input on long runs; count all of it.
            let input = [
                "input_tokens",
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
            ]
            .iter()
            .filter_map(|k| u[*k].as_u64())
            .reduce(|a, b| a + b);
            let text = v["result"].as_str().unwrap_or("");
            let limit = (v["is_error"] == true).then(|| usage_limit(text)).flatten();
            let summary = if v["subtype"] == "error_max_budget_usd" {
                BUDGET_STOP.to_owned()
            } else if limit.is_some() {
                LIMIT_STOP.to_owned()
            } else {
                clip(text, 2000)
            };
            // A crashed run can report $0 after doing paid work; only a clean
            // finish or a budget stop has a cost worth believing.
            let cost_known = v["subtype"] == "success" || v["subtype"] == "error_max_budget_usd";
            let mut out = Vec::new();
            if let Some(reset_epoch) = limit {
                out.push(RunEvent::LimitHit { reset_epoch });
            }
            out.push(RunEvent::Finished {
                ok: v["subtype"] == "success" && v["is_error"] != true,
                summary,
                cost_usd: v["total_cost_usd"].as_f64().filter(|_| cost_known),
                input_tokens: input,
                output_tokens: u["output_tokens"].as_u64(),
            });
            out
        }
        Some("rate_limit_event") => usage_events(&v["rate_limit_info"]),
        _ => vec![],
    }
}

/// Unix seconds, whether Claude Code wrote them as a whole number or not.
fn unix_secs(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))
}

/// Turns a Max plan's `rate_limit_event` into meter readings, one per
/// allowance it mentions.
///
/// Checked against Claude Code 2.1.289 (`SDKRateLimitInfo` in the agent SDK's
/// `sdk.d.ts`, and the CLI that builds it): `status` is the plan's state,
/// `rateLimitType` names the allowance that status is about, `utilization` is
/// a fraction (0.42 is 42%) and `resetsAt` is in Unix seconds. The CLI also
/// sends an undocumented `unifiedWindows` with the five-hour and weekly
/// readings side by side; it is used when present.
fn usage_events(info: &Value) -> Vec<RunEvent> {
    if !info.is_object() {
        return vec![];
    }
    let status = info["status"].as_str().map(|s| clip(s, 40));
    let main = info["rateLimitType"].as_str();
    let pct = |w: &Value| {
        w["utilization"]
            .as_f64()
            .filter(|u| u.is_finite())
            .map(|u| u * 100.0)
    };
    let mut out = Vec::new();
    let mut status_sent = false;
    if let Some(windows) = info["unifiedWindows"].as_object() {
        for (name, w) in windows.iter().take(8) {
            let is_main = Some(name.as_str()) == main;
            status_sent |= is_main;
            out.push(RunEvent::Usage {
                window: Some(clip(name, 40)),
                used_pct: pct(w).or_else(|| pct(info).filter(|_| is_main)),
                resets_at: unix_secs(&w["resetsAt"])
                    .or_else(|| unix_secs(&info["resetsAt"]).filter(|_| is_main)),
                status: status.clone().filter(|_| is_main),
            });
        }
    }
    if !status_sent {
        out.push(RunEvent::Usage {
            window: main.map(|m| clip(m, 40)),
            used_pct: pct(info),
            resets_at: unix_secs(&info["resetsAt"]),
            status,
        });
    }
    out
}

/// What a pi run has reported so far. `pi --mode json` prints one JSON object
/// per line: a session header, then events. Each finished message arrives as
/// `message_end` with the model's usage and its cost in USD; answers being
/// streamed send `message_update` with their usage so far; `agent_settled` is
/// the last event of a run that ended on its own. pi has no result line, so
/// the result is put together from these.
#[derive(Debug, Default)]
pub struct PiRun {
    /// Cost of finished model calls: answers, tools that call a model, and
    /// compaction summaries.
    spent_usd: f64,
    /// Cost so far of the answer being streamed.
    streaming_usd: f64,
    input_tokens: u64,
    output_tokens: u64,
    /// The latest answer's text, why it stopped, and its error if any.
    answer: Option<(String, String, Option<String>)>,
    /// Set once pi says it is done: whether it was aborted.
    settled: Option<bool>,
    /// Something came after `agent_settled`, which pi never prints.
    after_settled: bool,
    /// A line could not be read (pi prints only JSON, so it was cut short or
    /// is not pi's): whatever it cost was not counted.
    unreadable: bool,
}

/// What a pi run reports when its output could not be read.
pub const UNREADABLE_STOP: &str =
    "Stopped: the agent printed output Decurion could not read, so its spending could not be tracked.";

/// A usage report's cost; anything that is not a sane amount counts as zero.
fn usage_cost(u: &Value) -> f64 {
    u["cost"]["total"]
        .as_f64()
        .filter(|c| c.is_finite() && *c >= 0.0)
        .unwrap_or(0.0)
}

impl PiRun {
    /// Money this run has spent so far, as pi reported it.
    pub fn cost_usd(&self) -> f64 {
        self.spent_usd + self.streaming_usd
    }

    fn add_usage(&mut self, u: &Value) {
        self.spent_usd += usage_cost(u);
        // pi reports cached input apart from the rest; count all of it.
        self.input_tokens += ["input", "cacheRead", "cacheWrite"]
            .iter()
            .filter_map(|k| u[*k].as_u64())
            .sum::<u64>();
        self.output_tokens += u["output"].as_u64().unwrap_or(0);
    }

    /// Takes in one line of pi's output and returns the board events in it.
    pub fn parse_line(&mut self, line: &str) -> Vec<RunEvent> {
        if self.settled.is_some() && !line.trim().is_empty() {
            self.after_settled = true;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            self.unreadable |= !line.trim().is_empty();
            return vec![];
        };
        match v["type"].as_str() {
            Some("message_update") => {
                self.streaming_usd = usage_cost(&v["usage"]);
                vec![]
            }
            Some("message_end") => {
                self.streaming_usd = 0.0;
                let m = &v["message"];
                match m["role"].as_str() {
                    Some("assistant") => self.answer(m),
                    // A tool that called a model itself reports that usage.
                    Some("toolResult") if m["usage"].is_object() => {
                        self.add_usage(&m["usage"]);
                        vec![]
                    }
                    _ => vec![],
                }
            }
            Some("compaction_end") if v["result"]["usage"].is_object() => {
                self.add_usage(&v["result"]["usage"]);
                vec![]
            }
            Some("agent_settled") => {
                self.settled = Some(v["aborted"] == true);
                vec![]
            }
            _ => vec![],
        }
    }

    fn answer(&mut self, m: &Value) -> Vec<RunEvent> {
        self.add_usage(&m["usage"]);
        let parts = m["content"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let mut text = String::new();
        let events = parts
            .iter()
            .filter_map(|p| match p["type"].as_str() {
                Some("text") => p["text"]
                    .as_str()
                    .filter(|t| !t.trim().is_empty())
                    .map(|t| {
                        text.push_str(t);
                        RunEvent::Message {
                            text: clip(t, 2000),
                        }
                    }),
                Some("toolCall") => {
                    let args = &p["arguments"];
                    let summary = ["command", "path", "pattern", "url", "description"]
                        .iter()
                        .find_map(|k| args[*k].as_str())
                        .unwrap_or("");
                    Some(RunEvent::Tool {
                        name: clip(p["name"].as_str().unwrap_or("tool"), 64),
                        summary: clip(summary, 200),
                    })
                }
                _ => None,
            })
            .collect();
        self.answer = Some((
            text,
            m["stopReason"].as_str().unwrap_or("").to_owned(),
            m["errorMessage"].as_str().map(str::to_owned),
        ));
        events
    }

    /// Whether the runner must stop the run now: it has spent its budget and
    /// another model call may still come, or its spending can no longer be
    /// tracked. A last answer that pushed it over is let finish, since pi
    /// makes no further call after an answer that did not ask for a tool.
    pub fn must_stop(&self, budget_usd: Option<f64>) -> bool {
        let finished_answering = self.streaming_usd == 0.0
            && self
                .answer
                .as_ref()
                .is_some_and(|(_, stop, _)| stop != "toolUse");
        self.unreadable || (over_budget(self.cost_usd(), budget_usd) && !finished_answering)
    }

    /// The result of a run whose output has ended, or `None` if pi never said
    /// it was done (it crashed or was killed). Its cost is not believed if
    /// anything came after; the hub then charges the whole amount set aside.
    pub fn finished(&self) -> Option<RunEvent> {
        let aborted = self.settled?;
        let (ok, summary) = match &self.answer {
            Some((text, stop, _)) if stop == "stop" => (!aborted, clip(text, 2000)),
            Some((_, stop, error)) if stop == "error" || stop == "aborted" => (
                false,
                clip(error.as_deref().unwrap_or("the model request failed"), 2000),
            ),
            Some((text, stop, _)) if stop == "length" => (
                false,
                format!("Stopped at the model's output limit: {}", clip(text, 2000)),
            ),
            _ => (false, "pi ended without an answer".to_owned()),
        };
        Some(RunEvent::Finished {
            ok,
            summary,
            cost_usd: (!self.after_settled && !self.unreadable).then(|| self.cost_usd()),
            input_tokens: Some(self.input_tokens),
            output_tokens: Some(self.output_tokens),
        })
    }

    /// The result of a run the runner stopped (see [`PiRun::must_stop`]). The cost is
    /// everything pi reported up to then.
    pub fn budget_stop(&self) -> RunEvent {
        if self.unreadable {
            // The hub charges everything set aside for the run.
            return RunEvent::Finished {
                ok: false,
                summary: UNREADABLE_STOP.to_owned(),
                cost_usd: None,
                input_tokens: Some(self.input_tokens),
                output_tokens: Some(self.output_tokens),
            };
        }
        RunEvent::Finished {
            ok: false,
            summary: BUDGET_STOP.to_owned(),
            cost_usd: Some(self.cost_usd()),
            input_tokens: Some(self.input_tokens),
            output_tokens: Some(self.output_tokens),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_passed_by_name_and_no_host_paths_are_mounted() {
        let a = run_args(
            7,
            Path::new("C:\\data\\worktrees\\demo 7"),
            "sid",
            false,
            Credential::Max(Profile::A),
            None,
        )
        .unwrap_or_default();
        assert!(a.contains(&"-i".to_string()));
        let joined = a.join(" ");
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-e" && w[1] == "CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(!joined.contains("sk-ant"));
        let mounts: Vec<&String> = a
            .windows(2)
            .filter(|w| w[0] == "--mount")
            .map(|w| &w[1])
            .collect();
        assert_eq!(mounts.len(), 2);
        assert!(mounts[0].contains("dst=/work"));
        assert!(mounts[1].starts_with("type=volume,"));
        assert!(
            !joined.contains(".claude")
                && !joined.contains(".ssh")
                && !joined.contains("docker.sock")
        );
        assert!(joined.contains("--session-id sid"));
    }

    #[test]
    fn resume_uses_the_same_session() {
        let a = run_args(
            7,
            Path::new("/w"),
            "sid",
            true,
            Credential::Max(Profile::A),
            None,
        )
        .unwrap_or_default();
        assert_eq!(
            a[a.len() - 2..],
            ["--resume".to_string(), "sid".to_string()]
        );
    }

    #[test]
    fn pay_per_use_runs_get_a_gatekeeper_token_and_a_budget_only() {
        let a = run_args(
            7,
            Path::new("/w"),
            "sid",
            false,
            Credential::PayPerUse,
            Some(9.5),
        )
        .unwrap_or_default();
        // Both by name: the values come from the docker process's environment.
        for var in ["ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL"] {
            assert!(a.windows(2).any(|w| w[0] == "-e" && w[1] == var), "{var}");
        }
        let joined = a.join(" ");
        assert!(!joined.contains("sk-ant") && !joined.contains("http"));
        assert!(!a.iter().any(|x| x == "CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "--max-budget-usd" && w[1] == "9.50"));
        let m = run_args(
            7,
            Path::new("/w"),
            "sid",
            false,
            Credential::Max(Profile::B),
            None,
        )
        .unwrap_or_default();
        assert!(!m.iter().any(|x| x == "ANTHROPIC_API_KEY"
            || x == "ANTHROPIC_BASE_URL"
            || x == "--max-budget-usd"));
        assert_eq!(
            Credential::Max(Profile::B).target(),
            "decurion/claude-profile-B"
        );
    }

    #[test]
    fn a_max_usage_limit_is_recognised_with_its_reset_time() {
        let l = r#"{"type":"result","subtype":"success","is_error":true,"result":"Claude AI usage limit reached|1760900400"}"#;
        assert_eq!(
            parse_line(l)[0],
            RunEvent::LimitHit {
                reset_epoch: Some(1_760_900_400)
            }
        );
        assert!(
            matches!(&parse_line(l)[1], RunEvent::Finished { ok: false, summary, .. } if summary == LIMIT_STOP)
        );
        let newer = r#"{"type":"result","subtype":"success","is_error":true,"result":"You've hit your limit · resets 3pm (America/Phoenix)"}"#;
        assert_eq!(
            parse_line(newer)[0],
            RunEvent::LimitHit { reset_epoch: None }
        );
        // An agent merely talking about limits in a successful result is not one.
        let fine = r#"{"type":"result","subtype":"success","is_error":false,"result":"Added a usage limit check"}"#;
        assert!(!parse_line(fine)
            .iter()
            .any(|e| matches!(e, RunEvent::LimitHit { .. })));
        for other in [
            "API Error: 429 rate limit reached",
            "Output token limit reached",
            "You have reached your specified API usage limits",
        ] {
            assert_eq!(usage_limit(other), None, "{other}");
        }
    }

    #[test]
    fn run_tokens_are_redacted_too() {
        assert_eq!(
            redact("my key is decurion-run-0a1b2c3d ok"),
            "my key is [redacted] ok"
        );
    }

    #[test]
    fn a_crash_result_does_not_claim_to_be_free() {
        let l = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"total_cost_usd":0}"#;
        assert!(matches!(
            &parse_line(l)[..],
            [RunEvent::Finished { cost_usd: None, .. }]
        ));
    }

    #[test]
    fn a_budget_stop_explains_itself() {
        let l = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"total_cost_usd":9.1}"#;
        assert!(matches!(
            &parse_line(l)[..],
            [RunEvent::Finished { ok: false, summary, cost_usd: Some(_), .. }] if summary == BUDGET_STOP
        ));
    }

    #[test]
    fn mount_paths_with_commas_are_refused() {
        assert!(run_args(
            7,
            Path::new("C:\\a,b\\demo-7"),
            "sid",
            false,
            Credential::Max(Profile::A),
            None
        )
        .is_err());
    }

    #[test]
    fn parses_text_tools_and_result() {
        let l = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking"},{"type":"tool_use","name":"Bash","input":{"command":"git log"}}]}}"#;
        assert_eq!(
            parse_line(l),
            vec![
                RunEvent::Message {
                    text: "Looking".into()
                },
                RunEvent::Tool {
                    name: "Bash".into(),
                    summary: "git log".into()
                }
            ]
        );
        let ok = r#"{"type":"result","subtype":"success","is_error":false,"result":"DONE","total_cost_usd":0.0421,"usage":{"input_tokens":1200,"cache_read_input_tokens":50000,"output_tokens":340}}"#;
        assert_eq!(
            parse_line(ok),
            vec![RunEvent::Finished {
                ok: true,
                summary: "DONE".into(),
                cost_usd: Some(0.0421),
                input_tokens: Some(51200),
                output_tokens: Some(340),
            }]
        );
        let bad = r#"{"type":"result","subtype":"success","is_error":true,"result":"Failed to authenticate"}"#;
        assert!(matches!(
            parse_line(bad)[..],
            [RunEvent::Finished { ok: false, .. }]
        ));
        assert!(parse_line("not json").is_empty());
        assert!(parse_line(r#"{"type":"system"}"#).is_empty());
    }

    #[test]
    fn tokens_are_redacted_from_agent_output() {
        let l = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"token is sk-ant-x_1-z ok"}]}}"#;
        assert_eq!(
            parse_line(l),
            vec![RunEvent::Message {
                text: "token is [redacted] ok".into()
            }]
        );
    }

    #[test]
    fn plan_usage_becomes_meter_readings() {
        // As Claude Code 2.1.289 prints it on a Max plan, mid-week.
        let l = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1760954400,"rateLimitType":"five_hour","utilization":0.31,"isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.31,"resetsAt":1760954400},"seven_day":{"utilization":0.42,"resetsAt":1761300000}}},"uuid":"0b3c4b8e-1111-4222-8333-944455556666","session_id":"5e0a2c1d-aaaa-4bbb-8ccc-ddddeeeeffff"}"#;
        let got = parse_line(l);
        assert_eq!(got.len(), 2);
        assert!(got.contains(&RunEvent::Usage {
            window: Some("five_hour".into()),
            used_pct: Some(31.0),
            resets_at: Some(1760954400),
            status: Some("allowed".into()),
        }));
        assert!(got.iter().any(|e| matches!(e,
            RunEvent::Usage { window: Some(w), used_pct: Some(p), resets_at: Some(1761300000), status: None }
                if w == "seven_day" && (p - 42.0).abs() < 1e-9)));
    }

    #[test]
    fn a_used_up_plan_reads_as_rejected() {
        // Without the per-window detail: just the limit that was hit.
        let l = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1761300000,"rateLimitType":"seven_day","utilization":1.0,"overageStatus":"rejected","overageDisabledReason":"overage_not_provisioned"},"uuid":"u","session_id":"s"}"#;
        assert_eq!(
            parse_line(l),
            vec![RunEvent::Usage {
                window: Some("seven_day".into()),
                used_pct: Some(100.0),
                resets_at: Some(1761300000),
                status: Some("rejected".into()),
            }]
        );
        // A bare status still gets through.
        let l = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning"}}"#;
        assert_eq!(
            parse_line(l),
            vec![RunEvent::Usage {
                window: None,
                used_pct: None,
                resets_at: None,
                status: Some("allowed_warning".into()),
            }]
        );
        assert!(parse_line(r#"{"type":"rate_limit_event"}"#).is_empty());
    }

    #[test]
    fn clip_respects_char_boundaries() {
        assert_eq!(clip("héllo", 2), "hé…");
        assert_eq!(clip("hi", 5), "hi");
    }

    #[test]
    fn claude_arguments_are_unchanged() {
        let a = run_args(
            7,
            Path::new("/w"),
            "sid",
            false,
            Credential::PayPerUse,
            Some(9.5),
        )
        .unwrap_or_default();
        // As on main: the pay-per-use run gets the gatekeeper token and address.
        let want = "run -i --rm --name decurion-thread-7 \
--mount type=bind,src=/w,dst=/work --mount type=volume,src=decurion-home-7,dst=/home/agent \
--cap-drop ALL --security-opt no-new-privileges --memory 4g --pids-limit 1024 \
-e ANTHROPIC_API_KEY -e ANTHROPIC_BASE_URL -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory \
-e GIT_CONFIG_VALUE_0=/work -e GIT_AUTHOR_NAME=decurion-agent \
-e GIT_AUTHOR_EMAIL=agent@decurion.invalid -e GIT_COMMITTER_NAME=decurion-agent \
-e GIT_COMMITTER_EMAIL=agent@decurion.invalid decurion/claude-worker:dev \
-p --output-format stream-json --verbose --permission-mode bypassPermissions \
--session-id sid --max-budget-usd 9.50";
        assert_eq!(a.join(" "), want);
    }

    const SID: &str = "3f2c9a7e-1b2d-4c5e-8f90-123456789abc";

    #[test]
    fn pi_runs_are_sealed_the_same_way_and_get_their_key_by_name() {
        let a = pi_run_args(
            7,
            Path::new("C:\\data\\worktrees\\demo 7"),
            SID,
            Credential::OpenAi,
        )
        .unwrap_or_default();
        let c = run_args(
            7,
            Path::new("C:\\data\\worktrees\\demo 7"),
            SID,
            false,
            Credential::OpenAi,
            None,
        )
        .unwrap_or_default();
        // Everything up to the image is the same as for Claude Code.
        let image = a.iter().position(|x| x == PI_IMAGE).unwrap_or(0);
        assert!(image > 0);
        assert_eq!(a[..image], c[..image]);
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-e" && w[1] == "OPENAI_API_KEY"));
        assert_eq!(a.iter().filter(|x| *x == "--mount").count(), 2);
        assert_eq!(
            a[image + 1..].join(" "),
            format!(
                "--mode json --provider openai --model gpt-5.5 \
--session-dir /home/agent/.pi/sessions --session-id {SID} \
--no-extensions --no-skills --no-prompt-templates --no-approve"
            )
        );
        let x = pi_run_args(7, Path::new("/w"), SID, Credential::Xai).unwrap_or_default();
        assert!(x.windows(2).any(|w| w[0] == "-e" && w[1] == "XAI_API_KEY"));
        assert!(x.join(" ").contains("--provider xai --model grok-4.7"));
        assert!(!x.iter().any(|v| v == "OPENAI_API_KEY"));
    }

    #[test]
    fn pi_refuses_claude_keys_odd_session_ids_and_comma_paths() {
        let w = Path::new("/w");
        assert!(pi_run_args(7, w, SID, Credential::PayPerUse).is_err());
        assert!(pi_run_args(7, w, SID, Credential::Max(Profile::A)).is_err());
        assert!(pi_run_args(7, w, "-rf", Credential::Xai).is_err());
        assert!(pi_run_args(7, w, "a b", Credential::Xai).is_err());
        assert!(pi_run_args(7, Path::new("C:\\a,b"), SID, Credential::Xai).is_err());
    }

    #[test]
    fn each_paid_account_has_its_own_key() {
        let o = Credential::for_account(Account::OpenAi);
        assert_eq!(o, Credential::OpenAi);
        assert_eq!(o.target(), "decurion/openai-api");
        assert_eq!(o.env_fallback(), "DECURION_OPENAI_API_KEY");
        let x = Credential::for_account(Account::Xai);
        assert_eq!(x.vars(), ["XAI_API_KEY"]);
        assert_eq!(x.target(), "decurion/xai-api");
        assert_eq!(x.env_fallback(), "DECURION_XAI_API_KEY");
        assert_eq!(
            Credential::for_account(Account::B),
            Credential::Max(Profile::B)
        );
        assert_eq!(Credential::for_account(Account::Api).pi_model(), None);
    }

    // Lines as pi 1.3.0 prints them (`pi --mode json`), shortened: unused
    // fields are left out.
    const PI_HEADER: &str = r#"{"type":"session","version":3,"id":"3f2c9a7e-1b2d-4c5e-8f90-123456789abc","timestamp":"2026-10-17T21:34:49.344Z","cwd":"/work"}"#;
    const PI_STREAMING: &str = r#"{"type":"message_update","usage":{"input":200,"output":40,"cacheRead":1000,"cacheWrite":0,"reasoning":0,"totalTokens":1240,"cost":{"input":0.001,"output":0.0012,"cacheRead":0.0005,"cacheWrite":0,"total":0.0027}},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"Checking"}}"#;
    const PI_TOOL_TURN: &str = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Checking the repo."},{"type":"toolCall","id":"call_1","name":"bash","arguments":{"command":"git status"}}],"api":"openai-responses","provider":"openai","model":"gpt-5.5","usage":{"input":200,"output":40,"cacheRead":1000,"cacheWrite":0,"reasoning":0,"totalTokens":1240,"cost":{"input":0.001,"output":0.0012,"cacheRead":0.0005,"cacheWrite":0,"total":0.0027}},"stopReason":"toolUse","timestamp":1792272889428}}"#;
    const PI_TOOL_START: &str = r#"{"type":"tool_execution_start","toolCallId":"call_1","toolName":"bash","args":{"command":"git status"}}"#;
    const PI_TOOL_RESULT: &str = r#"{"type":"message_end","message":{"role":"toolResult","toolCallId":"call_1","toolName":"bash","content":[{"type":"text","text":"On branch decurion/thread-7"}],"isError":false,"timestamp":1792272889488}}"#;
    const PI_ANSWER: &str = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"done"},{"type":"text","text":"All done: committed the fix."}],"api":"openai-responses","provider":"openai","model":"gpt-5.5","usage":{"input":300,"output":60,"cacheRead":1000,"cacheWrite":0,"totalTokens":1360,"cost":{"input":0.0015,"output":0.0018,"cacheRead":0.0005,"cacheWrite":0,"total":0.0038}},"stopReason":"stop","timestamp":1792272889491}}"#;
    const PI_TURN_END: &str = r#"{"type":"turn_end","message":{"role":"assistant","content":[],"usage":{"cost":{"total":5}},"stopReason":"stop"},"toolResults":[]}"#;
    const PI_SETTLED: &str = r#"{"type":"agent_settled","aborted":false}"#;

    fn feed(run: &mut PiRun, lines: &[&str]) -> Vec<RunEvent> {
        lines.iter().flat_map(|l| run.parse_line(l)).collect()
    }

    #[test]
    fn parses_pi_text_tools_and_result() {
        let mut run = PiRun::default();
        let events = feed(
            &mut run,
            &[
                PI_HEADER,
                PI_STREAMING,
                PI_TOOL_TURN,
                PI_TOOL_START,
                PI_TOOL_RESULT,
                PI_ANSWER,
                PI_TURN_END,
                PI_SETTLED,
            ],
        );
        assert_eq!(
            events,
            vec![
                RunEvent::Message {
                    text: "Checking the repo.".into()
                },
                RunEvent::Tool {
                    name: "bash".into(),
                    summary: "git status".into()
                },
                RunEvent::Message {
                    text: "All done: committed the fix.".into()
                },
            ]
        );
        // turn_end repeats the message; only message_end is counted.
        let Some(RunEvent::Finished {
            ok,
            summary,
            cost_usd,
            input_tokens,
            output_tokens,
        }) = run.finished()
        else {
            panic!("not a result");
        };
        assert!(ok);
        assert_eq!(summary, "All done: committed the fix.");
        assert!((cost_usd.unwrap_or(0.0) - 0.0065).abs() < 1e-12);
        assert_eq!(input_tokens, Some(2500));
        assert_eq!(output_tokens, Some(100));
    }

    #[test]
    fn a_failed_pi_request_explains_itself_and_keeps_its_cost() {
        let err = r#"{"type":"message_end","message":{"role":"assistant","content":[],"api":"openai-responses","provider":"xai","model":"grok-4.7","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"error","timestamp":1792273064278,"errorMessage":"401 Incorrect API key provided: xai-abcdefghijklmnopqrstuvwxyz0123","durationMs":308}}"#;
        let retry = r#"{"type":"auto_retry_end","success":false,"attempt":3,"finalError":"Connection error."}"#;
        let mut run = PiRun::default();
        assert!(feed(&mut run, &[PI_HEADER, PI_TOOL_TURN, err, retry, PI_SETTLED]).len() == 2);
        assert!(matches!(
            run.finished(),
            Some(RunEvent::Finished { ok: false, summary, cost_usd: Some(c), .. })
                if summary == "401 Incorrect API key provided: [redacted]" && c > 0.0
        ));
    }

    #[test]
    fn a_pi_run_that_did_not_settle_has_no_result_or_trusted_cost() {
        // Crashed or killed: no result, so the runner reports the container's
        // own error and the hub charges the money set aside.
        let mut crashed = PiRun::default();
        feed(&mut crashed, &[PI_HEADER, PI_TOOL_TURN]);
        assert_eq!(crashed.finished(), None);
        // pi prints nothing after agent_settled; anything there is suspect.
        let mut forged = PiRun::default();
        feed(&mut forged, &[PI_ANSWER, PI_SETTLED, PI_ANSWER]);
        assert!(matches!(
            forged.finished(),
            Some(RunEvent::Finished { cost_usd: None, .. })
        ));
        let mut aborted = PiRun::default();
        feed(
            &mut aborted,
            &[PI_ANSWER, r#"{"type":"agent_settled","aborted":true}"#],
        );
        assert!(matches!(
            aborted.finished(),
            Some(RunEvent::Finished {
                ok: false,
                cost_usd: Some(_),
                ..
            })
        ));
        let mut empty = PiRun::default();
        feed(&mut empty, &[PI_HEADER, PI_SETTLED]);
        assert!(matches!(
            empty.finished(),
            Some(RunEvent::Finished { ok: false, summary, .. }) if summary == "pi ended without an answer"
        ));
    }

    #[test]
    fn pi_counts_compaction_nested_and_streaming_costs() {
        let mut run = PiRun::default();
        run.parse_line(PI_STREAMING);
        assert!((run.cost_usd() - 0.0027).abs() < 1e-12);
        // The finished message replaces its streaming estimate.
        run.parse_line(PI_TOOL_TURN);
        assert!((run.cost_usd() - 0.0027).abs() < 1e-12);
        run.parse_line(r#"{"type":"compaction_end","reason":"threshold","result":{"summary":"s","firstKeptEntryId":"a","tokensBefore":150000,"usage":{"input":150000,"output":2000,"cacheRead":0,"cacheWrite":0,"totalTokens":152000,"cost":{"total":0.81}}},"aborted":false,"willRetry":false}"#);
        run.parse_line(r#"{"type":"message_end","message":{"role":"toolResult","toolCallId":"c","toolName":"codemode","content":[],"usage":{"input":10,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":11,"cost":{"total":0.01}},"isError":false,"timestamp":1}}"#);
        // A bad report counts as nothing rather than breaking the total.
        run.parse_line(r#"{"type":"message_end","message":{"role":"assistant","content":[],"usage":{"cost":{"total":-5}},"stopReason":"stop"}}"#);
        assert!((run.cost_usd() - 0.8227).abs() < 1e-9);
    }

    #[test]
    fn budget_is_reached_at_or_past_the_limit_only() {
        assert!(!over_budget(8.99, Some(9.0)));
        assert!(over_budget(9.0, Some(9.0)));
        assert!(over_budget(12.0, Some(9.0)));
        assert!(!over_budget(1e9, None));
        let mut run = PiRun::default();
        run.parse_line(PI_TOOL_TURN);
        assert!(!over_budget(run.cost_usd(), Some(0.005)));
        // A long answer still streaming can reach the budget on its own.
        run.parse_line(PI_STREAMING);
        assert!(over_budget(run.cost_usd(), Some(0.005)));
        assert!(matches!(
            run.budget_stop(),
            RunEvent::Finished { ok: false, summary, cost_usd: Some(c), .. }
                if summary == BUDGET_STOP && (c - 0.0054).abs() < 1e-12
        ));
    }

    #[test]
    fn a_last_answer_over_budget_may_finish_but_another_call_may_not_start() {
        // Over budget after asking for a tool: another call would follow.
        let mut tool = PiRun::default();
        tool.parse_line(PI_TOOL_TURN);
        assert!(tool.must_stop(Some(0.001)));
        // Over budget on the final answer: nothing more is coming.
        let mut last = PiRun::default();
        last.parse_line(PI_ANSWER);
        assert!(last.cost_usd() > 0.001);
        assert!(!last.must_stop(Some(0.001)));
        // Any further call starts streaming, and is stopped.
        last.parse_line(PI_STREAMING);
        assert!(last.must_stop(Some(0.001)));
        assert!(!PiRun::default().must_stop(Some(0.001)));
    }

    #[test]
    fn unreadable_pi_output_stops_the_run_and_is_not_trusted() {
        let mut run = PiRun::default();
        run.parse_line(PI_TOOL_TURN);
        assert!(!run.must_stop(Some(9.0)));
        // A line cut short at the runner's limit: its cost is unknown.
        run.parse_line(&PI_ANSWER[..PI_ANSWER.len() / 2]);
        assert!(run.must_stop(Some(9.0)));
        assert!(matches!(
            run.budget_stop(),
            RunEvent::Finished { ok: false, summary, cost_usd: None, .. } if summary == UNREADABLE_STOP
        ));
        run.parse_line(PI_SETTLED);
        assert!(matches!(
            run.finished(),
            Some(RunEvent::Finished { cost_usd: None, .. })
        ));
        // Blank lines are fine.
        let mut blank = PiRun::default();
        blank.parse_line("  ");
        assert!(!blank.must_stop(Some(9.0)));
    }

    #[test]
    fn openai_and_xai_keys_are_redacted_but_ordinary_words_are_not() {
        let proj = "sk-proj-Ab3_dE-fGhIjKlMnOpQrStUvWxYz0123456789";
        let legacy = "sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijkl";
        let xai = "xai-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefgh";
        assert_eq!(
            redact(&format!("key={proj}, old {legacy}; grok:{xai}.")),
            "key=[redacted], old [redacted]; grok:[redacted]."
        );
        assert_eq!(redact("token is sk-ant-x_1-z ok"), "token is [redacted] ok");
        for plain in [
            "risk-assessment-framework-for-everything",
            "task-1234567890abcdefghijklmnop",
            "see xai-docs and sk-learn",
            "naïve café sk-",
            "",
        ] {
            assert_eq!(redact(plain), plain);
        }
        let l = format!(
            r#"{{"type":"message_end","message":{{"role":"assistant","content":[{{"type":"text","text":"my key: {proj}"}}],"usage":{{}},"stopReason":"stop"}}}}"#
        );
        assert_eq!(
            PiRun::default().parse_line(&l),
            vec![RunEvent::Message {
                text: "my key: [redacted]".into()
            }]
        );
    }
}
