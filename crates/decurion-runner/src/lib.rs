//! Pieces of the runner that do not need Docker or a hub, kept here so they
//! can be tested directly.

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

const RULES: &str = "You are working in your own copy of the code on its own branch. \
Commit your work to this branch as you go. Do not push and do not open pull requests; \
Decurion does that after review.";

const RESUME: &str = "Your container was restarted. Continue the same task from where it stopped. \
Check `git status` and `git log` first and do not redo finished steps.";

/// The task and the rules for a new run. `test_keys` are the names of the
/// app's test keys, which the agent is told about so it uses them by name.
pub fn prompt_for(task: &str, resume: bool, test_keys: &[String]) -> String {
    if resume && test_keys.is_empty() {
        RESUME.to_owned()
    } else if resume {
        // A run stopped for a key in its commits comes back this way too.
        format!(
            "{RESUME} Your branch must not contain the value of any of these test keys: {}. \
If an earlier commit on it does, rewrite the branch so no commit contains it.",
            test_keys.join(", ")
        )
    } else if test_keys.is_empty() {
        format!("{task}\n\n{RULES}")
    } else {
        format!(
            "{task}\n\n{RULES}\n\nThese environment variables hold test-mode keys for running \
the app's tests: {}. Use them only by name. Never print them, and never write their values \
into files or commits; a commit that contains one will not be accepted.",
            test_keys.join(", ")
        )
    }
}

/// Arguments for `docker run`. Credentials and the app's test keys are passed
/// by name only, so their values never appear on a command line, and the task
/// goes in on stdin (`-i`), so a long task cannot hit the Windows
/// command-line limit.
pub fn run_args(
    thread_id: i64,
    worktree: &Path,
    session_id: &str,
    resume: bool,
    cred: Credential,
    budget_usd: Option<f64>,
    test_keys: &[String],
) -> anyhow::Result<Vec<String>> {
    let src = worktree.display().to_string();
    // docker reads --mount as comma-separated fields.
    if src.contains([',', '"']) {
        anyhow::bail!("workspace path {src} contains a comma or quote");
    }
    // A plain name is what keeps `-e` from taking a value (`-e NAME=value`)
    // or the place of another flag.
    for k in test_keys {
        decurion_vault::check_name(k).map_err(|e| anyhow::anyhow!("test key {k:?}: {e}"))?;
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
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for v in cred.vars() {
        a.extend(["-e".to_owned(), (*v).to_owned()]);
    }
    for k in test_keys {
        a.extend(["-e".to_owned(), k.clone()]);
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

/// What the board shows in place of a test key's value.
pub const HIDDEN: &str = "[test key hidden]";

/// Hides every test key value in text going to the board. Every place any
/// value appears is covered, overlaps and values inside other values too, so
/// no part of one is left showing. Run it before anything shortens the text:
/// a value cut in half no longer matches.
pub fn hide(s: &str, secrets: &[String]) -> String {
    let mut covered = vec![false; s.len()];
    for x in secrets.iter().filter(|x| !x.is_empty()) {
        let mut from = 0;
        while let Some(i) = s[from..].find(x.as_str()) {
            let at = from + i;
            covered[at..at + x.len()].fill(true);
            // Step one character, not the whole value, to find overlaps.
            from = at + s[at..].chars().next().map_or(1, char::len_utf8);
        }
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        // Matches start and end on character boundaries, so every run does.
        let run = covered[i..]
            .iter()
            .position(|c| *c != covered[i])
            .map_or(s.len(), |n| i + n);
        out.push_str(if covered[i] { HIDDEN } else { &s[i..run] });
        i = run;
    }
    out
}

/// [`hide`] for text that was already cut off at its end, such as an
/// over-long line: a value cut there is not matched, so the last stretch
/// that could hold part of one is dropped too.
pub fn hide_cut(s: &str, secrets: &[String]) -> String {
    let h = hide(s, secrets);
    let Some(longest) = secrets.iter().map(String::len).max().filter(|n| *n > 0) else {
        return h;
    };
    // A few bytes more than the value, for a character the cut broke.
    let mut end = h.len().saturating_sub(longest + 4);
    while !h.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &h[..end])
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
    parse_line_hiding(line, &[])
}

/// [`parse_line`] with the run's test key values hidden from every text.
pub fn parse_line_hiding(line: &str, secrets: &[String]) -> Vec<RunEvent> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![];
    };
    // Hidden before clipped, so a value at the cut is still matched whole.
    let show = |s: &str, n: usize| clip(&hide(s, secrets), n);
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
                                    text: show(t, 2000),
                                })
                        }
                        Some("tool_use") => {
                            let input = &p["input"];
                            let summary = ["command", "file_path", "pattern", "url", "description"]
                                .iter()
                                .find_map(|k| input[*k].as_str())
                                .unwrap_or("");
                            Some(RunEvent::Tool {
                                name: hide(p["name"].as_str().unwrap_or("tool"), secrets),
                                summary: show(summary, 200),
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
                show(text, 2000)
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

/// Looks for test key values in a stream read in chunks, such as the
/// the raw objects of an agent's commits, without holding all of it. Each
/// value is looked for as UTF-8 and as UTF-16, the form Windows tools often
/// save text in. The end of each chunk is kept, one byte short of the
/// longest pattern, so a value split between two chunks is still found.
pub struct LeakScanner {
    keys: Vec<(String, Vec<u8>)>,
    seen: Vec<bool>,
    carry: Vec<u8>,
    overlap: usize,
}

impl LeakScanner {
    /// `keys` are (name, value) pairs.
    pub fn new(keys: &[(String, String)]) -> Self {
        let keys: Vec<(String, Vec<u8>)> = keys
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .flat_map(|(k, v)| {
                let utf16: Vec<u8> = v.encode_utf16().flat_map(u16::to_le_bytes).collect();
                [(k.clone(), v.as_bytes().to_vec()), (k.clone(), utf16)]
            })
            .collect();
        let overlap = keys.iter().map(|(_, v)| v.len()).max().unwrap_or(1) - 1;
        Self {
            seen: vec![false; keys.len()],
            keys,
            carry: Vec::new(),
            overlap,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(chunk);
        for ((_, v), seen) in self.keys.iter().zip(self.seen.iter_mut()) {
            if !*seen && buf.windows(v.len()).any(|w| w == v.as_slice()) {
                *seen = true;
            }
        }
        let keep = self.overlap.min(buf.len());
        buf.drain(..buf.len() - keep);
        self.carry = buf;
    }

    /// Names of the keys seen so far, in the order they were given.
    pub fn found(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for ((k, _), seen) in self.keys.iter().zip(&self.seen) {
            if *seen && !out.contains(k) {
                out.push(k.clone());
            }
        }
        out
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            None,
            &[],
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

    fn keys() -> Vec<String> {
        vec!["STRIPE_SECRET_KEY".into(), "MAPS_TEST_KEY".into()]
    }

    #[test]
    fn test_keys_are_passed_by_name_before_the_image() {
        let a = run_args(
            7,
            Path::new("/w"),
            "sid",
            false,
            Credential::Max(Profile::A),
            None,
            &keys(),
        )
        .unwrap_or_default();
        let image = a.iter().position(|x| x == IMAGE).unwrap_or_default();
        for k in keys() {
            let at = a
                .windows(2)
                .position(|w| w[0] == "-e" && w[1] == k)
                .expect("key passed with -e");
            assert!(at < image);
        }
        assert!(!a.iter().any(|x| x.contains('=') && x.starts_with("STRIPE")));
        assert!(!a.join(" ").contains("sk_test"));
        // A name that could carry a value or pose as a flag is refused.
        for bad in ["STRIPE=sk_test_x", "--privileged", "PATH"] {
            assert!(run_args(
                7,
                Path::new("/w"),
                "sid",
                false,
                Credential::Max(Profile::A),
                None,
                &[bad.to_string()],
            )
            .is_err());
        }
    }

    #[test]
    fn the_agent_is_told_about_test_keys_only_when_there_are_some() {
        let p = prompt_for("Fix it", false, &keys());
        assert!(p.starts_with("Fix it\n\n"));
        assert!(p.contains("STRIPE_SECRET_KEY, MAPS_TEST_KEY"));
        assert!(p.contains("will not be accepted"));
        assert_eq!(
            prompt_for("Fix it", false, &[]),
            format!("Fix it\n\n{RULES}")
        );
        // A resumed run is reminded, so a run stopped for a key in its
        // commits can clean its branch when the owner retries it.
        let r = prompt_for("Fix it", true, &keys());
        assert!(r.starts_with(RESUME) && r.contains("rewrite the branch"));
        assert!(r.contains("STRIPE_SECRET_KEY, MAPS_TEST_KEY"));
        assert_eq!(prompt_for("Fix it", true, &[]), RESUME);
    }

    #[test]
    fn test_key_values_are_hidden_whole() {
        let s = vec!["sk_test_abcdef".to_string(), "abcdef12".to_string()];
        assert_eq!(
            hide("key sk_test_abcdef ok", &s),
            "key [test key hidden] ok"
        );
        // One value inside another, and two that overlap, leave nothing showing.
        let inner = vec!["abcdefgh".to_string(), "xxabcdefghyy".to_string()];
        assert_eq!(hide("1xxabcdefghyy2", &inner), "1[test key hidden]2");
        let lap = vec!["abcdefgh".to_string(), "efghijkl".to_string()];
        assert_eq!(hide("<abcdefghijkl>", &lap), "<[test key hidden]>");
        assert_eq!(hide("aaaaaaaaa", &["aaaaaaaa".to_string()]), HIDDEN);
        assert_eq!(
            hide("héllo wörld", &["wörld".to_string()]),
            "héllo [test key hidden]"
        );
        assert_eq!(hide("plain", &[String::new()]), "plain");
    }

    #[test]
    fn a_value_at_the_clip_limit_is_hidden_not_cut() {
        let v = "sk_test_".to_string() + &"z".repeat(300);
        let text = format!("{}{v}", "a".repeat(1900));
        let l = serde_json::json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "text", "text": text},
                {"type": "tool_use", "name": "Bash", "input": {"command": format!("echo {v}")}},
            ]},
        })
        .to_string();
        let ev = parse_line_hiding(&l, std::slice::from_ref(&v));
        let [RunEvent::Message { text }, RunEvent::Tool { summary, .. }] = &ev[..] else {
            panic!("unexpected events {ev:?}");
        };
        assert!(!text.contains("zzz") && text.ends_with(HIDDEN));
        assert_eq!(summary, "echo [test key hidden]");
        let r =
            serde_json::json!({"type": "result", "subtype": "success", "result": v}).to_string();
        assert!(matches!(
            &parse_line_hiding(&r, std::slice::from_ref(&v))[..],
            [RunEvent::Finished { summary, .. }] if summary == HIDDEN
        ));
    }

    #[test]
    fn a_value_cut_off_at_the_end_is_dropped() {
        let s = vec!["sk_test_abcdéfgh".to_string()];
        let cut = "error: sk_test_abcdéfgh was refused by the test server, then sk_test_abcd";
        let h = hide_cut(cut, &s);
        assert!(h.starts_with("error: [test key hidden]") && !h.contains("abcd"));
        assert_eq!(hide_cut("short", &[]), "short");
        // A broken last character from a cut line.
        let broken = "x".repeat(50) + "sk_test_abcd\u{FFFD}";
        assert!(!hide_cut(&broken, &s).contains("abcd"));
    }

    #[test]
    fn leaks_are_found_across_chunk_boundaries() {
        let k = vec![
            (
                "STRIPE_SECRET_KEY".to_string(),
                "sk_test_1234567890".to_string(),
            ),
            ("MAPS_TEST_KEY".to_string(), "maps-test-abc".to_string()),
            ("UNUSED_KEY".to_string(), "never-there-1".to_string()),
        ];
        let stream = b"diff\n+STRIPE=sk_test_1234567890\nauthor maps-test-abc <a@b>\n";
        for size in 1..stream.len() {
            let mut s = LeakScanner::new(&k);
            for chunk in stream.chunks(size) {
                s.feed(chunk);
            }
            assert_eq!(
                s.found(),
                ["STRIPE_SECRET_KEY", "MAPS_TEST_KEY"],
                "size {size}"
            );
        }
        let mut s = LeakScanner::new(&k);
        s.feed(b"sk_test_12345");
        s.feed(b"6789 nothing");
        assert!(s.found().is_empty());
        // A file saved as UTF-16, as Windows tools often do.
        let utf16: Vec<u8> = "K=maps-test-abc"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut s = LeakScanner::new(&k);
        s.feed(&utf16);
        assert_eq!(s.found(), ["MAPS_TEST_KEY"]);
        let mut none = LeakScanner::new(&[]);
        none.feed(b"anything");
        assert!(none.found().is_empty());
    }

    #[test]
    fn clip_respects_char_boundaries() {
        assert_eq!(clip("héllo", 2), "hé…");
        assert_eq!(clip("hi", 5), "hi");
    }
}
