//! Handing work between computers: rebuilding a thread's copy from the hub,
//! saving it back as a checkpoint, and the sealed throwaway containers that
//! touch the agent's files on the runner's behalf (saving edits, moving
//! sessions, putting the owner's attached files in the agent's home).
//!
//! Everything here is decided without Docker, git or a hub, so it can be
//! tested directly; `main.rs` does the work.

use crate::{home_mount, work_mount, GIT_ENV, IMAGE};
use decurion_proto::context::{MAX_APP_BYTES, MAX_FILES, MAX_TEXT_BYTES};
use decurion_proto::Place;
use std::path::Path;

/// Largest code bundle the runner downloads or uploads (the hub's limit).
pub const CODE_CAP: u64 = 2 << 30;
/// Largest session archive the runner downloads or uploads (the hub's limit).
pub const SESSION_CAP: u64 = 512 << 20;
/// Largest archive of attached files the runner downloads: every file of the
/// app, a text version of each, and room for the tar headers.
pub const CONTEXT_CAP: u64 =
    MAX_APP_BYTES + MAX_FILES as u64 * (MAX_TEXT_BYTES as u64 + 4096) + (1 << 20);

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
/// `name` is the container's name, so a timed-out one can be removed by it.
fn sealed_args(
    name: &str,
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
        name,
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
            &tool_container_name(thread_id),
            &work,
            false,
            GIT_ENV,
            "git",
            &["-C", "/work", "add", "-A"],
        ),
        sealed_args(
            &tool_container_name(thread_id),
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

/// `docker run` that writes the agent's sessions as a tar archive to stdout:
/// Claude Code's, or pi's for ChatGPT and Grok work. A thread has only one of
/// them, so a missing folder is skipped rather than failing the save.
pub fn session_export_args(thread_id: i64) -> Vec<String> {
    sealed_args(
        &tool_container_name(thread_id),
        &home_mount(thread_id),
        false,
        &[],
        "tar",
        &[
            "--ignore-failed-read",
            "-C",
            "/home/agent",
            "-cf",
            "-",
            ".claude/projects",
            ".pi/sessions",
        ],
    )
}

/// `docker run` that unpacks a session archive from stdin into the agent's
/// (fresh) home volume.
pub fn session_import_args(thread_id: i64) -> Vec<String> {
    sealed_args(
        &tool_container_name(thread_id),
        &home_mount(thread_id),
        true,
        &[],
        "tar",
        &["-C", "/home/agent", "-xf", "-"],
    )
}

/// The one program the attached-files box runs, under `sh` (dash in the
/// image). It reads the hub's archive on stdin and never sees a file name
/// from anywhere else: names only ever reach it inside the archive and are
/// used quoted, so spaces and brackets are plain characters.
///
/// What must stay true:
/// - The old folder goes entirely first, so a file the owner removed, or
///   anything the agent left there, is gone. A folder the agent made
///   unwritable is made writable again first, but a link is never followed.
/// - PDFs get `<name>.txt` here, inside the box, because the hub does not
///   read them: each try is cut at 60 seconds and [`MAX_TEXT_BYTES`], all of
///   them stop after ten minutes (inside the runner's own limit for the box),
///   and a PDF without text simply gets none. Without `pdftotext` (an older
///   image) there are no PDF text versions, and nothing fails.
/// - Files end read-only, so an agent sees they are copies; the folder stays
///   writable by its owner so the next run can remove it.
pub const CONTEXT_SCRIPT: &str = r#"set -eu
d=/home/agent/context
if [ -d "$d" ] && [ ! -L "$d" ]; then chmod -R u+rwX "$d" 2>/dev/null || true; fi
rm -rf "$d"
mkdir "$d"
tar -C "$d" -xf - --no-same-owner
if command -v pdftotext >/dev/null 2>&1; then
  end=$(( $(date +%s) + 600 ))
  for f in "$d"/*.[pP][dD][fF]; do
    [ -f "$f" ] || continue
    [ ! -e "$f.txt" ] || continue
    [ "$(date +%s)" -lt "$end" ] || break
    timeout 60 pdftotext -q -enc UTF-8 "$f" - 2>/dev/null | head -c 4194304 > "$f.txt" || true
    [ -s "$f.txt" ] || rm -f "$f.txt"
  done
fi
find "$d" -type f -exec chmod a-w {} +
"#;

/// `docker run` that puts the owner's attached files (a tar archive on
/// stdin) in a fresh `/home/agent/context` in `home_volume`, and adds PDF
/// text versions there. Sealed like the other throwaway boxes: no network,
/// and the agent's home volume as its only mount. Work runs and helper jobs
/// both use it, each with its own container name and volume.
pub fn context_import_args(container_name: &str, home_volume: &str) -> Vec<String> {
    sealed_args(
        container_name,
        &format!("type=volume,src={home_volume},dst=/home/agent"),
        true,
        &[],
        "sh",
        &["-c", CONTEXT_SCRIPT],
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

/// [`bundle_spec`] for a saved copy, as `git bundle create` arguments. For
/// a Bring up to date run whose branch holds the main commit it merged
/// (`main`), that commit's history is left out too (`^<main>`): the hub
/// keeps it pinned and every copy of the work starts with it, so it never
/// needs to travel again.
pub fn bundle_specs(base: &str, branch: &str, new_commits: u64, main: Option<&str>) -> Vec<String> {
    let mut v = vec![bundle_spec(base, branch, new_commits)];
    if new_commits > 0 {
        v.extend(main.map(|m| format!("^{m}")));
    }
    v
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
/// anything commits (there is no user config to take one from). Long paths
/// are on because Git for Windows otherwise refuses any file whose full path
/// passes 260 characters, and the PC's own setting for that is not read.
pub fn host_git_args() -> Vec<String> {
    [
        format!("core.hooksPath={NULL_DEVICE}"),
        "core.fsmonitor=false".to_owned(),
        "core.longpaths=true".to_owned(),
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
            "--ignore-failed-read".into(),
            "-C".into(),
            "/home/agent".into(),
            "-cf".into(),
            "-".into(),
            ".claude/projects".into(),
            ".pi/sessions".into()
        ]));
        assert!(!out.contains(&"-i".to_string()));
        assert!(inp.contains(&"-i".to_string()));
        assert!(inp.ends_with(&["-xf".into(), "-".into()]));
    }

    #[test]
    fn attached_files_go_in_through_a_sealed_box_with_only_the_home_volume() {
        for (name, vol) in [
            ("decurion-thread-7-tool", "decurion-home-7"),
            ("decurion-helper-9-tool", "decurion-goal-home-3"),
        ] {
            let a = context_import_args(name, vol);
            assert_sealed(&a);
            let image = a.iter().position(|x| x == IMAGE).expect("image");
            let opts = &a[..image];
            assert!(opts.windows(2).any(|w| w[0] == "--name" && w[1] == name));
            let mounts: Vec<&String> = opts
                .windows(2)
                .filter(|w| w[0] == "--mount")
                .map(|w| &w[1])
                .collect();
            assert_eq!(
                mounts,
                [&format!("type=volume,src={vol},dst=/home/agent")],
                "the home volume is the one mount"
            );
            assert!(!a.iter().any(|x| x.contains("type=bind") || x == "-v"));
            assert!(!a.iter().any(|x| x == "--privileged" || x.contains("host")));
            assert!(opts.windows(2).any(|w| w[0] == "--memory"));
            assert!(opts.windows(2).any(|w| w[0] == "--pids-limit"));
            assert!(opts.contains(&"-i".to_string()));
            assert!(!opts.contains(&"-e".to_string()));
            assert!(opts
                .windows(2)
                .any(|w| w[0] == "--entrypoint" && w[1] == "sh"));
            // The script is fixed: nothing from the call goes into it.
            assert_eq!(&a[image + 1..], ["-c", CONTEXT_SCRIPT]);
        }
    }

    #[test]
    fn the_attached_files_script_replaces_the_folder_and_caps_pdf_text() {
        let s = CONTEXT_SCRIPT;
        assert!(s.contains(&format!("head -c {MAX_TEXT_BYTES} ")));
        assert!(s.contains("d=/home/agent/context\n"));
        assert_eq!(decurion_proto::context::BOX_DIR, "/home/agent/context");
        let rm = s.find("rm -rf \"$d\"").expect("old folder removed");
        let tar = s
            .find("tar -C \"$d\" -xf - --no-same-owner")
            .expect("unpacked");
        let pdf = s
            .find("pdftotext -q -enc UTF-8 \"$f\" -")
            .expect("pdf text");
        let ro = s.find("chmod a-w").expect("read-only");
        assert!(rm < tar && tar < pdf && pdf < ro);
        assert!(s.contains("timeout 60 pdftotext"));
        // Every name is used quoted, and bash-only syntax would break dash.
        for bad in ["[[", "pipefail", "$f.txt ", " $f ", "${", "\\`"] {
            assert!(!s.contains(bad), "{bad}");
        }
        assert!(CONTEXT_CAP > MAX_APP_BYTES + MAX_FILES as u64 * MAX_TEXT_BYTES as u64);
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
        let main = "b".repeat(40);
        assert_eq!(
            bundle_specs(&base, "decurion/7", 3, Some(&main)),
            [format!("{base}..refs/heads/decurion/7"), format!("^{main}")]
        );
        assert_eq!(
            bundle_specs(&base, "decurion/7", 0, Some(&main)),
            ["refs/heads/decurion/7"]
        );
        assert_eq!(
            bundle_specs(&base, "decurion/7", 3, None),
            [format!("{base}..refs/heads/decurion/7")]
        );
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
            "core.longpaths=true".to_owned(),
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
