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
            let summary = if v["subtype"] == "error_max_budget_usd" {
                BUDGET_STOP.to_owned()
            } else {
                clip(v["result"].as_str().unwrap_or(""), 2000)
            };
            // A crashed run can report $0 after doing paid work; only a clean
            // finish or a budget stop has a cost worth believing.
            let cost_known = v["subtype"] == "success" || v["subtype"] == "error_max_budget_usd";
            vec![RunEvent::Finished {
                ok: v["subtype"] == "success" && v["is_error"] != true,
                summary,
                cost_usd: v["total_cost_usd"].as_f64().filter(|_| cost_known),
                input_tokens: input,
                output_tokens: u["output_tokens"].as_u64(),
            }]
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
}
