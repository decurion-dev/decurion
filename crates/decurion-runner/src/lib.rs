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

/// The only credential an agent gets: its profile's Claude token.
pub const TOKEN_VAR: &str = "CLAUDE_CODE_OAUTH_TOKEN";

pub fn container_name(thread_id: i64) -> String {
    format!("decurion-thread-{thread_id}")
}

pub fn home_volume(thread_id: i64) -> String {
    format!("decurion-home-{thread_id}")
}

pub fn credential_target(p: Profile) -> String {
    format!("decurion/claude-profile-{}", p.as_str())
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
        TOKEN_VAR,
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
    Ok(a)
}

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
        Some("result") => vec![RunEvent::Finished {
            ok: v["subtype"] == "success" && v["is_error"] != true,
            summary: clip(v["result"].as_str().unwrap_or(""), 2000),
        }],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_passed_by_name_and_no_host_paths_are_mounted() {
        let a =
            run_args(7, Path::new("C:\\data\\worktrees\\demo 7"), "sid", false).unwrap_or_default();
        assert!(a.contains(&"-i".to_string()));
        let joined = a.join(" ");
        assert!(a.windows(2).any(|w| w[0] == "-e" && w[1] == TOKEN_VAR));
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
        let a = run_args(7, Path::new("/w"), "sid", true).unwrap_or_default();
        assert_eq!(
            a[a.len() - 2..],
            ["--resume".to_string(), "sid".to_string()]
        );
    }

    #[test]
    fn mount_paths_with_commas_are_refused() {
        assert!(run_args(7, Path::new("C:\\a,b\\demo-7"), "sid", false).is_err());
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
        let ok = r#"{"type":"result","subtype":"success","is_error":false,"result":"DONE"}"#;
        assert_eq!(
            parse_line(ok),
            vec![RunEvent::Finished {
                ok: true,
                summary: "DONE".into()
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
