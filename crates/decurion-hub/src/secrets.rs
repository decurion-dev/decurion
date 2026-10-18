//! The one credential a run needs, for a cloud worker that has no copy
//! of its own. On the PC it is read from Windows Credential Manager under the
//! same names the PC runner uses, falling back to environment variables
//! (tests, a hub on Linux). A Max plan's sign-in saved from the board is
//! read from the vault first. It is handed out only for work running on the
//! asking runner, and never stored by the hub.

use decurion_proto::RunCredential;

/// `(variable the agent reads, Credential Manager name, fallback variable)`
/// for the account a run is billed to: `A`, `B`, `api`, `openai` or `xai`.
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
        "openai" => Some((
            "OPENAI_API_KEY",
            "decurion/openai-api".to_owned(),
            "DECURION_OPENAI_API_KEY".to_owned(),
        )),
        "xai" => Some((
            "XAI_API_KEY",
            "decurion/xai-api".to_owned(),
            "DECURION_XAI_API_KEY".to_owned(),
        )),
        _ => None,
    }
}

pub fn read(vault: Option<&decurion_vault::Vault>, account: &str) -> Option<RunCredential> {
    let (var, target, fallback) = names(account)?;
    let board = account
        .parse::<decurion_proto::Profile>()
        .ok()
        .zip(vault)
        .and_then(|(p, v)| v.get_claude(p).ok().flatten());
    if let Some(value) = board.filter(|v| !v.is_empty()) {
        return Some(RunCredential {
            var: var.to_owned(),
            value,
        });
    }
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
