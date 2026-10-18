//! Where new work starts: the app's default branch as its git server has it,
//! never whatever branch the owner's folder happens to be on.
//!
//! Rules that must stay true:
//!
//! - **The server says, not the folder's memory.** The default branch comes
//!   from `git ls-remote --symref origin HEAD` run in the owner's folder (the
//!   remote by name, read-only, with the owner's own sign-in). The folder's
//!   saved `origin/HEAD` and its local branches are never trusted for it.
//!   The owner's folder is only ever read.
//! - **The server's answer lands in a copy of its own.** That branch is
//!   fetched into a bare repo used for nothing else
//!   ([`Files::upstream_cache`], `app-<id>-upstream.git`), which borrows the
//!   objects of the hub's copy of the app ([`Files::app_cache`]) through
//!   `objects/info/alternates`. The folder's own branches are read into the
//!   hub's copy first, so even a first fetch only brings what is new. The
//!   hub's copy then takes the one branch from it with a local fetch, under
//!   `git_lock`. Nothing from the network writes the hub's copy directly.
//! - **No secrets in arguments or logs.** The fetch goes to the origin
//!   address with any password, or a token used as the user name, taken out
//!   ([`credential_free`]); the user name stays, and so does the folder's own
//!   `core.sshCommand`, so git's own sign-in still finds the owner. Every git
//!   message is passed through [`scrub`] before it is logged.
//! - **Never waits on a question, never for long.** Both network steps run
//!   through [`netgit`], each with a time limit of [`NET_SECS`].
//! - **Network work holds no lock other work needs.** It runs outside
//!   `git_lock`, under a lock of the app's own ([`Locks`]), so two requests
//!   for one app do not ask the server twice. Reading the folder, importing
//!   the one branch, choosing and pinning hold `git_lock`, and are local.
//! - **A start never disappears.** The chosen commit is pinned at
//!   `refs/decurion/start/<sha>` in the hub's copy before anything uses it.
//! - **Falls back in order, and says so once.** With no `origin`, or a
//!   server that cannot be reached, is slow or names no default: the default
//!   it gave last time for this same folder and address
//!   (`refs/upstream-default/<id>`, keyed by [`identity`], so a changed
//!   folder never inherits the old one's); else the folder's own `main` or
//!   `master`; else whatever the folder has out, which is how Decurion chose
//!   before. A fallback is logged once per folder and address, scrubbed,
//!   until the server answers again.

use crate::checkpoints::{git, io, is_commit, Files};
use crate::github::scrub;
use crate::netgit;
use crate::protected::plain_branch;
use crate::store::{Result, StoreError};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The longest each network step (asking, then fetching) may take. A first
/// fetch of a large app can take a while even though it only brings what
/// the folder does not already have.
pub(crate) const NET_SECS: u64 = 120;

/// One app's lock for asking its git server, holding the last answer and
/// when it was ready.
type AppLock = Arc<tokio::sync::Mutex<Option<(Instant, String)>>>;

/// One lock per app for asking its git server, so a request that waited
/// behind another one asking at the same moment takes that answer instead
/// of asking again; and which folders' fallbacks were already logged.
#[derive(Default)]
pub(crate) struct Locks {
    apps: Mutex<HashMap<i64, AppLock>>,
    warned: Mutex<HashSet<String>>,
}

impl Locks {
    fn for_app(&self, app_id: i64) -> AppLock {
        // A poisoned map only means another request panicked; the locks in
        // it are still good.
        let mut map = self.apps.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(app_id).or_default().clone()
    }

    /// True the first time the server for folder-and-address `id` could not
    /// be asked since it last answered, so a long outage is logged once.
    pub(crate) fn first_fallback(&self, id: &str) -> bool {
        let mut warned = self.warned.lock().unwrap_or_else(|e| e.into_inner());
        warned.insert(id.to_owned())
    }

    /// The server for `id` answered: its next fallback is news again.
    pub(crate) fn answered(&self, id: &str) {
        let mut warned = self.warned.lock().unwrap_or_else(|e| e.into_inner());
        warned.remove(id);
    }
}

/// The commit new work in this app starts from, pinned in the hub's copy.
/// Fails only when even the folder's own current commit cannot be read.
pub(crate) async fn start_commit(files: &Files, app_id: i64, repo_path: &str) -> Result<String> {
    let asked_at = Instant::now();
    let lock = files.upstream.for_app(app_id);
    let mut last = lock.lock().await;
    if let Some((ready, sha)) = last.as_ref() {
        if *ready >= asked_at {
            return Ok(sha.clone());
        }
    }
    let sha = resolve(files, app_id, repo_path).await?;
    *last = Some((Instant::now(), sha.clone()));
    Ok(sha)
}

/// The start for a goal about to be created, chosen before its write
/// transaction opens. `None` when this hub keeps no saved work, or not even
/// the folder's own commit can be read; that goal's helpers then copy the
/// folder as before, and fail there the way they always would have.
pub(crate) async fn for_new_goal(st: &crate::AppState, app_id: i64) -> Option<String> {
    let files = st.files.clone()?;
    let app = st.store.app(app_id).await.ok()?;
    match start_commit(&files, app.id, &app.repo_path).await {
        Ok(sha) => Some(sha),
        Err(e) => {
            tracing::warn!(app = app_id, error = %scrub(&e.to_string()), "no start commit for a goal");
            None
        }
    }
}

/// A fresh start for the new pieces of a later round of goal `goal`, when
/// approving `plan_message_id` would add pieces to a goal that already has
/// some. `None` otherwise: the first approval keeps the start the goal was
/// created with, and a request that would be refused never waits on the
/// network.
pub(crate) async fn for_next_round(
    st: &crate::AppState,
    goal: i64,
    plan_message_id: i64,
) -> Option<String> {
    match st.store.next_round(goal, plan_message_id).await {
        Ok(Some(app_id)) => for_new_goal(st, app_id).await,
        _ => None,
    }
}

/// Why the server's default could not be used this time.
enum NotAsked {
    NoOrigin,
    TooSlow,
    NoGit(std::io::Error),
    /// git's own message, already scrubbed.
    Failed(String),
    NoDefault,
}

impl std::fmt::Display for NotAsked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoOrigin => f.write_str("the folder has no remote named origin"),
            Self::TooSlow => write!(f, "the server did not answer within {NET_SECS} seconds"),
            Self::NoGit(e) => write!(f, "git could not be started: {e}"),
            Self::Failed(e) => write!(f, "git said: {e}"),
            Self::NoDefault => f.write_str("the server named no default branch"),
        }
    }
}

async fn resolve(files: &Files, app_id: i64, repo_path: &str) -> Result<String> {
    let folder = Path::new(repo_path);
    let cache = files.app_cache(app_id);
    // The folder's own branches first, locally, so the server only has to
    // send what is new.
    let read_folder = {
        let _held = files.git_lock.lock().await;
        files.fetch_app(&cache, repo_path).await
    };
    let url = git(folder, &["remote", "get-url", "origin"])
        .await
        .ok()
        .filter(|u| !u.is_empty())
        .map(|u| fetch_url(folder, &u));
    let id = identity(repo_path, url.as_deref().unwrap_or(""));
    let asked = match (&url, &read_folder) {
        (None, _) => Err(NotAsked::NoOrigin),
        (Some(_), Err(e)) => Err(NotAsked::Failed(scrub(&e.to_string()))),
        (Some(u), Ok(())) => ask_server(files, app_id, folder, u, &id).await,
    };
    let _held = files.git_lock.lock().await;
    let sha = match asked {
        Ok(branch) => {
            let from = files.upstream_cache(app_id).to_string_lossy().into_owned();
            let name = format!("refs/upstream/{id}/{branch}");
            git(
                &cache,
                &[
                    "-c",
                    "protocol.allow=never",
                    "-c",
                    "protocol.file.allow=always",
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--no-write-fetch-head",
                    "--",
                    &from,
                    &format!("+{name}:{name}"),
                ],
            )
            .await?;
            let sha = git(
                &cache,
                &["rev-parse", "--verify", &format!("{name}^{{commit}}")],
            )
            .await?;
            git(
                &cache,
                &["update-ref", &format!("refs/upstream-default/{id}"), &sha],
            )
            .await?;
            files.upstream.answered(&id);
            sha
        }
        Err(why) => {
            if read_folder.is_err() {
                files.fetch_app(&cache, repo_path).await?;
            }
            let (sha, used) = fall_back(&cache, folder, &id).await?;
            if files.upstream.first_fallback(&id) {
                tracing::warn!(
                    app = app_id,
                    reason = %scrub(&why.to_string()),
                    used,
                    "could not ask the app's git server where new work starts"
                );
            }
            sha
        }
    };
    if !is_commit(&sha) {
        return Err(StoreError::Invalid(format!("not a commit id: {sha}")));
    }
    git(
        &cache,
        &["update-ref", &format!("refs/decurion/start/{sha}"), &sha],
    )
    .await?;
    Ok(sha)
}

/// Asks the server its default branch and fetches that branch into the
/// app's server copy. Returns the branch name. The caller holds the app's
/// own lock, which is all the server copy needs.
async fn ask_server(
    files: &Files,
    app_id: i64,
    folder: &Path,
    url: &str,
    id: &str,
) -> std::result::Result<String, NotAsked> {
    let limit = Duration::from_secs(NET_SECS);
    let mut cmd = netgit::command(folder).await;
    cmd.args(["ls-remote", "--symref", "origin", "HEAD"]);
    let out = run(cmd, limit).await?;
    let branch =
        advertised_default(&String::from_utf8_lossy(&out.stdout)).ok_or(NotAsked::NoDefault)?;
    let up = files.upstream_cache(app_id);
    files
        .init_upstream_cache(&up, &files.app_cache(app_id))
        .await
        .map_err(|e| NotAsked::Failed(scrub(&e.to_string())))?;
    let ssh = owner_ssh_command(folder).await;
    let mut cmd = netgit::command_with(&up, ssh.as_deref()).await;
    cmd.args(fetch_args(url, &branch, id));
    run(cmd, limit).await?;
    Ok(branch)
}

async fn run(
    cmd: tokio::process::Command,
    limit: Duration,
) -> std::result::Result<std::process::Output, NotAsked> {
    match netgit::output(cmd, limit).await {
        Err(netgit::Failed::TooSlow) => Err(NotAsked::TooSlow),
        Err(netgit::Failed::NotStarted(e)) => Err(NotAsked::NoGit(e)),
        Ok(out) if !out.status.success() => Err(NotAsked::Failed(scrub(
            String::from_utf8_lossy(&out.stderr).trim(),
        ))),
        Ok(out) => Ok(out),
    }
}

/// The owner's folder's own `core.sshCommand`, set in that repo, if any:
/// the fetch runs elsewhere, so it would not see it otherwise.
async fn owner_ssh_command(folder: &Path) -> Option<String> {
    git(folder, &["config", "--local", "--get", "core.sshCommand"])
        .await
        .ok()
        .filter(|c| !c.is_empty())
}

/// The start when the server's default cannot be had, and which fallback
/// gave it. The caller holds `git_lock` and has read the folder into the
/// hub's copy.
async fn fall_back(cache: &Path, folder: &Path, id: &str) -> Result<(String, &'static str)> {
    if let Ok(sha) = git(
        cache,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/upstream-default/{id}^{{commit}}"),
        ],
    )
    .await
    {
        return Ok((sha, "the default the server gave last time"));
    }
    // Read from the folder itself: the hub's copy may still hold branches
    // of a folder the app used before.
    for (b, used) in [
        ("main", "the folder's main branch"),
        ("master", "the folder's master branch"),
    ] {
        if let Ok(sha) = git(
            folder,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}^{{commit}}"),
            ],
        )
        .await
        {
            return Ok((sha, used));
        }
    }
    Ok((
        git(cache, &["rev-parse", "refs/heads/decurion-latest"]).await?,
        "the folder's current branch",
    ))
}

/// The default branch in `git ls-remote --symref <remote> HEAD`'s answer,
/// if it names a plain branch.
pub(crate) fn advertised_default(ls_remote: &str) -> Option<String> {
    ls_remote.lines().find_map(|line| {
        let (target, name) = line.split_once('\t')?;
        if name.trim() != "HEAD" {
            return None;
        }
        let branch = target.strip_prefix("ref: refs/heads/")?;
        plain_branch(branch).then(|| branch.to_owned())
    })
}

/// git's arguments for fetching the server's default branch into the app's
/// server copy, after what [`netgit::command_with`] puts first. Exactly one
/// source and one destination, none of the remote's own mappings, no tags,
/// no submodules. Pure, so a test can check no secret is ever among them.
pub(crate) fn fetch_args(url: &str, branch: &str, id: &str) -> Vec<String> {
    [
        "-c",
        "gc.auto=0",
        "-c",
        "maintenance.auto=false",
        "fetch",
        "--quiet",
        "--no-tags",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--refmap=",
        "--",
        url,
        &format!("+refs/heads/{branch}:refs/upstream/{id}/{branch}"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// `url` with any password taken out. The user name stays: it is not a
/// secret, and git's own sign-in (Git Credential Manager, SSH) looks the
/// owner up by it. A user name that is itself a token (GitHub or GitLab
/// token marks, or a long opaque string) goes too.
pub(crate) fn credential_free(url: &str) -> String {
    let Some(i) = url.find("://") else {
        return url.to_owned();
    };
    let (scheme, rest) = (&url[..i], &url[i + 3..]);
    let end = rest.find('/').unwrap_or(rest.len());
    let (auth, path) = rest.split_at(end);
    let Some(at) = auth.rfind('@') else {
        return url.to_owned();
    };
    let (user, host) = (&auth[..at], &auth[at + 1..]);
    let login = match user.split(':').next() {
        Some(name) if !name.is_empty() && !looks_like_token(name) => format!("{name}@"),
        _ => String::new(),
    };
    format!("{scheme}://{login}{host}{path}")
}

fn looks_like_token(name: &str) -> bool {
    const MARKS: &[&str] = &[
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
    ];
    MARKS.iter().any(|m| name.starts_with(m)) || name.len() >= 40
}

/// The address to fetch from the server copy: credential-free, and a
/// relative folder path made relative to the owner's folder, where git
/// would read it.
fn fetch_url(folder: &Path, url: &str) -> String {
    let url = credential_free(url);
    let scp_like = url
        .find(':')
        .is_some_and(|i| i > 1 && !url[..i].contains(['/', '\\']));
    if url.contains("://") || scp_like || Path::new(&url).is_absolute() {
        url
    } else {
        folder.join(&url).to_string_lossy().into_owned()
    }
}

/// A short name for one folder and one server address together, so the
/// default saved for one is never used for another.
pub(crate) fn identity(repo_path: &str, url: &str) -> String {
    let folder = std::fs::canonicalize(repo_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| repo_path.to_owned());
    let digest = Sha256::digest(format!("{folder}\n{url}").as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

impl Files {
    /// Makes the app's bare copy if it is not there yet. The caller holds
    /// `git_lock`.
    pub(crate) async fn init_cache(&self, cache: &Path) -> Result<()> {
        if !cache.exists() {
            std::fs::create_dir_all(cache).map_err(io)?;
            git(cache, &["init", "--quiet", "--bare"]).await?;
        }
        Ok(())
    }

    /// The bare repo that only takes fetches from the app's git server.
    pub(crate) fn upstream_cache(&self, app_id: i64) -> PathBuf {
        self.app_cache(app_id)
            .with_file_name(format!("app-{app_id}-upstream.git"))
    }

    /// Makes the app's server copy if needed and has it borrow the objects
    /// of the hub's copy `cache`, so the server is only asked for what is
    /// new. The caller holds the app's own lock.
    pub(crate) async fn init_upstream_cache(&self, up: &Path, cache: &Path) -> Result<()> {
        if !up.join("objects").is_dir() {
            std::fs::create_dir_all(up).map_err(io)?;
            git(up, &["init", "--quiet", "--bare"]).await?;
        }
        let objects = std::path::absolute(cache.join("objects")).map_err(io)?;
        // Forward slashes: git reads them on every system.
        let line = format!("{}\n", objects.to_string_lossy().replace('\\', "/"));
        let info = up.join("objects").join("info");
        std::fs::create_dir_all(&info).map_err(io)?;
        let file = info.join("alternates");
        if std::fs::read_to_string(&file).ok().as_deref() != Some(line.as_str()) {
            std::fs::write(&file, line).map_err(io)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up `user:password` for addresses in these tests, built from
    /// parts so the source itself holds no address with a login in it.
    fn login(user: &str, password: &str) -> String {
        [user, password].join(":")
    }

    #[test]
    fn the_default_branch_is_read_from_the_servers_own_answer() {
        let sha = "a".repeat(40);
        assert_eq!(
            advertised_default(&format!("ref: refs/heads/trunk\tHEAD\n{sha}\tHEAD\n")),
            Some("trunk".to_owned())
        );
        assert_eq!(
            advertised_default("ref: refs/heads/release/2.0\tHEAD\n"),
            Some("release/2.0".to_owned())
        );
        // Only HEAD's target, only a branch, only a plain name.
        assert_eq!(advertised_default(&format!("{sha}\tHEAD\n")), None);
        assert_eq!(advertised_default("ref: refs/tags/v1\tHEAD\n"), None);
        assert_eq!(advertised_default("ref: refs/heads/main\tmain\n"), None);
        assert_eq!(advertised_default("ref: refs/heads/-x\tHEAD\n"), None);
        assert_eq!(advertised_default("ref: refs/heads/a..b\tHEAD\n"), None);
        assert_eq!(advertised_default(""), None);
    }

    #[test]
    fn no_password_or_token_is_ever_in_the_fetch() {
        // The user name stays: git's own sign-in looks the owner up by it.
        let url = credential_free(&format!(
            "https://{}@github.com/acme/shop.git",
            login("me", "ghp_secret123")
        ));
        assert_eq!(url, "https://me@github.com/acme/shop.git");
        let args = fetch_args(&url, "main", "0123456789abcdef");
        assert!(args.iter().all(|a| !a.contains("ghp_secret123")));
        assert!(args.contains(&"--no-tags".to_owned()));
        assert!(args.contains(&"--refmap=".to_owned()));
        assert_eq!(
            args.last().map(String::as_str),
            Some("+refs/heads/main:refs/upstream/0123456789abcdef/main")
        );
        // A token used as the user name is still a token.
        assert_eq!(
            credential_free("https://ghp_token@github.com/a/b"),
            "https://github.com/a/b"
        );
        assert_eq!(
            credential_free(&format!("https://{}@gitlab.com/a/b", "f".repeat(40))),
            "https://gitlab.com/a/b"
        );
        assert_eq!(
            credential_free(&format!(
                "https://{}@github.com/a/b",
                login("x-access-token", "ghs_abc")
            )),
            "https://x-access-token@github.com/a/b"
        );
        // An ssh login is kept; a password with it is not.
        assert_eq!(
            credential_free(&format!("ssh://{}@host.example/a.git", login("git", "pw"))),
            "ssh://git@host.example/a.git"
        );
        assert_eq!(
            credential_free("git@github.com:acme/shop.git"),
            "git@github.com:acme/shop.git"
        );
        assert_eq!(credential_free("/srv/git/shop.git"), "/srv/git/shop.git");
    }

    #[test]
    fn what_a_fallback_logs_has_no_password_in_it() {
        let why = NotAsked::Failed(scrub(&format!(
            "fatal: unable to access 'https://{}@github.com/a/b.git/': 403",
            login("me", "ghp_secret123")
        )));
        let line = scrub(&why.to_string());
        assert!(
            !line.contains("ghp_secret123") && !line.contains("me:"),
            "{line}"
        );
        assert!(line.contains("https://github.com/a/b.git/"), "{line}");
    }

    #[test]
    fn a_relative_server_path_is_read_from_the_owners_folder() {
        let folder = Path::new("repos").join("shop");
        assert_eq!(
            fetch_url(&folder, "../origin.git"),
            folder.join("../origin.git").to_string_lossy()
        );
        assert_eq!(
            fetch_url(&folder, &format!("https://{}@host/x.git", login("u", "p"))),
            "https://u@host/x.git"
        );
        assert_eq!(
            fetch_url(&folder, "git@github.com:a/b.git"),
            "git@github.com:a/b.git"
        );
    }

    #[test]
    fn a_fallback_is_told_once_per_folder_until_the_server_answers_again() {
        let locks = Locks::default();
        assert!(locks.first_fallback("aaaa"));
        assert!(!locks.first_fallback("aaaa"));
        assert!(locks.first_fallback("bbbb"));
        locks.answered("aaaa");
        assert!(locks.first_fallback("aaaa"));
    }

    #[tokio::test]
    async fn the_folders_own_ssh_command_is_read_from_its_own_settings() {
        let dir = std::env::temp_dir().join(format!("decurion-ssh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        git(&dir, &["init", "--quiet"]).await.expect("init");
        assert_eq!(owner_ssh_command(&dir).await, None);
        git(&dir, &["config", "core.sshCommand", "ssh -i ~/.ssh/shop"])
            .await
            .expect("config");
        assert_eq!(
            owner_ssh_command(&dir).await.as_deref(),
            Some("ssh -i ~/.ssh/shop")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_folder_and_address_has_its_own_saved_default() {
        let a = identity("C:\\repos\\shop", "https://github.com/a/shop.git");
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            a,
            identity("C:\\repos\\shop", "https://github.com/a/shop.git")
        );
        assert_ne!(
            a,
            identity("C:\\repos\\other", "https://github.com/a/shop.git")
        );
        assert_ne!(
            a,
            identity("C:\\repos\\shop", "https://github.com/a/fork.git")
        );
    }
}
