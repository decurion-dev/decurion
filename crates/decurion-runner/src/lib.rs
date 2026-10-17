//! Pieces of the runner that do not need Docker or a hub, kept here so they
//! can be tested directly.

use decurion_proto::{Profile, RunEvent};
use serde_json::Value;
use std::path::Path;

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

/// The one credential an agent gets: its Max profile's Claude token, or the
/// pay-per-use API key. Never both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    Max(Profile),
    PayPerUse,
}

impl Credential {
    /// The environment variable Claude Code reads it from.
    pub fn var(self) -> &'static str {
        match self {
            Credential::Max(_) => "CLAUDE_CODE_OAUTH_TOKEN",
            Credential::PayPerUse => "ANTHROPIC_API_KEY",
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

/// Arguments for `docker run`. The token is passed by name only, so it never
/// appears on a command line, and the task goes in on stdin (`-i`), so a long
/// task cannot hit the Windows command-line limit.
pub fn run_args(
    thread_id: i64,
    worktree: &Path,
    session_id: &str,
    resume: bool,
    cred: Credential,
    budget_usd: Option<f64>,
) -> anyhow::Result<Vec<String>> {
    let src = worktree.display().to_string();
    // docker reads --mount as comma-separated fields.
    if src.contains([',', '"']) {
        anyhow::bail!("workspace path {src} contains a comma or quote");
    }
    let mut a: Vec<String> = [
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
        "-e",
        cred.var(),
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
    .iter()
    .map(|s| s.to_string())
    .collect();
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
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("sk-ant-") {
        out.push_str(&rest[..i]);
        out.push_str("[redacted]");
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
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
        _ => vec![],
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
    fn pay_per_use_runs_get_the_api_key_and_a_budget_only() {
        let a = run_args(
            7,
            Path::new("/w"),
            "sid",
            false,
            Credential::PayPerUse,
            Some(9.5),
        )
        .unwrap_or_default();
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-e" && w[1] == "ANTHROPIC_API_KEY"));
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
        assert!(!m
            .iter()
            .any(|x| x == "ANTHROPIC_API_KEY" || x == "--max-budget-usd"));
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
}
