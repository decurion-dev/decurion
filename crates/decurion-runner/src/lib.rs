//! Pieces of the runner that do not need Docker or a hub, kept here so they
//! can be tested directly.

use decurion_proto::goal::{HelperResult, MAX_PAGE_BYTES};
use decurion_proto::{Profile, RunEvent};
use serde_json::Value;
use std::path::Path;

pub mod gatekeeper;

pub const IMAGE: &str = "decurion/claude-worker:dev";

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

/// The one credential an agent gets: its Max profile's Claude token, or a
/// per-run token for the gatekeeper, which holds the pay-per-use API key on
/// the host. Never both, and never the API key itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    Max(Profile),
    PayPerUse,
}

impl Credential {
    /// The environment variables the container gets for it.
    pub fn vars(self) -> &'static [&'static str] {
        match self {
            Credential::Max(_) => &["CLAUDE_CODE_OAUTH_TOKEN"],
            Credential::PayPerUse => &["ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL"],
        }
    }

    /// Where it is read from on machines without Credential Manager.
    pub fn env_fallback(self) -> String {
        match self {
            Credential::Max(p) => format!("DECURION_CLAUDE_TOKEN_{}", p.as_str()),
            Credential::PayPerUse => "DECURION_ANTHROPIC_API_KEY".to_owned(),
        }
    }

    /// Where it is kept in Windows Credential Manager.
    pub fn target(self) -> String {
        match self {
            Credential::Max(p) => format!("decurion/claude-profile-{}", p.as_str()),
            Credential::PayPerUse => "decurion/anthropic-api".to_owned(),
        }
    }
}

pub fn container_name(thread_id: i64) -> String {
    format!("decurion-thread-{thread_id}")
}

pub fn home_volume(thread_id: i64) -> String {
    format!("decurion-home-{thread_id}")
}

/// One coordinator or checker job. Its own prefix, so a restarted runner can
/// find and stop helpers left running as well as threads.
pub fn helper_container_name(job_id: i64) -> String {
    format!("decurion-helper-{job_id}")
}

/// The coordinator's home, kept for the whole goal so every turn can resume
/// the same Claude Code session.
pub fn goal_home_volume(goal_id: i64) -> String {
    format!("decurion-goal-home-{goal_id}")
}

/// A checker's home, thrown away with its copy of the code.
pub fn check_home_volume(job_id: i64) -> String {
    format!("decurion-check-home-{job_id}")
}

/// A designer's home, thrown away with its copy of the code.
pub fn design_home_volume(job_id: i64) -> String {
    format!("decurion-design-home-{job_id}")
}

/// A branch name the hub would hand out (`decurion/thread-7`). Anything else
/// is refused before it reaches a git command line.
pub fn valid_branch(b: &str) -> bool {
    !b.is_empty()
        && !b.starts_with(['-', '/'])
        && !b.contains("..")
        && b.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'))
}

const RULES: &str = "You are working in your own copy of the code on its own branch. \
Commit your work to this branch as you go. Do not push and do not open pull requests; \
Decurion does that after review.";

/// Said again with follow-up work, which arrives long after the first rules.
const RULES_AGAIN: &str = "Commit your work to this branch as you go. \
Do not push and do not open pull requests.";

/// What the agent is told on stdin. A resumed session already has the task, so
/// it only gets what to do next: the follow-up if there is one (a reviewer's
/// notes, say), otherwise a note that its container was restarted.
pub fn prompt_for(task: &str, resume: bool, follow_up: Option<&str>) -> String {
    let follow_up = follow_up.map(str::trim).filter(|f| !f.is_empty());
    match (resume, follow_up) {
        (true, Some(f)) => format!("{f}\n\n{RULES_AGAIN}"),
        (true, None) => {
            "Your container was restarted. Continue the same task from where it stopped. \
Check `git status` and `git log` first and do not redo finished steps."
                .to_owned()
        }
        // A new session has never seen the task, so it gets both.
        (false, Some(f)) => format!("{task}\n\n{f}\n\n{RULES}"),
        (false, None) => format!("{task}\n\n{RULES}"),
    }
}

/// What a container sees: its code at /work and its own home volume.
pub struct Sandbox<'a> {
    pub name: &'a str,
    pub src: &'a Path,
    /// The coordinator only reads the code.
    pub read_only: bool,
    pub home_volume: &'a str,
    /// An empty host folder mounted writable at /out, for jobs that hand back
    /// a page.
    pub out: Option<&'a Path>,
}

/// Arguments for `docker run`. Credentials are passed by name only, so they
/// never appear on a command line, and the task goes in on stdin (`-i`), so a long
/// task cannot hit the Windows command-line limit.
pub fn run_args(
    thread_id: i64,
    worktree: &Path,
    session_id: &str,
    resume: bool,
    cred: Credential,
    budget_usd: Option<f64>,
) -> anyhow::Result<Vec<String>> {
    agent_args(
        &Sandbox {
            name: &container_name(thread_id),
            src: worktree,
            read_only: false,
            home_volume: &home_volume(thread_id),
            out: None,
        },
        session_id,
        resume,
        cred,
        budget_usd,
    )
}

/// Arguments for a coordinator or checker container: the same sealed box as
/// a work run, minus the tools the hub took away for this job.
pub fn helper_run_args(
    sb: &Sandbox<'_>,
    session_id: &str,
    resume: bool,
    cred: Credential,
    budget_usd: Option<f64>,
    disallowed_tools: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut a = agent_args(sb, session_id, resume, cred, budget_usd)?;
    if let Some(bad) = disallowed_tools.iter().find(|t| !valid_tool_name(t)) {
        anyhow::bail!("refusing odd tool name {bad:?}");
    }
    if !disallowed_tools.is_empty() {
        a.push("--disallowedTools".into());
        a.push(disallowed_tools.join(","));
    }
    // The code in /work was written by an agent. Its project settings, hooks
    // and MCP servers must not steer the helper that reads or checks it.
    a.extend(["--setting-sources", "user", "--strict-mcp-config"].map(String::from));
    Ok(a)
}

/// Plain tool names and patterns such as `mcp__x__*` or `Bash(git:*)`: no
/// commas or spaces that would split the list, and nothing that looks like
/// a flag.
fn valid_tool_name(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 100
        && !t.starts_with('-')
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '*' | '(' | ')' | '-' | ':'))
}

/// A host path for `--mount`, which docker reads as comma-separated fields.
fn mount_src(p: &Path) -> anyhow::Result<String> {
    let src = p.display().to_string();
    if src.contains([',', '"']) {
        anyhow::bail!("workspace path {src} contains a comma or quote");
    }
    Ok(src)
}

fn agent_args(
    sb: &Sandbox<'_>,
    session_id: &str,
    resume: bool,
    cred: Credential,
    budget_usd: Option<f64>,
) -> anyhow::Result<Vec<String>> {
    let src = mount_src(sb.src)?;
    let ro = if sb.read_only { ",readonly" } else { "" };
    let mut a: Vec<String> = [
        "run",
        "-i",
        "--rm",
        "--name",
        sb.name,
        "--mount",
        &format!("type=bind,src={src},dst=/work{ro}"),
        "--mount",
        &format!("type=volume,src={},dst=/home/agent", sb.home_volume),
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
    .collect();
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
            IMAGE,
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
        ]
        .map(str::to_owned),
    );
    if let Some(out) = sb.out {
        // Before the image name: everything after it goes to Claude Code.
        let at = a.iter().position(|x| x == IMAGE).unwrap_or(a.len());
        let mount = format!("type=bind,src={},dst=/out", mount_src(out)?);
        a.splice(at..at, ["--mount".to_owned(), mount]);
    }
    a.push(if resume { "--resume" } else { "--session-id" }.into());
    a.push(session_id.into());
    if let Some(b) = budget_usd {
        // Claude Code stops itself at this amount; the hub set it aside.
        a.push("--max-budget-usd".into());
        a.push(format!("{b:.2}"));
    }
    Ok(a)
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

/// Hides anything shaped like an Anthropic token, in case an agent is tricked
/// into echoing its own credential into its output.
pub fn redact(s: &str) -> String {
    let mut out = s.to_owned();
    // Real keys, and the gatekeeper's per-run tokens while they may be live.
    for prefix in ["sk-ant-", "decurion-run-"] {
        let mut done = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(i) = rest.find(prefix) {
            done.push_str(&rest[..i]);
            done.push_str("[redacted]");
            let tail = &rest[i..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(tail.len());
            rest = &tail[end..];
        }
        done.push_str(rest);
        out = done;
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
                                name: p["name"].as_str().unwrap_or("tool").to_owned(),
                                summary: clip(summary, 200),
                            })
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Some("result") => {
            let o = outcome(&v);
            let text = v["result"].as_str().unwrap_or("");
            let limit = (v["is_error"] == true).then(|| usage_limit(text)).flatten();
            let summary = if o.budget_stop {
                BUDGET_STOP.to_owned()
            } else if limit.is_some() {
                LIMIT_STOP.to_owned()
            } else {
                clip(text, 2000)
            };
            let mut out = Vec::new();
            if let Some(reset_epoch) = limit {
                out.push(RunEvent::LimitHit { reset_epoch });
            }
            out.push(RunEvent::Finished {
                ok: o.ok,
                summary,
                cost_usd: o.cost_usd,
                input_tokens: o.input_tokens,
                output_tokens: o.output_tokens,
            });
            out
        }
        _ => vec![],
    }
}

/// How a Claude Code `result` line ended, read the same way for work runs and
/// helper jobs.
struct Outcome {
    ok: bool,
    budget_stop: bool,
    cost_usd: Option<f64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

fn outcome(v: &Value) -> Outcome {
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
    let budget_stop = v["subtype"] == "error_max_budget_usd";
    // A crashed run can report $0 after doing paid work; only a clean
    // finish or a budget stop has a cost worth believing.
    let cost_known = v["subtype"] == "success" || budget_stop;
    Outcome {
        ok: v["subtype"] == "success" && v["is_error"] != true,
        budget_stop,
        cost_usd: v["total_cost_usd"].as_f64().filter(|_| cost_known),
        input_tokens: input,
        output_tokens: u["output_tokens"].as_u64(),
    }
}

/// The most of a helper's answer passed on. A coordinator's plan can run to
/// tens of kilobytes, far past what the board shows of a work run.
pub const MAX_ANSWER: usize = 256 << 10;

/// Keeps the last `max` bytes of `s`, marked with "…" where it was cut and
/// without splitting a character. The end is kept because a helper's answer
/// closes with the JSON the hub reads.
fn tail_bytes(s: &str, max: usize) -> String {
    const CUT: &str = "…";
    if s.len() <= max {
        return s.to_owned();
    }
    let mut start = s.len() - max.saturating_sub(CUT.len());
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("{CUT}{}", &s[start..])
}

/// Reads a helper job's `result` line: its whole answer, which the hub parses,
/// instead of the clipped summary [`parse_line`] gives the board. Other lines
/// give `None`.
pub fn parse_result(line: &str) -> Option<HelperResult> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v["type"] != "result" {
        return None;
    }
    let o = outcome(&v);
    let text = if o.budget_stop {
        BUDGET_STOP.to_owned()
    } else {
        let full = redact(v["result"].as_str().unwrap_or("").trim());
        tail_bytes(&full, MAX_ANSWER)
    };
    Some(HelperResult {
        ok: o.ok,
        text,
        cost_usd: o.cost_usd,
        input_tokens: o.input_tokens,
        output_tokens: o.output_tokens,
        page_html: None,
    })
}

/// The page a design or review job left in its out folder. Only a regular
/// file counts, never a link the agent planted to something on the host, and
/// only up to [`MAX_PAGE_BYTES`].
pub fn read_page(out: &Path) -> Option<String> {
    use std::io::Read;
    let p = out.join("page.html");
    let meta = std::fs::symlink_metadata(&p).ok()?;
    let max = u64::try_from(MAX_PAGE_BYTES).ok()?;
    if !meta.file_type().is_file() || meta.len() > max {
        return None;
    }
    let mut buf = Vec::new();
    // The container is gone by now, but read no more than the limit anyway.
    std::fs::File::open(&p)
        .ok()?
        .take(max + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() > MAX_PAGE_BYTES {
        return None;
    }
    // Invalid bytes and redaction can make the text longer than the file.
    let page = redact(&String::from_utf8_lossy(&buf));
    (page.len() <= MAX_PAGE_BYTES).then_some(page)
}

/// Picks Claude Code's own `result` out of a container's output. Claude
/// prints one, as its last line, but code the agent runs can write to the
/// same stream. So the first result counts, and a second one, or any output
/// after the first, means a result may be forged.
#[derive(Debug)]
pub struct FirstResult<T> {
    pub result: Option<T>,
    /// Something other than blank lines came after the first result.
    pub after_result: bool,
    /// More than one result line was printed.
    pub repeated: bool,
}

impl<T> Default for FirstResult<T> {
    fn default() -> Self {
        Self {
            result: None,
            after_result: false,
            repeated: false,
        }
    }
}

impl<T> FirstResult<T> {
    /// Takes one line of output and, if it is a result line, what it says.
    pub fn push(&mut self, line: &str, parsed: Option<T>) {
        if self.result.is_some() && !line.trim().is_empty() {
            self.after_result = true;
        }
        if let Some(r) = parsed {
            if self.result.is_some() {
                self.repeated = true;
            } else {
                self.result = Some(r);
            }
        }
    }

    /// Whether the result's cost and usage can be believed: it was the only
    /// result and nothing came after it.
    pub fn trusted(&self) -> bool {
        !self.after_result && !self.repeated
    }
}

impl FirstResult<HelperResult> {
    /// The helper's answer. With two results either could be the forged one,
    /// so it counts as a failure with nothing charged from it; output after
    /// the only result just makes its cost untrusted.
    pub fn helper_answer(self) -> Option<HelperResult> {
        let mut r = self.result?;
        if self.repeated {
            return Some(HelperResult {
                ok: false,
                text: "The agent's output held more than one result, so none of it is trusted."
                    .to_owned(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                page_html: None,
            });
        }
        if self.after_result {
            r.cost_usd = None;
        }
        Some(r)
    }
}

/// What a work run's `result` line said: how it finished, and the usage-limit
/// report that came with it, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    /// Always a [`RunEvent::Finished`].
    pub finished: RunEvent,
    pub limit: Option<Option<i64>>,
}

/// Splits one line's events into those for the board and, for a `result`
/// line, the result.
pub fn split_result(events: Vec<RunEvent>) -> (Vec<RunEvent>, Option<RunResult>) {
    let mut board = Vec::new();
    let mut finished = None;
    let mut limit = None;
    for e in events {
        match e {
            RunEvent::Finished { .. } => finished = Some(e),
            RunEvent::LimitHit { reset_epoch } => limit = Some(reset_epoch),
            _ => board.push(e),
        }
    }
    (
        board,
        finished.map(|finished| RunResult { finished, limit }),
    )
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
    fn clip_respects_char_boundaries() {
        assert_eq!(clip("héllo", 2), "hé…");
        assert_eq!(clip("hi", 5), "hi");
    }

    #[test]
    fn pages_must_be_small_regular_files() {
        let out = std::env::temp_dir().join(format!("decurion-page-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let made = std::fs::create_dir_all(&out);
        assert!(made.is_ok());
        assert_eq!(read_page(&out), None, "no page");
        let _ = std::fs::write(out.join("page.html"), "<h1>Plan</h1> sk-ant-abc");
        assert_eq!(read_page(&out).as_deref(), Some("<h1>Plan</h1> [redacted]"));
        let _ = std::fs::write(out.join("page.html"), vec![b'x'; MAX_PAGE_BYTES + 1]);
        assert_eq!(read_page(&out), None, "too big");
        // Each invalid byte becomes a three-byte replacement character.
        let _ = std::fs::write(out.join("page.html"), vec![0xff; MAX_PAGE_BYTES / 2]);
        assert_eq!(read_page(&out), None, "too big once read as text");
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(out.join("page.html"));
            let _ = std::fs::write(out.join("secret"), "host file");
            let _ = std::os::unix::fs::symlink(out.join("secret"), out.join("page.html"));
            assert_eq!(read_page(&out), None, "symlink");
        }
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn follow_up_replaces_the_restart_note() {
        let p = prompt_for(
            "Build it",
            true,
            Some("Fix the two issues the reviewer found."),
        );
        assert!(p.starts_with("Fix the two issues"));
        assert!(p.contains("Commit your work to this branch") && p.contains("Do not push"));
        assert!(!p.contains("restarted") && !p.contains("Build it"));
        let r = prompt_for("Build it", true, None);
        assert!(r.contains("restarted"));
        assert_eq!(prompt_for("Build it", true, Some("  ")), r);
        let fresh = prompt_for("Build it", false, None);
        assert!(fresh.starts_with("Build it") && fresh.contains("Do not push"));
        let fresh = prompt_for("Build it", false, Some("Also fix x"));
        assert!(fresh.starts_with("Build it") && fresh.contains("Also fix x"));
    }

    fn coordinator_args(tools: &[&str]) -> anyhow::Result<Vec<String>> {
        let tools: Vec<String> = tools.iter().map(|t| t.to_string()).collect();
        helper_run_args(
            &Sandbox {
                name: &helper_container_name(3),
                src: Path::new("C:\\data\\goals\\demo-5"),
                read_only: true,
                home_volume: &goal_home_volume(5),
                out: None,
            },
            "sid",
            true,
            Credential::Max(Profile::A),
            None,
            &tools,
        )
    }

    #[test]
    fn coordinator_sees_the_code_read_only_and_loses_its_tools() {
        let a = coordinator_args(&["Bash", "Edit", "Write", "mcp__x__*"]).unwrap_or_default();
        let joined = a.join(" ");
        let mounts: Vec<&String> = a
            .windows(2)
            .filter(|w| w[0] == "--mount")
            .map(|w| &w[1])
            .collect();
        assert_eq!(mounts.len(), 2);
        assert!(mounts[0].contains("dst=/work") && mounts[0].ends_with(",readonly"));
        assert_eq!(
            mounts[1].as_str(),
            "type=volume,src=decurion-goal-home-5,dst=/home/agent"
        );
        assert!(a
            .windows(2)
            .any(|w| w[0] == "--name" && w[1] == "decurion-helper-3"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-e" && w[1] == "CLAUDE_CODE_OAUTH_TOKEN"));
        for flag in [
            "--cap-drop ALL",
            "no-new-privileges",
            "--pids-limit",
            "--memory",
        ] {
            assert!(joined.contains(flag), "{flag}");
        }
        assert!(joined.contains("--resume sid"));
        assert!(!joined.contains("sk-ant"));
        assert!(
            !joined.contains(".claude")
                && !joined.contains(".ssh")
                && !joined.contains("docker.sock")
        );
        assert!(a
            .windows(2)
            .any(|w| w[0] == "--disallowedTools" && w[1] == "Bash,Edit,Write,mcp__x__*"));
        assert!(joined.ends_with("--setting-sources user --strict-mcp-config"));
    }

    #[test]
    fn odd_tool_names_are_refused() {
        for bad in [
            "",
            "Bash,Edit",
            "Bash Edit",
            "--dangerously-skip-permissions",
            "a\nb",
        ] {
            assert!(coordinator_args(&["Read", bad]).is_err(), "{bad:?}");
        }
        assert!(coordinator_args(&["Bash(git:*)"]).is_ok());
        let none = coordinator_args(&[]).unwrap_or_default();
        assert!(!none.is_empty() && !none.iter().any(|x| x == "--disallowedTools"));
    }

    #[test]
    fn checker_gets_a_writable_copy_and_its_own_home() {
        let a = helper_run_args(
            &Sandbox {
                name: &helper_container_name(4),
                src: Path::new("/data/checks/demo-7-job4"),
                read_only: false,
                home_volume: &check_home_volume(4),
                out: Some(Path::new("/data/out/job4")),
            },
            "sid",
            false,
            Credential::PayPerUse,
            Some(2.0),
            &["WebFetch".to_string()],
        )
        .unwrap_or_default();
        let joined = a.join(" ");
        assert!(joined.contains("type=bind,src=/data/checks/demo-7-job4,dst=/work "));
        assert!(!joined.contains("readonly"));
        assert!(joined.contains("src=decurion-check-home-4,"));
        let image = a.iter().position(|x| x == IMAGE).unwrap_or(0);
        assert!(a[..image]
            .windows(2)
            .any(|w| w[0] == "--mount" && w[1] == "type=bind,src=/data/out/job4,dst=/out"));
        assert!(joined.contains("--session-id sid") && joined.contains("--max-budget-usd 2.00"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-e" && w[1] == "ANTHROPIC_API_KEY"));
        assert!(!a.iter().any(|x| x == "CLAUDE_CODE_OAUTH_TOKEN"));
    }

    #[test]
    fn only_branch_names_we_make_are_accepted() {
        assert!(valid_branch("decurion/thread-7"));
        assert!(valid_branch("feature/a_b.c"));
        for bad in ["", "-b", "/x", "a..b", "a b", "a;b", "a~1", "x\n"] {
            assert!(!valid_branch(bad), "{bad:?}");
        }
    }

    #[test]
    fn helper_results_keep_the_whole_answer() {
        let long = "p".repeat(20_000);
        let l = serde_json::json!({
            "type": "result", "subtype": "success", "is_error": false,
            "result": format!("{long} sk-ant-abc_1 end"),
            "total_cost_usd": 0.5,
            "usage": {"input_tokens": 10, "cache_creation_input_tokens": 5, "output_tokens": 7}
        })
        .to_string();
        let r = parse_result(&l);
        assert_eq!(
            r,
            Some(HelperResult {
                ok: true,
                text: format!("{long} [redacted] end"),
                cost_usd: Some(0.5),
                input_tokens: Some(15),
                output_tokens: Some(7),
                page_html: None,
            })
        );
        assert!(parse_result(r#"{"type":"assistant","message":{"content":[]}}"#).is_none());
        assert!(parse_result("not json").is_none());
    }

    #[test]
    fn helper_results_keep_their_end_on_a_char_boundary() {
        let huge = format!("start {} {{\"verdict\":\"ok\"}}", "é".repeat(MAX_ANSWER));
        let l =
            serde_json::json!({"type": "result", "subtype": "success", "result": huge}).to_string();
        let text = parse_result(&l).map(|r| r.text).unwrap_or_default();
        assert!(text.len() <= MAX_ANSWER && text.len() > MAX_ANSWER - 4);
        assert!(text.starts_with('…') && !text.contains("start"));
        assert!(text.ends_with(r#"{"verdict":"ok"}"#));
        assert_eq!(tail_bytes("short", 10), "short");
        assert_eq!(tail_bytes("abcdefghij", 6), "…hij");
        assert_eq!(tail_bytes("abcdéfgh", 7), "…fgh");
    }

    fn helper_lines(lines: &[&str]) -> Option<HelperResult> {
        let mut w = FirstResult::default();
        for l in lines {
            w.push(l, parse_result(l));
        }
        w.helper_answer()
    }

    const REAL: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"real","total_cost_usd":4.0,"usage":{"input_tokens":10,"output_tokens":5}}"#;
    const FAKE: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"fake","total_cost_usd":0.01,"usage":{"input_tokens":1,"output_tokens":1}}"#;

    #[test]
    fn a_single_helper_result_is_taken_as_is() {
        let r = helper_lines(&[r#"{"type":"system"}"#, REAL, ""]);
        assert!(matches!(
            r,
            Some(HelperResult { ok: true, cost_usd: Some(c), input_tokens: Some(10), text, .. })
                if c == 4.0 && text == "real"
        ));
        assert_eq!(helper_lines(&["not json"]), None);
    }

    #[test]
    fn a_second_helper_result_makes_the_job_fail_unpaid() {
        for lines in [[REAL, FAKE], [FAKE, REAL]] {
            let r = helper_lines(&lines);
            assert!(
                matches!(
                    &r,
                    Some(HelperResult {
                        ok: false,
                        cost_usd: None,
                        input_tokens: None,
                        output_tokens: None,
                        ..
                    })
                ),
                "{r:?}"
            );
            let text = r.map(|r| r.text).unwrap_or_default();
            assert!(!text.contains("real") && !text.contains("fake"));
        }
    }

    #[test]
    fn output_after_a_helper_result_drops_only_its_cost() {
        let r = helper_lines(&[REAL, "stray text"]);
        assert!(matches!(
            r,
            Some(HelperResult { ok: true, cost_usd: None, input_tokens: Some(10), text, .. })
                if text == "real"
        ));
    }

    #[test]
    fn work_runs_keep_the_first_result_and_distrust_a_second() {
        let limit = r#"{"type":"result","subtype":"success","is_error":true,"result":"Claude AI usage limit reached|1760900400"}"#;
        let msg = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        let mut w = FirstResult::default();
        let mut board = Vec::new();
        for l in [msg, REAL, limit] {
            let (b, r) = split_result(parse_line(l));
            board.extend(b);
            w.push(l, r);
        }
        assert_eq!(board, vec![RunEvent::Message { text: "hi".into() }]);
        assert!(!w.trusted() && w.repeated);
        assert!(matches!(
            w.result,
            Some(RunResult { finished: RunEvent::Finished { summary, .. }, limit: None })
                if summary == "real"
        ));

        let mut w = FirstResult::default();
        let (_, r) = split_result(parse_line(limit));
        w.push(limit, r);
        w.push("", None);
        assert!(w.trusted());
        assert_eq!(w.result.and_then(|r| r.limit), Some(Some(1_760_900_400)));
    }

    #[test]
    fn helper_crash_and_budget_stop_read_like_work_runs() {
        let crash = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"total_cost_usd":0}"#;
        assert!(matches!(
            parse_result(crash),
            Some(HelperResult {
                ok: false,
                cost_usd: None,
                ..
            })
        ));
        let stop = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"total_cost_usd":3.2,"result":"x"}"#;
        assert!(matches!(
            parse_result(stop),
            Some(HelperResult { ok: false, cost_usd: Some(_), text, .. }) if text == BUDGET_STOP
        ));
    }
}
