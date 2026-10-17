//! Saved copies of work, so any runner can continue it.
//!
//! The hub is the meeting point for code. A runner never clones the owner's
//! repo itself: it asks the hub for the thread's starting point (a git bundle
//! of one commit), and at the end of every run, or when the owner moves the
//! work, it uploads a saved copy: the branch as a git bundle plus the agent's
//! Claude Code session files. Whichever computer runs the work next rebuilds
//! from that copy.
//!
//! The hub never unpacks what runners upload. It stores the files and hands
//! them to the next runner, which unpacks the session inside a sealed
//! container and reads the code with git's own checks.

use crate::store::{runner_actor, Result, Store, StoreError};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// The two parts of a saved copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The thread's branch as a git bundle.
    Code,
    /// A tar of the agent's `.claude/projects` folder.
    Session,
}

impl Part {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "code" => Some(Self::Code),
            "session" => Some(Self::Session),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Session => "session",
        }
    }

    /// Largest upload accepted.
    pub fn max_bytes(self) -> u64 {
        match self {
            Self::Code => 2 << 30,
            Self::Session => 512 << 20,
        }
    }
}

/// How many saved copies of each piece of work are kept on disk. Older ones
/// are deleted; only the latest is ever handed out.
const KEEP: i64 = 2;

/// A full git commit id.
pub fn is_commit(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Where the hub keeps saved copies and starting-point bundles.
pub struct Files {
    dir: PathBuf,
    /// Git work in the hub's own repo caches happens one at a time.
    git_lock: tokio::sync::Mutex<()>,
}

impl Files {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            git_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn thread_dir(&self, thread: i64) -> PathBuf {
        self.dir.join("checkpoints").join(thread.to_string())
    }

    /// Where an upload waits until the runner says the saved copy is complete.
    pub fn incoming(&self, thread: i64, part: Part) -> PathBuf {
        self.thread_dir(thread)
            .join(format!("incoming-{}", part.as_str()))
    }

    pub fn saved(&self, thread: i64, checkpoint: i64, part: Part) -> PathBuf {
        self.thread_dir(thread)
            .join(format!("{checkpoint}.{}", part.as_str()))
    }

    pub fn prepare_thread_dir(&self, thread: i64) -> std::io::Result<()> {
        std::fs::create_dir_all(self.thread_dir(thread))
    }

    /// The bundle holding a thread's starting commit, made on first request
    /// from the owner's own checkout. The bundle has one ref,
    /// `refs/heads/decurion-base`.
    pub async fn base_bundle(
        &self,
        app_id: i64,
        repo_path: &str,
        base: Option<&str>,
    ) -> Result<(String, PathBuf)> {
        let _held = self.git_lock.lock().await;
        let cache = self.dir.join("repos").join(format!("app-{app_id}.git"));
        if let Some(sha) = base {
            let path = self.bundle_path(sha);
            if path.exists() {
                return Ok((sha.to_owned(), path));
            }
        }
        if !cache.exists() {
            std::fs::create_dir_all(&cache).map_err(io)?;
            git(&cache, &["init", "--quiet", "--bare"]).await?;
        }
        // The owner's repo, not an agent's: still, no hooks and nothing but
        // local files.
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
                "--",
                repo_path,
                "+refs/heads/*:refs/remotes/app/*",
                "+HEAD:refs/heads/decurion-latest",
            ],
        )
        .await?;
        let sha = match base {
            Some(s) => s.to_owned(),
            None => git(&cache, &["rev-parse", "refs/heads/decurion-latest"]).await?,
        };
        if !is_commit(&sha) {
            return Err(StoreError::Invalid(format!("not a commit id: {sha}")));
        }
        if git(&cache, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .await
            .is_err()
        {
            return Err(StoreError::Refused(
                "The commit this work started from is no longer in the app's repo.".into(),
            ));
        }
        let path = self.bundle_path(&sha);
        if !path.exists() {
            std::fs::create_dir_all(self.dir.join("bundles")).map_err(io)?;
            git(&cache, &["update-ref", "refs/heads/decurion-base", &sha]).await?;
            let tmp = path.with_extension("partial");
            let tmp_s = tmp.to_string_lossy().into_owned();
            git(
                &cache,
                &[
                    "bundle",
                    "create",
                    "--quiet",
                    &tmp_s,
                    "refs/heads/decurion-base",
                ],
            )
            .await?;
            std::fs::rename(&tmp, &path).map_err(io)?;
        }
        Ok((sha, path))
    }

    fn bundle_path(&self, sha: &str) -> PathBuf {
        self.dir.join("bundles").join(format!("base-{sha}.bundle"))
    }
}

fn io(e: std::io::Error) -> StoreError {
    StoreError::Invalid(format!("file error: {e}"))
}

async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let out = Command::new("git")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            &format!("core.hooksPath={null}"),
            "-c",
            "core.fsmonitor=false",
        ])
        .args(args)
        .output()
        .await
        .map_err(io)?;
    if !out.status.success() {
        return Err(StoreError::Invalid(format!(
            "git {}: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

impl Store {
    /// True if `thread` is running on the runner called `runner`.
    pub async fn running_on(&self, thread: i64, runner: &str) -> Result<bool> {
        let actor = runner_actor(runner)?;
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT state, runner FROM threads WHERE id = ?")
                .bind(thread)
                .fetch_optional(self.pool())
                .await?;
        let (state, holder) = row.ok_or(StoreError::NotFound)?;
        Ok(state == "running" && holder.as_deref() == Some(actor.as_str()))
    }

    pub async fn latest_checkpoint(&self, thread: i64) -> Result<Option<i64>> {
        Ok(
            sqlx::query_scalar("SELECT MAX(id) FROM checkpoints WHERE thread_id = ?")
                .bind(thread)
                .fetch_one(self.pool())
                .await?,
        )
    }

    pub async fn checkpoint_belongs(&self, thread: i64, checkpoint: i64) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM checkpoints WHERE id = ? AND thread_id = ?)",
        )
        .bind(checkpoint)
        .bind(thread)
        .fetch_one(self.pool())
        .await?)
    }

    /// The commit a thread's copy starts from, fixed once. Returns the one
    /// that stands, which is `sha` unless one was already set.
    pub async fn fix_base_commit(&self, thread: i64, sha: &str) -> Result<String> {
        sqlx::query("UPDATE threads SET base_commit = ? WHERE id = ? AND base_commit IS NULL")
            .bind(sha)
            .bind(thread)
            .execute(self.pool())
            .await?;
        self.base_commit(thread).await?.ok_or(StoreError::NotFound)
    }

    pub async fn base_commit(&self, thread: i64) -> Result<Option<String>> {
        let v: Option<Option<String>> =
            sqlx::query_scalar("SELECT base_commit FROM threads WHERE id = ?")
                .bind(thread)
                .fetch_optional(self.pool())
                .await?;
        v.ok_or(StoreError::NotFound)
    }

    /// Records a saved copy whose two parts the runner has uploaded, moving
    /// the uploads into place under the new id. Only the runner the work is
    /// running on may do this. Older copies beyond the newest few are deleted.
    pub async fn add_checkpoint(
        &self,
        files: &Files,
        thread: i64,
        runner: &str,
        head: &str,
        base: Option<&str>,
    ) -> Result<i64> {
        if !is_commit(head) || base.is_some_and(|b| !is_commit(b)) {
            return Err(StoreError::Invalid(
                "head and base must be commit ids".into(),
            ));
        }
        let actor = runner_actor(runner)?;
        for part in [Part::Code, Part::Session] {
            if !files.incoming(thread, part).is_file() {
                return Err(StoreError::Refused(format!(
                    "upload the {} part first",
                    part.as_str()
                )));
            }
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM threads WHERE id = ? AND state = 'running' AND runner = ?)",
        )
        .bind(thread)
        .bind(&actor)
        .fetch_one(&mut *tx)
        .await?;
        if !ok {
            return Err(StoreError::Conflict);
        }
        if let Some(b) = base {
            sqlx::query("UPDATE threads SET base_commit = ? WHERE id = ? AND base_commit IS NULL")
                .bind(b)
                .bind(thread)
                .execute(&mut *tx)
                .await?;
        }
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO checkpoints (thread_id, runner, head) VALUES (?, ?, ?) RETURNING id",
        )
        .bind(thread)
        .bind(&actor)
        .bind(head)
        .fetch_one(&mut *tx)
        .await?;
        for part in [Part::Code, Part::Session] {
            std::fs::rename(files.incoming(thread, part), files.saved(thread, id, part))
                .map_err(io)?;
        }
        tx.commit().await?;
        let old: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM checkpoints WHERE thread_id = ? ORDER BY id DESC LIMIT -1 OFFSET ?",
        )
        .bind(thread)
        .bind(KEEP)
        .fetch_all(self.pool())
        .await?;
        for cp in old {
            for part in [Part::Code, Part::Session] {
                let _ = std::fs::remove_file(files.saved(thread, cp, part));
            }
        }
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_ids_are_full_lowercase_hex() {
        assert!(is_commit(&"a".repeat(40)));
        assert!(!is_commit(&"A".repeat(40)));
        assert!(!is_commit("HEAD"));
        assert!(!is_commit(&format!("--{}", "a".repeat(38))));
    }

    #[test]
    fn parts_parse_and_have_limits() {
        assert_eq!(Part::parse("code"), Some(Part::Code));
        assert_eq!(Part::parse("../x"), None);
        assert!(Part::Session.max_bytes() < Part::Code.max_bytes());
    }
}
