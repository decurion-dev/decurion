//! Handing work between computers: rebuilding a thread's copy from the hub,
//! saving it back as a checkpoint, and the sealed throwaway containers that
//! touch the agent's files on the runner's behalf.
//!
//! Everything here is decided without Docker, git or a hub, so it can be
//! tested directly; `main.rs` does the work.

use crate::{home_mount, work_mount, GIT_ENV, IMAGE};
use decurion_proto::Place;
use std::path::Path;

/// Largest code bundle the runner downloads or uploads (the hub's limit).
pub const CODE_CAP: u64 = 2 << 30;
/// Largest session archive the runner downloads or uploads (the hub's limit).
pub const SESSION_CAP: u64 = 512 << 20;

/// The commit message for edits the agent had not committed when its work
/// was moved.
pub const SAVE_MESSAGE: &str = "Decurion: saved unfinished work before moving";

/// The name of a throwaway container working on a thread's files. It shares
/// the `decurion-thread-` prefix, so a restarted runner clears it too.
pub fn tool_container_name(thread_id: i64) -> String {
    format!("decurion-thread-{thread_id}-tool")
}

/// `docker run` for a sealed throwaway container: no network, no extra
/// powers, one mount, and a fixed program instead of Claude Code. The agent's
/// files are only ever handled in here, never by the runner itself, so
/// nothing the agent planted (git hooks, config, odd links) runs on the host.
fn sealed_args(
    thread_id: i64,
    mount: &str,
    stdin: bool,
    env: &[&str],
    entrypoint: &str,
    cmd: &[&str],
) -> Vec<String> {
    let mut a: Vec<String> = vec!["run".into(), "--rm".into()];
    if stdin {
        a.push("-i".into());
    }
    for s in [
        "--name",
        &tool_container_name(thread_id),
        "--network",
        "none",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--memory",
        "2g",
        "--pids-limit",
        "256",
        "--mount",
        mount,
    ] {
        a.push(s.into());
    }
    for e in env {
        a.push("-e".into());
        a.push((*e).into());
    }
    a.push("--entrypoint".into());
    a.push(entrypoint.into());
    a.push(IMAGE.into());
    a.extend(cmd.iter().map(|s| (*s).to_owned()));
    a
}

/// The two `docker run`s that save unfinished edits as a commit: stage
/// everything, then commit with hooks off. A commit that fails because
/// nothing changed is fine.
pub fn save_edits_args(thread_id: i64, worktree: &Path) -> anyhow::Result<[Vec<String>; 2]> {
    let work = work_mount(worktree)?;
    Ok([
        sealed_args(
            thread_id,
            &work,
            false,
            GIT_ENV,
            "git",
            &["-C", "/work", "add", "-A"],
        ),
        sealed_args(
            thread_id,
            &work,
            false,
            GIT_ENV,
            "git",
            &[
                "-C",
                "/work",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--no-verify",
                "-q",
                "-m",
                SAVE_MESSAGE,
            ],
        ),
    ])
}

/// `docker run` that writes the agent's Claude Code sessions as a tar
/// archive to stdout.
pub fn session_export_args(thread_id: i64) -> Vec<String> {
    sealed_args(
        thread_id,
        &home_mount(thread_id),
        false,
        &[],
        "tar",
        &["-C", "/home/agent", "-cf", "-", ".claude/projects"],
    )
}

/// `docker run` that unpacks a session archive from stdin into the agent's
/// (fresh) home volume.
pub fn session_import_args(thread_id: i64) -> Vec<String> {
    sealed_args(
        thread_id,
        &home_mount(thread_id),
        true,
        &[],
        "tar",
        &["-C", "/home/agent", "-xf", "-"],
    )
}

/// Whether the runner's own copy can be used as it is. Only when it is the
/// copy the hub's latest checkpoint was made from: `marker` is the text of
/// `<workspace>.checkpoint` (`None` when there is no such file, which matches
/// work with no checkpoint yet). Anything else is rebuilt from the hub.
pub fn reuse_workspace(has_repo: bool, marker: Option<&str>, checkpoint: Option<i64>) -> bool {
    let mine = match marker {
        None => None,
        Some(t) => match t.trim().parse::<i64>() {
            Ok(n) => Some(n),
            // An unreadable marker matches nothing.
            Err(_) => return false,
        },
    };
    has_repo && mine == checkpoint
}

/// What goes into the code bundle. Normally only the commits since the
/// starting commit (the receiver always has that one); a full branch when
/// that range is empty, because git refuses an empty bundle.
pub fn bundle_spec(base: &str, branch: &str, new_commits: u64) -> String {
    if new_commits == 0 {
        format!("refs/heads/{branch}")
    } else {
        format!("{base}..refs/heads/{branch}")
    }
}

/// A full git commit id (SHA-1, or SHA-256 repos).
pub fn is_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The null device, for settings that must point at nothing.
pub const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// Environment for every git the runner runs on the host. Only the runner's
/// own settings count: no system-wide or user config, no Git LFS downloads,
/// no password prompts and no lazy fetching, so nothing an agent committed
/// (`.gitattributes`, `.lfsconfig`) or set up on the computer can make git
/// run other programs here.
pub fn host_git_env() -> [(&'static str, &'static str); 5] {
    [
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", NULL_DEVICE),
        ("GIT_LFS_SKIP_SMUDGE", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
    ]
}

/// Settings put before every host git command. Command-line settings win
/// over any repo's config: no hooks, no fsmonitor, and a fixed name in case
/// anything commits (there is no user config to take one from).
pub fn host_git_args() -> Vec<String> {
    [
        format!("core.hooksPath={NULL_DEVICE}"),
        "core.fsmonitor=false".to_owned(),
        "user.name=decurion".to_owned(),
        "user.email=decurion@decurion.invalid".to_owned(),
    ]
    .into_iter()
    .flat_map(|s| ["-c".to_owned(), s])
    .collect()
}

/// The board note when work leaves this computer.
pub fn move_note(to: Place) -> &'static str {
    match to {
        Place::Cloud => "moved to the cloud",
        Place::Pc => "moved to the PC",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The options that seal a throwaway container, all before the image.
    fn assert_sealed(a: &[String]) {
        let image = a.iter().position(|x| x == IMAGE).expect("image");
        let opts = &a[..image];
        assert!(opts
            .windows(2)
            .any(|w| w[0] == "--network" && w[1] == "none"));
        assert!(opts
            .windows(2)
            .any(|w| w[0] == "--cap-drop" && w[1] == "ALL"));
        assert!(opts
            .windows(2)
            .any(|w| w[0] == "--security-opt" && w[1] == "no-new-privileges"));
        assert!(opts.contains(&"--rm".to_string()));
        assert_eq!(opts.iter().filter(|x| *x == "--mount").count(), 1);
        assert!(!a.join(" ").contains("docker.sock"));
    }

    #[test]
    fn saving_edits_commits_inside_a_sealed_box_with_hooks_off() {
        let [add, commit] = save_edits_args(7, Path::new("/d/w/demo-7")).expect("args");
        for a in [&add, &commit] {
            assert_sealed(a);
            assert!(a
                .windows(2)
                .any(|w| w[0] == "--entrypoint" && w[1] == "git"));
            assert!(a
                .windows(2)
                .any(|w| w[0] == "--mount" && w[1] == "type=bind,src=/d/w/demo-7,dst=/work"));
            assert!(a
                .windows(2)
                .any(|w| w[0] == "-e" && w[1] == "GIT_CONFIG_VALUE_0=/work"));
            assert!(!a.contains(&"-i".to_string()));
        }
        assert!(add.ends_with(&["add".into(), "-A".into()]));
        let image = commit.iter().position(|x| x == IMAGE).expect("image");
        let git = &commit[image + 1..];
        assert!(git.contains(&"core.hooksPath=/dev/null".to_string()));
        assert!(git.contains(&"--no-verify".to_string()));
        assert_eq!(git.last().map(String::as_str), Some(SAVE_MESSAGE));
        assert!(save_edits_args(7, Path::new("/a,b")).is_err());
    }

    #[test]
    fn sessions_move_through_a_sealed_tar() {
        let out = session_export_args(7);
        let inp = session_import_args(7);
        for a in [&out, &inp] {
            assert_sealed(a);
            assert!(a
                .windows(2)
                .any(|w| w[0] == "--entrypoint" && w[1] == "tar"));
            assert!(a
                .windows(2)
                .any(|w| w[0] == "--mount"
                    && w[1] == "type=volume,src=decurion-home-7,dst=/home/agent"));
            assert!(a
                .windows(2)
                .any(|w| w[0] == "--name" && w[1] == "decurion-thread-7-tool"));
        }
        assert!(out.ends_with(&[
            "-C".into(),
            "/home/agent".into(),
            "-cf".into(),
            "-".into(),
            ".claude/projects".into()
        ]));
        assert!(!out.contains(&"-i".to_string()));
        assert!(inp.contains(&"-i".to_string()));
        assert!(inp.ends_with(&["-xf".into(), "-".into()]));
    }

    #[test]
    fn a_copy_is_reused_only_when_it_made_the_latest_checkpoint() {
        assert!(reuse_workspace(true, None, None));
        assert!(reuse_workspace(true, Some("12\n"), Some(12)));
        assert!(!reuse_workspace(false, Some("12"), Some(12)));
        assert!(!reuse_workspace(true, Some("11"), Some(12)));
        assert!(!reuse_workspace(true, None, Some(12)));
        assert!(!reuse_workspace(true, Some("12"), None));
        assert!(!reuse_workspace(true, Some("junk"), None));
    }

    #[test]
    fn bundles_are_thin_unless_there_is_nothing_new() {
        let base = "a".repeat(40);
        assert_eq!(
            bundle_spec(&base, "decurion/7", 3),
            format!("{base}..refs/heads/decurion/7")
        );
        assert_eq!(bundle_spec(&base, "decurion/7", 0), "refs/heads/decurion/7");
    }

    #[test]
    fn commit_ids_and_notes() {
        assert!(is_sha(&"0f".repeat(20)));
        assert!(!is_sha("unknown") && !is_sha("--upload-pack=x") && !is_sha(&"g".repeat(40)));
        assert_eq!(move_note(Place::Cloud), "moved to the cloud");
        assert_eq!(move_note(Place::Pc), "moved to the PC");
    }

    #[test]
    fn host_git_ignores_outside_config_hooks_and_lfs() {
        let env = host_git_env();
        for want in [
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", NULL_DEVICE),
            ("GIT_LFS_SKIP_SMUDGE", "1"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_NO_LAZY_FETCH", "1"),
        ] {
            assert!(env.contains(&want), "{want:?}");
        }
        assert_eq!(NULL_DEVICE, if cfg!(windows) { "NUL" } else { "/dev/null" });
        let args = host_git_args();
        for want in [
            format!("core.hooksPath={NULL_DEVICE}"),
            "core.fsmonitor=false".to_owned(),
            "user.name=decurion".to_owned(),
            "user.email=decurion@decurion.invalid".to_owned(),
        ] {
            assert!(
                args.windows(2).any(|w| w[0] == "-c" && w[1] == want),
                "{want}"
            );
        }
        // Every setting comes as `-c <key=value>`, so none is read as a command.
        assert!(args.chunks(2).all(|p| p[0] == "-c" && p[1].contains('=')));
    }
}
