//! The one Claude credential a run needs, for a cloud worker that has no copy
//! of its own. On the PC it is read from Windows Credential Manager under the
//! same names the PC runner uses, falling back to environment variables
//! (tests, a hub on Linux). It is handed out only for work running on the
//! asking runner, and never stored by the hub.

use decurion_proto::RunCredential;

/// `(variable Claude Code reads, Credential Manager name, fallback variable)`
/// for the account a run is billed to: `A`, `B` or `api`.
pub fn names(account: &str) -> Option<(&'static str, String, String)> {
    match account {
        "A" | "B" => Some((
            "CLAUDE_CODE_OAUTH_TOKEN",
            format!("decurion/claude-profile-{account}"),
            format!("DECURION_CLAUDE_TOKEN_{account}"),
        )),
        "api" => Some((
            "ANTHROPIC_API_KEY",
            "decurion/anthropic-api".to_owned(),
            "DECURION_ANTHROPIC_API_KEY".to_owned(),
        )),
        _ => None,
    }
}

pub fn read(account: &str) -> Option<RunCredential> {
    let (var, target, fallback) = names(account)?;
    #[cfg(windows)]
    let saved = keyring::Entry::new_with_target(&target, "decurion", "claude")
        .and_then(|e| e.get_password())
        .ok();
    #[cfg(not(windows))]
    let saved: Option<String> = {
        let _ = target;
        None
    };
    // The variable is the fallback everywhere, so the same setup (and the
    // tests) work on any computer.
    saved
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var(&fallback).ok())
        .filter(|v| !v.trim().is_empty())
        .map(|value| RunCredential {
            var: var.to_owned(),
            value,
        })
}
