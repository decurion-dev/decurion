//! git that talks to a git server. The hub does this in two places: sending
//! accepted work ([`crate::github`], the push) and asking the app's git
//! server where new work starts ([`crate::upstream`]). Both build git with
//! [`command`] and run it with [`output`], so the rules below are the same
//! for both and cannot drift apart:
//!
//! - **Never waits on a question.** Terminal, credential-helper, askpass and
//!   SSH prompts are off and git's input is closed. A server that does not
//!   answer is given up on at the caller's time limit, and git is killed
//!   (`kill_on_drop`), never left running.
//! - **Nothing of the repo's own runs.** Hooks and file watchers are off.
//! - **The owner's own sign-in only.** Credentials come from git's own
//!   helpers (Git Credential Manager on Windows) or the owner's SSH setup.
//!   A caller never puts a password or token in an argument, and anything
//!   git says is passed through [`crate::github::scrub`] before it is logged.

use crate::checkpoints::git;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;
use tokio::process::Command;

/// Why a network git command gave no answer.
#[derive(Debug)]
pub(crate) enum Failed {
    /// The server did not answer within the time limit; git was killed.
    TooSlow,
    /// git itself could not be started on this PC.
    NotStarted(std::io::Error),
}

/// git, run in `dir`, set up to reach a git server without ever asking
/// anything. The caller adds the subcommand and its arguments.
pub(crate) async fn command(dir: &Path) -> Command {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args([
            "-c",
            &format!("core.hooksPath={null}"),
            "-c",
            "core.fsmonitor=false",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        // No helper may ask either: Git Credential Manager, askpass, SSH.
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS_REQUIRE", "never")
        // git's messages in English, to tell failures apart.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    // The owner's own SSH command is theirs to keep; only plain ssh is told
    // never to ask.
    let own_ssh = std::env::var_os("GIT_SSH_COMMAND").is_some()
        || std::env::var_os("GIT_SSH").is_some()
        || git(dir, &["config", "--get", "core.sshCommand"])
            .await
            .is_ok();
    if !own_ssh {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    cmd
}

/// Runs `cmd` and waits at most `limit` for it. Past the limit the future
/// holding git is dropped, which kills it.
pub(crate) async fn output(mut cmd: Command, limit: Duration) -> Result<Output, Failed> {
    match tokio::time::timeout(limit, cmd.output()).await {
        Err(_) => Err(Failed::TooSlow),
        Ok(Err(e)) => Err(Failed::NotStarted(e)),
        Ok(Ok(out)) => Ok(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::time::Instant;

    fn env_of<'a>(cmd: &'a Command, name: &str) -> Option<&'a OsStr> {
        cmd.as_std()
            .get_envs()
            .find(|(k, _)| *k == OsStr::new(name))
            .and_then(|(_, v)| v)
    }

    #[tokio::test]
    async fn network_git_never_asks_and_runs_nothing_of_the_repos_own() {
        let dir = std::env::temp_dir();
        let cmd = command(&dir).await;
        assert_eq!(env_of(&cmd, "GIT_TERMINAL_PROMPT"), Some(OsStr::new("0")));
        assert_eq!(env_of(&cmd, "GCM_INTERACTIVE"), Some(OsStr::new("never")));
        assert_eq!(env_of(&cmd, "GIT_ASKPASS"), Some(OsStr::new("")));
        assert_eq!(
            env_of(&cmd, "SSH_ASKPASS_REQUIRE"),
            Some(OsStr::new("never"))
        );
        if std::env::var_os("GIT_SSH_COMMAND").is_none() && std::env::var_os("GIT_SSH").is_none()
        {
            assert!(env_of(&cmd, "GIT_SSH_COMMAND")
                .is_some_and(|v| v.to_string_lossy().contains("BatchMode=yes")));
        }
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.iter().any(|a| a.starts_with("core.hooksPath=")));
        assert!(args.contains(&"core.fsmonitor=false".to_owned()));
    }

    #[tokio::test]
    async fn a_server_that_never_answers_is_given_up_on_at_the_time_limit() {
        let dir = std::env::temp_dir().join(format!("decurion-netgit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        git(&dir, &["init", "--quiet"]).await.expect("init");
        // A "server" that hangs: git runs this in place of upload-pack. Short,
        // because on Windows the test's runtime waits for it to end before
        // the test does.
        let mut cmd = command(&dir).await;
        cmd.args(["ls-remote", "--upload-pack=sleep 8; :", "."]);
        let started = Instant::now();
        let r = output(cmd, Duration::from_millis(500)).await;
        assert!(matches!(r, Err(Failed::TooSlow)), "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(6));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
