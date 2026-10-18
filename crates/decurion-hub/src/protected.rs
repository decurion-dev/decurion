//! Protected parts of an app: files the owner wants to look at themselves.
//!
//! An app's repo may hold `.decurion/protected-paths`, a list of the parts
//! of the app that matter most (its walls, its money, its keys). When the
//! checker passes a piece of work that changed any of them, the piece is
//! held for the owner instead of passing. This is decided here, by the hub,
//! and never by an agent: the checker only says whether the work is good.
//!
//! The list is always read from the commit the work started from, never
//! from the work's own branch, so the agent being checked cannot take
//! anything off it. A change to the list itself is held too.
//!
//! The list, one pattern per line:
//! - Blank lines and lines starting with `#` are skipped. Spaces around a
//!   line are ignored.
//! - A pattern is matched against the whole path from the top of the repo,
//!   such as `src/billing/charge.rs`. A leading `/` is allowed and changes
//!   nothing.
//! - `*` matches any run of characters inside one folder or file name, and
//!   `?` matches one such character. Neither crosses a `/`.
//! - `**` as a whole name matches any number of folders, none included, so
//!   `src/**/secret.rs` covers `src/secret.rs` and `src/a/b/secret.rs`.
//! - A pattern that matches a folder covers everything inside it. A trailing
//!   `/`, as in `docker/`, only makes that plain to the reader.
//! - Matching ignores ASCII letter case, because Windows and macOS treat
//!   `Lib.rs` and `lib.rs` as the same file.
//! - There is no `!` to make exceptions: such a line is an ordinary pattern,
//!   so nothing in the list can unprotect a file.

use crate::checkpoints::{git, is_commit, Files, Part};
use crate::store::{Result, Store, StoreError};
use decurion_proto::goal::{last_json_object, HelperResult, ReviewVerdict};
use sqlx::Row;

/// Where an app keeps its list of protected parts.
pub const LIST_FILE: &str = ".decurion/protected-paths";

/// How many changed paths the owner is shown before "and N more".
const SHOWN: usize = 5;

/// A parsed list of protected parts.
#[derive(Debug, Default)]
pub struct Patterns(Vec<Vec<String>>);

/// Reads the list's text. Every line that is not blank or a comment becomes
/// a pattern; there is nothing that can fail.
pub fn parse(text: &str) -> Patterns {
    Patterns(
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| {
                l.split('/')
                    .filter(|s| !s.is_empty())
                    .map(str::to_ascii_lowercase)
                    .collect::<Vec<_>>()
            })
            // A line of only slashes names no file.
            .filter(|p| !p.is_empty())
            .collect(),
    )
}

impl Patterns {
    /// True if `path` (from the top of the repo, `/` between names) is
    /// covered by any pattern.
    pub fn matches(&self, path: &str) -> bool {
        let path = path.to_ascii_lowercase();
        let names: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        self.0.iter().any(|p| covers(p, &names))
    }
}

/// True if `pat` matches `names` or one of the folders holding it.
fn covers(pat: &[String], names: &[&str]) -> bool {
    match pat.split_first() {
        // The whole pattern is used up: what is left of the path is inside
        // the matched folder (or is the matched file itself).
        None => true,
        Some((first, rest)) if first == "**" => {
            (0..=names.len()).any(|skip| covers(rest, &names[skip..]))
        }
        Some((first, rest)) => match names.split_first() {
            Some((name, more)) => {
                name_matches(first.as_bytes(), name.as_bytes()) && covers(rest, more)
            }
            None => false,
        },
    }
}

/// One folder or file name against one pattern piece with `*` and `?`.
fn name_matches(pat: &[u8], name: &[u8]) -> bool {
    match pat.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|i| name_matches(rest, &name[i..])),
        Some((b'?', rest)) => !name.is_empty() && name_matches(rest, &name[1..]),
        Some((c, rest)) => name.first() == Some(c) && name_matches(rest, &name[1..]),
    }
}

/// The changed paths the owner must look at: those the list covers, and the
/// list itself.
pub fn protected_changes(patterns: &Patterns, changed: &[String]) -> Vec<String> {
    changed
        .iter()
        .filter(|p| p.eq_ignore_ascii_case(LIST_FILE) || patterns.matches(p))
        .cloned()
        .collect()
}

/// Whether a piece the checker passed must wait for the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// Nothing protected changed, or the app protects nothing.
    Clear,
    /// These protected paths changed.
    Protected(Vec<String>),
    /// The hub could not tell, so it holds the piece to be safe. The text is
    /// for the hub's log, not the owner.
    Unchecked(String),
}

impl Hold {
    /// The plain-words reason put in front of the checker's summary, for an
    /// owner who is not a programmer.
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Clear => None,
            Self::Protected(paths) => {
                let mut list = paths
                    .iter()
                    .take(SHOWN)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                if paths.len() > SHOWN {
                    list.push_str(&format!(" and {} more", paths.len() - SHOWN));
                }
                Some(format!(
                    "Held for you: this changes protected parts of the app ({list}). The checker \
passed it, but you asked to look at changes like this yourself."
                ))
            }
            Self::Unchecked(_) => Some(
                "Held for you: the hub could not check whether this changes protected parts of \
the app, so it waits for you to look. The checker passed it."
                    .to_owned(),
            ),
        }
    }

    /// The line for the goal's conversation.
    pub fn note(&self, piece_title: &str) -> Option<String> {
        match self {
            Self::Clear => None,
            Self::Protected(_) => Some(format!(
                "{piece_title} passed the check, but it changes protected parts of the app, so it is held for you to look at."
            )),
            Self::Unchecked(_) => Some(format!(
                "{piece_title} passed the check, but the hub could not check its protected parts, so it is held for you to look at."
            )),
        }
    }
}

/// What the hub needs to know about the work a review job checked.
struct Target {
    thread_id: i64,
    app_id: i64,
    repo_path: String,
    base: Option<String>,
    branch: String,
    checkpoint: Option<i64>,
}

impl Store {
    /// The work a running review job checks, or `None` for any other job.
    async fn review_target(&self, job_id: i64) -> Result<Option<Target>> {
        let row = sqlx::query(
            "SELECT t.id, t.app_id, a.repo_path, t.base_commit, t.branch,
                 (SELECT MAX(c.id) FROM checkpoints c WHERE c.thread_id = t.id) AS checkpoint
             FROM helper_jobs j JOIN threads t ON t.id = j.thread_id JOIN apps a ON a.id = t.app_id
             WHERE j.id = ? AND j.kind = 'review' AND j.state = 'running'",
        )
        .bind(job_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(r) = row else { return Ok(None) };
        let thread_id: i64 = r.try_get("id")?;
        Ok(Some(Target {
            thread_id,
            app_id: r.try_get("app_id")?,
            repo_path: r.try_get("repo_path")?,
            base: r.try_get("base_commit")?,
            // The same name the checker's copy is made from.
            branch: r
                .try_get::<Option<String>, _>("branch")?
                .unwrap_or_else(|| format!("decurion/thread-{thread_id}")),
            checkpoint: r.try_get("checkpoint")?,
        }))
    }
}

/// A branch name safe to put in a refspec.
fn plain_branch(b: &str) -> bool {
    !b.is_empty()
        && !b.starts_with(['-', '/'])
        && !b.ends_with(['/', '.'])
        && !b.contains("..")
        && !b.contains("//")
        && b.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'-' | b'_' | b'.'))
}

impl Files {
    /// The protected list as it was at `base`, or `None` when the app had
    /// none then. Fetches from the owner's checkout if the hub's copy of the
    /// repo does not have `base` yet.
    async fn protected_list(
        &self,
        app_id: i64,
        repo_path: &str,
        base: &str,
    ) -> Result<Option<String>> {
        let _held = self.git_lock.lock().await;
        let cache = self.app_cache(app_id);
        let commit = format!("{base}^{{commit}}");
        if !cache.exists() || git(&cache, &["cat-file", "-e", &commit]).await.is_err() {
            self.fetch_app(&cache, repo_path).await?;
            git(&cache, &["cat-file", "-e", &commit]).await?;
        }
        let entry = git(&cache, &["ls-tree", base, "--", LIST_FILE]).await?;
        if entry.is_empty() {
            return Ok(None);
        }
        // `<mode> <type> <id>\t<path>`. A link or folder in its place would be
        // read as something else, so it counts as unreadable.
        if !matches!(entry.split_whitespace().next(), Some("100644" | "100755")) {
            return Err(StoreError::Invalid(format!(
                "{LIST_FILE} is not a plain file"
            )));
        }
        git(
            &cache,
            &["cat-file", "blob", &format!("{base}:{LIST_FILE}")],
        )
        .await
        .map(Some)
    }

    /// Every path that differs between `base` and the branch in a saved
    /// copy's code part. The tree at the end is compared straight with the
    /// starting commit, not through their merge base, so a branch whose
    /// history was rewritten cannot hide a change.
    async fn changed_paths(
        &self,
        app_id: i64,
        thread: i64,
        checkpoint: i64,
        base: &str,
        branch: &str,
    ) -> Result<Vec<String>> {
        if !plain_branch(branch) {
            return Err(StoreError::Invalid(format!("odd branch name {branch:?}")));
        }
        let _held = self.git_lock.lock().await;
        let cache = self.app_cache(app_id);
        let bundle = self.saved(thread, checkpoint, Part::Code);
        let bundle = bundle.to_string_lossy().into_owned();
        let check_ref = format!("refs/decurion/check/{thread}");
        // Written by an agent: git checks every object it takes in.
        git(
            &cache,
            &[
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.file.allow=always",
                "-c",
                "transfer.fsckObjects=true",
                "fetch",
                "--quiet",
                "--no-tags",
                "--",
                &bundle,
                &format!("+refs/heads/{branch}:{check_ref}"),
            ],
        )
        .await?;
        let out = git(
            &cache,
            &[
                "diff",
                "--name-only",
                "--no-renames",
                "--no-ext-diff",
                "-z",
                base,
                &check_ref,
                "--",
            ],
        )
        .await;
        let _ = git(&cache, &["update-ref", "-d", &check_ref]).await;
        Ok(out?
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect())
    }
}

/// Decides whether review job `job_id`, finishing with `res`, must be held
/// for the owner if it passes. Only a running review job whose answer is a
/// pass is looked at; anything else is [`Hold::Clear`]. Never fails: when the
/// check itself goes wrong, the piece is held.
pub async fn hold_for(store: &Store, files: &Files, job_id: i64, res: &HelperResult) -> Hold {
    let passes = res.ok && last_json_object::<ReviewVerdict>(&res.text).is_ok_and(|v| v.pass);
    if !passes {
        return Hold::Clear;
    }
    match check(store, files, job_id).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(job_id, error = %e, "could not check protected parts; holding the piece");
            Hold::Unchecked(e.to_string())
        }
    }
}

async fn check(store: &Store, files: &Files, job_id: i64) -> Result<Hold> {
    let Some(t) = store.review_target(job_id).await? else {
        return Ok(Hold::Clear);
    };
    let base = t
        .base
        .filter(|b| is_commit(b))
        .ok_or_else(|| StoreError::Invalid("no starting commit is known for this work".into()))?;
    let Some(list) = files.protected_list(t.app_id, &t.repo_path, &base).await? else {
        return Ok(Hold::Clear);
    };
    let checkpoint = t
        .checkpoint
        .ok_or_else(|| StoreError::Invalid("the hub has no saved copy of this work".into()))?;
    let changed = files
        .changed_paths(t.app_id, t.thread_id, checkpoint, &base, &t.branch)
        .await?;
    let hit = protected_changes(&parse(&list), &changed);
    Ok(if hit.is_empty() {
        Hold::Clear
    } else {
        Hold::Protected(hit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn m(list: &str, path: &str) -> bool {
        parse(list).matches(path)
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let p = parse("# the walls\n\n   \n  docker/  \n#src/\n/deny.toml\n///\n");
        assert_eq!(
            p.0,
            vec![vec!["docker".to_owned()], vec!["deny.toml".to_owned()]]
        );
        assert!(parse("").0.is_empty());
    }

    #[test]
    fn patterns_match_the_whole_path_from_the_top() {
        assert!(m("deny.toml", "deny.toml"));
        assert!(!m("deny.toml", "crates/x/deny.toml"));
        assert!(!m("deny.toml", "deny.toml.bak"));
        assert!(m("/deny.toml", "deny.toml"));
        assert!(m("crates/hub/src/spend.rs", "crates/hub/src/spend.rs"));
        assert!(!m("crates/hub/src/spend.rs", "crates/hub/src/spend_rs"));
    }

    #[test]
    fn a_folder_covers_everything_inside_it() {
        assert!(m("docker/", "docker/cloud/walls.sh"));
        assert!(m("docker", "docker/mcp.json"));
        assert!(!m("docker/", "dockerfile"));
        assert!(!m("docker/", "src/docker/x"));
        assert!(m("crates/runner/src/", "crates/runner/src/lib.rs"));
        assert!(!m("crates/runner/src/", "crates/runner/tests/a.rs"));
    }

    #[test]
    fn stars_stay_inside_one_name() {
        assert!(m("src/*.rs", "src/lib.rs"));
        assert!(!m("src/*.rs", "src/a/lib.rs"));
        assert!(
            m("src/*", "src/a/lib.rs"),
            "a starred folder still covers its insides"
        );
        assert!(m("crates/*/src/limits.rs", "crates/hub/src/limits.rs"));
        assert!(!m("crates/*/src/limits.rs", "crates/a/b/src/limits.rs"));
        assert!(m("src/lib.r?", "src/lib.rs"));
        assert!(!m("src/lib.r?", "src/lib.r"));
        assert!(m("*", "anything/at/all"));
    }

    #[test]
    fn double_stars_cross_folders() {
        assert!(m("src/**/secret.rs", "src/secret.rs"));
        assert!(m("src/**/secret.rs", "src/a/b/secret.rs"));
        assert!(!m("src/**/secret.rs", "other/secret.rs"));
        assert!(m("**/walls.sh", "walls.sh"));
        assert!(m("**/walls.sh", "docker/cloud/walls.sh"));
        assert!(m("**", "x"));
    }

    #[test]
    fn letter_case_does_not_get_around_a_pattern() {
        assert!(m("crates/runner/src/", "Crates/Runner/SRC/lib.rs"));
        assert!(m("SECRETS.rs", "secrets.rs"));
    }

    #[test]
    fn exceptions_cannot_unprotect() {
        let p = parse("docker/\n!docker/cloud/walls.sh\n");
        assert!(p.matches("docker/cloud/walls.sh"));
    }

    #[test]
    fn the_list_itself_is_always_protected() {
        let changed = vec![
            "README.md".to_owned(),
            LIST_FILE.to_owned(),
            "docker/a".to_owned(),
        ];
        assert_eq!(
            protected_changes(&parse("docker/"), &changed),
            vec![LIST_FILE.to_owned(), "docker/a".to_owned()]
        );
        assert_eq!(
            protected_changes(&parse(""), &changed),
            vec![LIST_FILE.to_owned()]
        );
        assert!(protected_changes(&parse("docker/"), &["src/a.rs".to_owned()]).is_empty());
    }

    #[test]
    fn the_reason_names_a_few_paths_and_counts_the_rest() {
        let few = Hold::Protected(vec!["a".into(), "b".into()]);
        assert_eq!(
            few.reason().as_deref(),
            Some(
                "Held for you: this changes protected parts of the app (a, b). The checker \
passed it, but you asked to look at changes like this yourself."
            )
        );
        let many = Hold::Protected((1..=8).map(|i| format!("f{i}")).collect());
        let why = many.reason().expect("reason");
        assert!(why.contains("(f1, f2, f3, f4, f5 and 3 more)"), "{why}");
        assert!(!why.contains("f6"));
        let unsure = Hold::Unchecked("git: boom".into());
        let why = unsure.reason().expect("reason");
        assert!(
            why.starts_with("Held for you:") && !why.contains("boom"),
            "{why}"
        );
        assert_eq!(Hold::Clear.reason(), None);
        assert_eq!(Hold::Clear.note("x"), None);
        assert!(few
            .note("Reset link")
            .expect("note")
            .contains("held for you"));
    }

    #[test]
    fn decurions_own_list_names_real_parts() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let text = std::fs::read_to_string(root.join(LIST_FILE)).expect("Decurion's list");
        for line in text.lines().map(str::trim) {
            if !line.is_empty() && !line.starts_with('#') {
                assert!(root.join(line).exists(), "{line} is not in the repo");
            }
        }
        let p = parse(&text);
        assert!(p.matches("crates/decurion-runner/src/lib.rs"));
        assert!(p.matches("docker/cloud/walls.sh"));
        assert!(p.matches(LIST_FILE));
        assert!(p.matches("crates/decurion-hub/src/goals.rs"));
        assert!(!p.matches("crates/decurion-hub/src/store.rs"));
        assert!(!p.matches("README.md"));
    }

    #[test]
    fn odd_branch_names_are_refused() {
        assert!(plain_branch("decurion/thread-7"));
        assert!(!plain_branch("-x"));
        assert!(!plain_branch("a..b"));
        assert!(!plain_branch("a:b"));
        assert!(!plain_branch("a b"));
        assert!(!plain_branch(""));
    }
}
