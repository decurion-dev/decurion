//! Add-ons on the hub (ADR 0001, v1: tools only): fetching one from its
//! link, the owner's one approval, turning it on per app, updates, removal,
//! the add-on log, and handing a run exactly the files the owner approved.
//!
//! What must stay true:
//! - **Owner only.** Every route here is the owner's, except the runner's
//!   download of a running piece of work's add-on files. No agent, runner
//!   key or file in an app's repo can add, widen or turn on an add-on.
//! - **One approval, bound to the bytes shown.** Fetching stages the files
//!   under their tree hash and shows the card; Add sends that hash back,
//!   and the hub hashes the staged files again before keeping them. Nothing
//!   from the add-on runs at fetch or install time: git only copies files.
//! - **Never asks again unless it widens.** An update that asks for nothing
//!   beyond what was approved ([`addon_manifest::new_lines`]) is applied
//!   when the owner checks for it, and logged. Anything new needs one
//!   approval showing only what is new; until then the old version runs.
//! - **Checked before use.** The run's download hashes the kept files
//!   again and refuses them if even one byte changed. The runner checks
//!   once more on its side.
//! - **Everything is written down** in `addon_log`, which is append-only and
//!   outlives the add-on: installs, updates, removals, on and off per app,
//!   each run that used it, each use of a test key (by name), and refusals.
//! - **Each tap acts once.** Add, Update, on or off for an app, and Remove
//!   carry the tap's receipt. The answer is kept in the same transaction as
//!   the change (`addon_answers`), and a resend gets that answer back.
//! - **No two tools of one name.** Whenever an add-on's tools could reach
//!   an app (added for it, turned on, or updated while on), the hub checks
//!   the app has every test key it reads and no other add-on on for it
//!   gives a tool of the same name, which would take the other's calls.
//! - Owner text is plain words; git's own messages are scrubbed of
//!   passwords before they are logged and never shown raw.

use crate::addon_manifest::{self, Grants, Manifest};
use crate::store::{check_receipt, receipt_result, save_receipt, Result, Store, StoreError, OWNER};
use crate::{forbidden, holding, Access, ApiError, AppState, Caller};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use base64::Engine;
use decurion_proto::addon::{self, AddonUse, Bundle, BundleFile, ToolUse};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Row, Sqlite, Transaction};
use std::path::{Path as FsPath, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// How long fetching an add-on may take.
const FETCH_SECS: u64 = 120;
/// The longest manifest read.
const MAX_MANIFEST: u64 = 64 << 10;
/// How many log lines the Add-ons tab shows.
const LOG_LINES: i64 = 30;

const NO_FILES: &str = "This hub keeps no files, so it cannot add add-ons.";
const CHANGED: &str = "This add-on's files changed after you approved them, so Decurion did not use it. Remove it and add it again to check it.";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/addons", get(list))
        .route("/addons/preview", post(preview))
        .route("/addons/approve", post(approve))
        .route("/addons/{id}/update", post(update))
        .route("/addons/{id}/apps/{app}", post(set_app))
        .route("/addons/{id}/remove", post(remove))
        .route("/runner/threads/{id}/addons/{addon}", get(runner_bundle))
}

// ---------- What is kept ----------

/// An installed add-on: exactly the version the owner approved.
#[derive(Debug, Clone)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub source: String,
    pub folder: String,
    pub rev: String,
    pub hash: String,
    pub tools: Vec<ToolUse>,
    pub grants: Grants,
    pub approved_at: String,
}

const COLS: &str =
    "id, name, version, description, source, folder, rev, tree_hash, tools, grants, approved_at";

fn installed_from(row: &sqlx::sqlite::SqliteRow) -> Result<Installed> {
    let tools: String = row.try_get("tools")?;
    let grants: String = row.try_get("grants")?;
    Ok(Installed {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        version: row.try_get("version")?,
        description: row.try_get("description")?,
        source: row.try_get("source")?,
        folder: row.try_get("folder")?,
        rev: row.try_get("rev")?,
        hash: row.try_get("tree_hash")?,
        // Written only by this module, from checked manifests.
        tools: serde_json::from_str(&tools)
            .map_err(|e| StoreError::Invalid(format!("stored add-on tools: {e}")))?,
        grants: serde_json::from_str(&grants)
            .map_err(|e| StoreError::Invalid(format!("stored add-on grants: {e}")))?,
        approved_at: row.try_get("approved_at")?,
    })
}

/// One line of the add-on log.
#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub at: String,
    pub actor: String,
    pub addon_id: String,
    pub event: String,
    pub app_id: Option<i64>,
    pub thread_id: Option<i64>,
    pub detail: String,
}

impl Store {
    pub async fn addons(&self) -> Result<Vec<Installed>> {
        let rows = sqlx::query(&format!("SELECT {COLS} FROM addons ORDER BY name, id"))
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(installed_from).collect()
    }

    pub async fn addon(&self, id: &str) -> Result<Option<Installed>> {
        let row = sqlx::query(&format!("SELECT {COLS} FROM addons WHERE id = ?"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(installed_from).transpose()
    }

    /// The apps an add-on is on for.
    pub async fn addon_apps(&self, id: &str) -> Result<Vec<i64>> {
        Ok(
            sqlx::query_scalar("SELECT app_id FROM addon_apps WHERE addon_id = ? ORDER BY app_id")
                .bind(id)
                .fetch_all(self.pool())
                .await?,
        )
    }

    /// The add-ons on for an app, for a claim: what the runner fetches,
    /// checks and starts. Never a key's value.
    pub async fn claim_addons(&self, app_id: i64) -> Result<Vec<AddonUse>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM addons WHERE id IN
                 (SELECT addon_id FROM addon_apps WHERE app_id = ?)
             ORDER BY id"
        ))
        .bind(app_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                let a = installed_from(r)?;
                Ok(AddonUse {
                    id: a.id,
                    name: a.name,
                    version: a.version,
                    hash: a.hash,
                    tools: a.tools,
                })
            })
            .collect()
    }

    pub async fn addon_log(&self, limit: i64) -> Result<Vec<LogLine>> {
        let rows = sqlx::query(
            "SELECT at, actor, addon_id, event, app_id, thread_id, detail
             FROM addon_log ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(LogLine {
                    at: r.try_get("at")?,
                    actor: r.try_get("actor")?,
                    addon_id: r.try_get("addon_id")?,
                    event: r.try_get("event")?,
                    app_id: r.try_get("app_id")?,
                    thread_id: r.try_get("thread_id")?,
                    detail: r.try_get("detail")?,
                })
            })
            .collect()
    }

    pub(crate) async fn log_addon(
        &self,
        actor: &str,
        addon_id: &str,
        event: &str,
        app_id: Option<i64>,
        thread_id: Option<i64>,
        detail: &str,
    ) -> Result<()> {
        let mut conn = self.pool().acquire().await?;
        log(&mut conn, actor, addon_id, event, app_id, thread_id, detail).await
    }

    /// The answer a tap with this receipt already gave, if it went through.
    async fn addon_answer(&self, tap: &Tap<'_>) -> Result<Option<Value>> {
        check_receipt(tap.receipt)?;
        if tap.receipt.is_none() {
            return Ok(None);
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let a = answered(&mut tx, tap).await?;
        tx.commit().await?;
        Ok(a)
    }

    /// Keeps an approved add-on: a new one on for `apps`, or a new version
    /// of one already kept (its apps stay as they are). Answers `answer`, or
    /// what the tap answered before if its receipt already went through.
    async fn keep_addon(
        &self,
        k: &Keep<'_>,
        apps: &[i64],
        tap: &Tap<'_>,
        answer: Value,
    ) -> Result<Value> {
        let m = k.manifest;
        let tools: Vec<ToolUse> = m.tools.iter().map(|t| t.run.clone()).collect();
        let tools = serde_json::to_string(&tools).expect("tools always serialize");
        let grants = serde_json::to_string(&m.grants()).expect("grants always serialize");
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(first) = answered(&mut tx, tap).await? {
            return Ok(first);
        }
        let before: Option<String> = sqlx::query_scalar("SELECT version FROM addons WHERE id = ?")
            .bind(&m.id)
            .fetch_optional(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO addons (id, name, version, description, source, folder, rev, tree_hash,
                 tools, grants, approved_by)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET name = excluded.name, version = excluded.version,
                 description = excluded.description, rev = excluded.rev,
                 tree_hash = excluded.tree_hash, tools = excluded.tools,
                 grants = excluded.grants, approved_by = excluded.approved_by,
                 approved_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(&m.id)
        .bind(&m.name)
        .bind(&m.version)
        .bind(&m.description)
        .bind(&k.meta.source)
        .bind(&k.meta.folder)
        .bind(&k.meta.rev)
        .bind(k.hash)
        .bind(&tools)
        .bind(&grants)
        .bind(OWNER)
        .execute(&mut *tx)
        .await?;
        let short = &k.meta.rev[..k.meta.rev.len().min(12)];
        match before {
            None => {
                let detail = format!(
                    "version {} from {} at commit {short}, files {}",
                    m.version,
                    k.meta.where_from(),
                    &k.hash[..12]
                );
                log(&mut tx, OWNER, &m.id, "installed", None, None, &detail).await?;
                for app in apps {
                    sqlx::query(
                        "INSERT OR IGNORE INTO addon_apps (addon_id, app_id) VALUES (?, ?)",
                    )
                    .bind(&m.id)
                    .bind(app)
                    .execute(&mut *tx)
                    .await?;
                    log(&mut tx, OWNER, &m.id, "enabled", Some(*app), None, "").await?;
                }
            }
            Some(old) => {
                let detail = format!(
                    "version {old} to {} at commit {short}, files {}{}",
                    m.version,
                    &k.hash[..12],
                    k.note
                );
                log(&mut tx, k.actor, &m.id, "updated", None, None, &detail).await?;
            }
        }
        save_answer(&mut tx, tap, &answer).await?;
        tx.commit().await?;
        Ok(answer)
    }

    async fn set_addon_app(&self, id: &str, app: i64, on: bool, tap: &Tap<'_>) -> Result<Value> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(first) = answered(&mut tx, tap).await? {
            return Ok(first);
        }
        let changed = if on {
            sqlx::query("INSERT OR IGNORE INTO addon_apps (addon_id, app_id) VALUES (?, ?)")
        } else {
            sqlx::query("DELETE FROM addon_apps WHERE addon_id = ? AND app_id = ?")
        }
        .bind(id)
        .bind(app)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        if changed {
            let event = if on { "enabled" } else { "disabled" };
            log(&mut tx, OWNER, id, event, Some(app), None, "").await?;
        }
        let answer = json!({"id": id, "app_id": app, "on": on});
        save_answer(&mut tx, tap, &answer).await?;
        tx.commit().await?;
        Ok(answer)
    }

    /// Removes an add-on. Answers whether this tap removed it (false when
    /// the tap is a resend of one that already did) and what it answered.
    async fn remove_addon(&self, id: &str, tap: &Tap<'_>) -> Result<(bool, Value)> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(first) = answered(&mut tx, tap).await? {
            return Ok((false, first));
        }
        sqlx::query("DELETE FROM addon_apps WHERE addon_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let gone = sqlx::query("DELETE FROM addons WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if gone == 0 {
            return Err(StoreError::NotFound);
        }
        log(&mut tx, OWNER, id, "removed", None, None, "").await?;
        let answer = json!({"removed": id});
        save_answer(&mut tx, tap, &answer).await?;
        tx.commit().await?;
        Ok((true, answer))
    }
}

/// One owner tap on an add-on: its receipt, if the page sent one, and the
/// kind of tap it is, as `receipts` names it. A receipt used for one kind
/// of tap is refused for another.
struct Tap<'a> {
    receipt: Option<&'a str>,
    action: &'static str,
}

/// What a tap with this receipt answered, if it already went through.
async fn answered(tx: &mut Transaction<'_, Sqlite>, tap: &Tap<'_>) -> Result<Option<Value>> {
    let Some(id) = receipt_result(tx, tap.receipt, tap.action).await? else {
        return Ok(None);
    };
    let text: String = sqlx::query_scalar("SELECT answer FROM addon_answers WHERE id = ?")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    // Written only by `save_answer`, from a JSON value.
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| StoreError::Invalid(format!("stored add-on answer: {e}")))
}

/// Keeps a tap's answer and its receipt, in the transaction that acted.
async fn save_answer(
    tx: &mut Transaction<'_, Sqlite>,
    tap: &Tap<'_>,
    answer: &Value,
) -> Result<()> {
    if tap.receipt.is_none() {
        return Ok(());
    }
    let id: i64 = sqlx::query_scalar("INSERT INTO addon_answers (answer) VALUES (?) RETURNING id")
        .bind(answer.to_string())
        .fetch_one(&mut **tx)
        .await?;
    save_receipt(tx, tap.receipt, tap.action, id).await
}

async fn log(
    conn: &mut sqlx::SqliteConnection,
    actor: &str,
    addon_id: &str,
    event: &str,
    app_id: Option<i64>,
    thread_id: Option<i64>,
    detail: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO addon_log (actor, addon_id, event, app_id, thread_id, detail)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(actor)
    .bind(addon_id)
    .bind(event)
    .bind(app_id)
    .bind(thread_id)
    .bind(detail)
    .execute(conn)
    .await?;
    Ok(())
}

// ---------- Fetching and staging ----------

/// Where a staged add-on came from, kept beside its files.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Meta {
    source: String,
    folder: String,
    rev: String,
}

impl Meta {
    fn where_from(&self) -> String {
        if self.folder.is_empty() {
            self.source.clone()
        } else {
            format!("{} (folder {})", self.source, self.folder)
        }
    }
}

/// What approving keeps.
struct Keep<'a> {
    manifest: &'a Manifest,
    hash: &'a str,
    meta: &'a Meta,
    actor: &'a str,
    note: &'a str,
}

/// An add-on fetched and staged, waiting for the owner.
struct Staged {
    manifest: Manifest,
    hash: String,
    meta: Meta,
}

fn staged_dir(root: &FsPath, hash: &str) -> PathBuf {
    root.join("staged").join(hash)
}

fn staged_meta(root: &FsPath, hash: &str) -> PathBuf {
    root.join("staged").join(format!("{hash}.json"))
}

fn kept_dir(root: &FsPath, id: &str, hash: &str) -> PathBuf {
    root.join(id).join(hash)
}

/// The link the owner pasted: an https git link, or the full path of a git
/// folder on this PC (for add-on authors). Never a link with a password in
/// it, and nothing git could read as an option.
fn check_source(s: &str) -> std::result::Result<(), String> {
    const HOW: &str = "Paste the add-on's https link, like https://github.com/example/words, or the full path to its folder on this PC.";
    if s.is_empty() || s.len() > 500 || s.starts_with('-') || s.chars().any(char::is_control) {
        return Err(HOW.into());
    }
    if let Some(rest) = s.strip_prefix("https://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        if host.contains('@') {
            return Err("Leave any password or token out of the link. Add-ons are fetched without signing in.".into());
        }
        if host.is_empty() || s.contains(char::is_whitespace) {
            return Err(HOW.into());
        }
        return Ok(());
    }
    if s.starts_with("http://") {
        return Err("Use the add-on's https link, not http.".into());
    }
    if FsPath::new(s).is_absolute() {
        return Ok(());
    }
    Err(HOW.into())
}

/// git that never asks anything, runs no hooks, keeps the hub's keys out
/// of its surroundings, and speaks only https or local files.
///
/// None of the owner's own git settings apply: not the PC's (system) or
/// their user one (global), and no `GIT_...` variable from the hub's
/// surroundings. Those can hold a credential helper such as Git Credential
/// Manager (which would hand the add-on's server the owner's GitHub
/// sign-in), extra headers with a token, a proxy or an `include` of more
/// settings. An add-on is always fetched as nobody.
fn quiet_git() -> Command {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut cmd = Command::new("git");
    for (k, _) in std::env::vars_os() {
        let name = k.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("GIT_")
            || name.starts_with("DECURION_")
            || name.starts_with("ANTHROPIC_")
            || matches!(
                name.as_str(),
                "CLAUDE_CODE_OAUTH_TOKEN"
                    | "OPENAI_API_KEY"
                    | "XAI_API_KEY"
                    | "GH_TOKEN"
                    | "GITHUB_TOKEN"
            )
        {
            cmd.env_remove(&k);
        }
    }
    cmd.args([
        "-c",
        &format!("core.hooksPath={null}"),
        "-c",
        "core.fsmonitor=false",
        // The same bytes on every computer: no line-ending changes.
        "-c",
        "core.autocrlf=false",
        // No helper may answer a sign-in question, even one set elsewhere.
        "-c",
        "credential.helper=",
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.https.allow=always",
        "-c",
        "protocol.file.allow=always",
    ]);
    // Without the PC's git settings, Git for Windows would not know where
    // its own list of trusted certificates is; Windows' own list always is.
    if cfg!(windows) {
        cmd.args(["-c", "http.sslBackend=schannel"]);
    }
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    cmd
}

/// Runs git and answers with exactly what it printed, or its scrubbed
/// error. `input`, if any, is written to git's stdin.
async fn git_bytes<S: AsRef<std::ffi::OsStr>>(
    args: &[S],
    input: Option<Vec<u8>>,
) -> std::result::Result<Vec<u8>, String> {
    let mut cmd = quiet_git();
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let run = async {
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("could not start git: {e}"))?;
        // Written beside the read, so a full pipe either way never stalls.
        let feed = match (input, child.stdin.take()) {
            (Some(data), Some(mut stdin)) => Some(tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(&data).await;
            })),
            _ => None,
        };
        let out = child
            .wait_with_output()
            .await
            .map_err(|e| format!("could not run git: {e}"))?;
        if let Some(f) = feed {
            let _ = f.await;
        }
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(crate::github::scrub(
                String::from_utf8_lossy(&out.stderr).trim(),
            ))
        }
    };
    match tokio::time::timeout(Duration::from_secs(FETCH_SECS), run).await {
        Err(_) => Err("git took too long".into()),
        Ok(r) => r,
    }
}

async fn run_git<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> std::result::Result<String, String> {
    let out = git_bytes(args, None).await?;
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

fn odd_name(rel: &str) -> String {
    format!(
        "The add-on has a file named {rel:?}. Names may use letters, digits and . _ - + @ only."
    )
}

fn a_link(rel: &str) -> String {
    format!("The add-on's {rel} is a link to somewhere else. Add-ons cannot hold links.")
}

fn too_big() -> String {
    format!(
        "The add-on is too big. Each file can be at most {} MB and all of them {} MB.",
        addon::MAX_FILE_BYTES >> 20,
        addon::MAX_TOTAL_BYTES >> 20
    )
}

fn too_many() -> String {
    format!("The add-on has more than {} files.", addon::MAX_FILES)
}

/// One file of the add-on as git lists it, before any of it is read.
#[derive(Debug, PartialEq)]
struct TreeFile {
    path: String,
    oid: String,
    size: u64,
}

/// Checks what `git ls-tree -r -l -z` lists for the add-on, before a single
/// file is read or written: plain files only (no links, no other repos
/// inside it), names [`addon::check_path`] allows, no `.git` folder, no two
/// names that are one file on Windows, and within the size and file-count
/// limits. git's sizes come from the pack's own headers, so a tiny download
/// that would unpack into something huge is refused here, not after it
/// filled the PC's disk.
fn check_listing(out: &[u8]) -> std::result::Result<Vec<TreeFile>, String> {
    let mut files: Vec<TreeFile> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut total: u64 = 0;
    let unreadable = || "could not read the add-on's list of files".to_owned();
    for entry in out.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let tab = entry
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(unreadable)?;
        let head = std::str::from_utf8(&entry[..tab]).map_err(|_| unreadable())?;
        let rel = String::from_utf8_lossy(&entry[tab + 1..]).into_owned();
        let mut parts = head.split_ascii_whitespace();
        let (Some(mode), Some(kind), Some(oid), Some(size)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(unreadable());
        };
        match mode {
            "120000" => return Err(a_link(&rel)),
            "160000" => {
                return Err(format!(
                    "The add-on's {rel} is another repo inside it. Add-ons cannot hold those."
                ))
            }
            "100644" | "100755" | "100664" if kind == "blob" => {}
            _ => return Err(format!("The add-on's {rel} is not a plain file.")),
        }
        if std::str::from_utf8(&entry[tab + 1..]).is_err()
            || !addon::check_path(&rel)
            || rel.split('/').any(|seg| seg.eq_ignore_ascii_case(".git"))
        {
            return Err(odd_name(&rel));
        }
        if !seen.insert(rel.to_ascii_lowercase()) {
            return Err(format!(
                "The add-on has two files named {rel:?} that differ only in capital letters. Windows keeps them as one file."
            ));
        }
        if !(oid.len() == 40 || oid.len() == 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(unreadable());
        }
        let size: u64 = size.parse().map_err(|_| unreadable())?;
        total = total.saturating_add(size);
        if size > addon::MAX_FILE_BYTES || total > addon::MAX_TOTAL_BYTES {
            return Err(too_big());
        }
        files.push(TreeFile {
            path: rel,
            oid: oid.to_owned(),
            size,
        });
        if files.len() > addon::MAX_FILES {
            return Err(too_many());
        }
    }
    Ok(files)
}

/// Splits what `git cat-file --batch` printed for `listed` (asked in that
/// order) into each file's bytes, checking each is the blob and the size
/// the list said.
fn split_blobs(
    listed: Vec<TreeFile>,
    out: &[u8],
) -> std::result::Result<Vec<(String, Vec<u8>)>, String> {
    let bad = || "could not read the add-on's files".to_owned();
    let mut rest = out;
    let mut files = Vec::with_capacity(listed.len());
    for l in listed {
        let nl = rest.iter().position(|b| *b == b'\n').ok_or_else(bad)?;
        let head = std::str::from_utf8(&rest[..nl]).map_err(|_| bad())?;
        let want = format!("{} blob {}", l.oid, l.size);
        let size = usize::try_from(l.size).map_err(|_| bad())?;
        if head != want || rest.len() < nl + 1 + size + 1 || rest[nl + 1 + size] != b'\n' {
            return Err(bad());
        }
        files.push((l.path, rest[nl + 1..nl + 1 + size].to_vec()));
        rest = &rest[nl + 1 + size + 1..];
    }
    if !rest.is_empty() {
        return Err(bad());
    }
    Ok(files)
}

/// Every file under `root` (leaving out a `.git` folder at its top), by
/// its path inside the add-on. Links, odd names and anything too big are
/// refused, in plain words.
fn read_tree(root: &FsPath) -> std::result::Result<Vec<(String, Vec<u8>)>, String> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;
    let mut stack = vec![(root.to_path_buf(), String::new())];
    let io = |e: std::io::Error| format!("could not read the add-on's files: {e}");
    while let Some((dir, prefix)) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(io)? {
            let e = entry.map_err(io)?;
            let name = e.file_name();
            let name = name.to_string_lossy();
            if prefix.is_empty() && name == ".git" {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            if !addon::check_path(&rel) {
                return Err(odd_name(&rel));
            }
            let ft = e.file_type().map_err(io)?;
            if ft.is_symlink() {
                return Err(a_link(&rel));
            }
            if ft.is_dir() {
                stack.push((e.path(), rel));
                continue;
            }
            if !ft.is_file() {
                return Err(format!("The add-on's {rel} is not a plain file."));
            }
            let len = e.metadata().map_err(io)?.len();
            total += len;
            if len > addon::MAX_FILE_BYTES || total > addon::MAX_TOTAL_BYTES {
                return Err(too_big());
            }
            out.push((rel, std::fs::read(e.path()).map_err(io)?));
            if out.len() > addon::MAX_FILES {
                return Err(too_many());
            }
        }
    }
    Ok(out)
}

fn hash_of(files: &[(String, Vec<u8>)]) -> String {
    addon::tree_hash(files.iter().map(|(p, d)| (p.as_str(), d.as_slice())))
}

fn manifest_of(files: &[(String, Vec<u8>)]) -> std::result::Result<Manifest, String> {
    let (_, text) = files
        .iter()
        .find(|(p, _)| p == addon::MANIFEST)
        .ok_or_else(|| {
            format!(
                "There is no {} at the top of the add-on. Check the link, and the folder if the add-on is in one.",
                addon::MANIFEST
            )
        })?;
    if text.len() as u64 > MAX_MANIFEST {
        return Err(format!("{} is too long.", addon::MANIFEST));
    }
    let text =
        std::str::from_utf8(text).map_err(|_| format!("{} is not plain text.", addon::MANIFEST))?;
    addon_manifest::parse(text)
}

fn write_tree(dest: &FsPath, files: &[(String, Vec<u8>)]) -> std::io::Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    for (rel, data) in files {
        let p = dest.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, data)?;
    }
    Ok(())
}

/// Fetches the add-on at `source` (its `folder`, if it is in one), pinned
/// to the commit it finds, and stages its files under their tree hash.
async fn fetch(root: &FsPath, source: &str, folder: &str) -> std::result::Result<Staged, String> {
    check_source(source)?;
    if !folder.is_empty() && !addon::check_path(folder) {
        return Err(
            "The folder must be a plain path inside the add-on's repo, like addons/words.".into(),
        );
    }
    forget_old(root).await;
    let tmp = root
        .join("tmp")
        .join(uuid::Uuid::new_v4().simple().to_string());
    std::fs::create_dir_all(&tmp)
        .map_err(|e| format!("could not make a folder to fetch into: {e}"))?;
    let repo = tmp.join("repo");
    let r = async {
        // No checkout: git only keeps the packed download until the list of
        // files below has been checked, so nothing is unpacked onto the disk
        // before Decurion knows how big it is.
        let clone: Vec<&std::ffi::OsStr> = [
            "clone",
            "--quiet",
            "--no-checkout",
            "--depth",
            "1",
            "--no-tags",
            "--single-branch",
            "--",
        ]
        .iter()
        .map(std::ffi::OsStr::new)
        .chain([std::ffi::OsStr::new(source), repo.as_os_str()])
        .collect();
        run_git(&clone).await.map_err(|e| {
            tracing::warn!(err = %e, "could not fetch an add-on");
            "Decurion could not fetch the add-on from that link. Check the link works in your browser, then try again.".to_owned()
        })?;
        let in_repo = |rest: &[&str]| -> Vec<std::ffi::OsString> {
            let mut a = vec!["-C".into(), repo.clone().into_os_string()];
            a.extend(rest.iter().map(std::ffi::OsString::from));
            a
        };
        let rev = run_git(&in_repo(&["rev-parse", "--verify", "HEAD^{commit}"]))
            .await
            .map_err(|e| format!("could not read the add-on's commit: {e}"))?;
        if !(rev.len() == 40 || rev.len() == 64) || !rev.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("could not read the add-on's commit".to_owned());
        }
        // The folder is looked up inside the commit, never on the disk, so a
        // folder that is a link in the repo is simply not a folder.
        let tree = if folder.is_empty() {
            format!("{rev}^{{tree}}")
        } else {
            format!("{rev}:{folder}")
        };
        let kind = run_git(&in_repo(&["cat-file", "-t", &tree])).await;
        if kind.as_deref() != Ok("tree") {
            return Err(format!("The add-on's repo has no folder {folder}."));
        }
        let listing = git_bytes(&in_repo(&["ls-tree", "-r", "-l", "-z", &tree]), None)
            .await
            .map_err(|e| format!("could not read the add-on's list of files: {e}"))?;
        let listed = check_listing(&listing)?;
        let ask: Vec<u8> = listed
            .iter()
            .flat_map(|l| format!("{}\n", l.oid).into_bytes())
            .collect();
        let blobs = git_bytes(&in_repo(&["cat-file", "--batch"]), Some(ask))
            .await
        .map_err(|e| format!("could not read the add-on's files: {e}"))?;
        let files = split_blobs(listed, &blobs)?;
        let manifest = manifest_of(&files)?;
        let hash = hash_of(&files);
        let meta = Meta {
            source: source.to_owned(),
            folder: folder.to_owned(),
            rev,
        };
        let dest = staged_dir(root, &hash);
        let meta_path = staged_meta(root, &hash);
        let meta_json = serde_json::to_vec(&meta).expect("meta always serializes");
        tokio::task::spawn_blocking(move || {
            write_tree(&dest, &files)?;
            crate::write_atomic(&meta_path, &meta_json)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("could not keep the add-on's files: {e}"))?;
        Ok(Staged {
            manifest,
            hash,
            meta,
        })
    }
    .await;
    let _ = std::fs::remove_dir_all(&tmp);
    r
}

/// Forgets add-ons fetched more than a day ago and never added, and
/// fetches a hub stopped in the middle of.
async fn forget_old(root: &FsPath) {
    let dirs = [root.join("staged"), root.join("tmp")];
    let _ = tokio::task::spawn_blocking(move || {
        let day = Duration::from_secs(24 * 60 * 60);
        for d in dirs {
            let Ok(list) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in list.flatten() {
                let old = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > day);
                if old {
                    let p = e.path();
                    let _ = if p.is_dir() {
                        std::fs::remove_dir_all(p)
                    } else {
                        std::fs::remove_file(p)
                    };
                }
            }
        }
    })
    .await;
}

/// Reads a staged add-on back, checking its files still hash to `hash`.
async fn staged(root: &FsPath, hash: &str) -> std::result::Result<Staged, String> {
    const GONE: &str =
        "That add-on is no longer waiting to be added. Fetch it again from its link.";
    if !addon::is_hash(hash) {
        return Err(GONE.into());
    }
    let dir = staged_dir(root, hash);
    let meta_path = staged_meta(root, hash);
    let (files, meta) = tokio::task::spawn_blocking(move || {
        let meta = std::fs::read(&meta_path).ok()?;
        Some((read_tree(&dir).ok()?, meta))
    })
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| GONE.to_owned())?;
    if hash_of(&files) != hash {
        return Err(GONE.into());
    }
    let meta: Meta = serde_json::from_slice(&meta).map_err(|_| GONE.to_owned())?;
    Ok(Staged {
        manifest: manifest_of(&files)?,
        hash: hash.to_owned(),
        meta,
    })
}

/// Moves a staged add-on to where kept ones live, beside the version kept
/// now, which [`prune`] removes once the database names the new one.
async fn keep_files(root: &FsPath, s: &Staged) -> std::io::Result<()> {
    let from = staged_dir(root, &s.hash);
    let meta = staged_meta(root, &s.hash);
    let id_dir = root.join(&s.manifest.id);
    let to = kept_dir(root, &s.manifest.id, &s.hash);
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&id_dir)?;
        if to.exists() {
            std::fs::remove_dir_all(&to)?;
        }
        std::fs::rename(&from, &to)?;
        let _ = std::fs::remove_file(meta);
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Removes every kept version of `id` but `hash`.
async fn prune(root: &FsPath, id: &str, hash: &str) {
    let id_dir = root.join(id);
    let hash = hash.to_owned();
    let _ = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        for e in std::fs::read_dir(&id_dir)? {
            let e = e?;
            if e.file_name().to_string_lossy() != hash {
                std::fs::remove_dir_all(e.path())?;
            }
        }
        Ok(())
    })
    .await;
}

// ---------- The approval card ----------

#[derive(Debug, Serialize)]
struct Card {
    hash: String,
    id: String,
    name: String,
    version: String,
    description: String,
    authors: Vec<String>,
    homepage: Option<String>,
    from: String,
    rev: String,
    title: String,
    /// What it may do (for an update: only what is new).
    can: Vec<String>,
    never: &'static str,
    warnings: Vec<String>,
    /// Set for an update: the version kept now.
    update_from: Option<String>,
    /// The test keys it reads, so the page can say which apps lack one.
    keys: Vec<String>,
    apps: Vec<CardApp>,
}

#[derive(Debug, Serialize)]
struct CardApp {
    id: i64,
    name: String,
    /// The test keys this app is missing.
    missing: Vec<String>,
}

async fn card_for(st: &AppState, s: &Staged, kept: Option<&Installed>) -> Result<Card> {
    let m = &s.manifest;
    let keys = m.keys();
    let mut apps = Vec::new();
    for a in st.store.apps().await? {
        let have = st.store.test_key_names(a.id).await?;
        apps.push(CardApp {
            id: a.id,
            name: a.name,
            missing: keys.iter().filter(|k| !have.contains(k)).cloned().collect(),
        });
    }
    let (title, can, update_from) = match kept {
        None => (
            format!("Add \"{}\" (version {})?", m.name, m.version),
            addon_manifest::can_lines(m),
            None,
        ),
        Some(k) => (
            format!("Update \"{}\" to version {}?", m.name, m.version),
            addon_manifest::new_lines(&k.grants, m),
            Some(k.version.clone()),
        ),
    };
    Ok(Card {
        hash: s.hash.clone(),
        id: m.id.clone(),
        name: m.name.clone(),
        version: m.version.clone(),
        description: m.description.clone(),
        authors: m.authors.clone(),
        homepage: m.homepage.clone(),
        from: s.meta.where_from(),
        rev: s.meta.rev.clone(),
        title,
        can,
        never: addon_manifest::NEVER,
        warnings: m.warnings.clone(),
        update_from,
        keys,
        apps,
    })
}

/// A kept add-on with this id, which only an add-on from the same place may
/// update.
fn same_place(kept: &Installed, meta: &Meta) -> std::result::Result<(), String> {
    if kept.source == meta.source && kept.folder == meta.folder {
        Ok(())
    } else {
        Err(format!(
            "An add-on with the id {} is already added, from {}. Remove it first to add this one instead.",
            kept.id,
            Meta {
                source: kept.source.clone(),
                folder: kept.folder.clone(),
                rev: String::new()
            }
            .where_from()
        ))
    }
}

// ---------- Routes ----------

fn refused(text: impl Into<String>) -> Response {
    ApiError(StoreError::Invalid(text.into())).into_response()
}

fn no_files() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, NO_FILES).into_response()
}

fn root_of(st: &AppState) -> Option<PathBuf> {
    st.files.as_ref().map(|f| f.addons_dir())
}

#[derive(Serialize)]
struct Listed {
    id: String,
    name: String,
    version: String,
    description: String,
    from: String,
    rev: String,
    hash: String,
    approved_at: String,
    can: Vec<String>,
    tools: Vec<String>,
    keys: Vec<String>,
    apps: Vec<ListedApp>,
}

#[derive(Serialize)]
struct ListedApp {
    id: i64,
    name: String,
    on: bool,
    missing: Vec<String>,
}

async fn list(State(st): State<AppState>, Extension(c): Extension<Caller>) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing add-ons");
    }
    let r = async {
        let apps = st.store.apps().await?;
        let mut have = Vec::new();
        for a in &apps {
            have.push(st.store.test_key_names(a.id).await?);
        }
        let mut out = Vec::new();
        for a in st.store.addons().await? {
            let on = st.store.addon_apps(&a.id).await?;
            out.push(Listed {
                can: addon_manifest::installed_lines(&a.tools, &a.grants),
                tools: a.tools.iter().map(|t| t.name.clone()).collect(),
                keys: a.grants.keys.clone(),
                apps: apps
                    .iter()
                    .zip(&have)
                    .map(|(x, have)| ListedApp {
                        id: x.id,
                        name: x.name.clone(),
                        on: on.contains(&x.id),
                        missing: a
                            .grants
                            .keys
                            .iter()
                            .filter(|k| !have.contains(k))
                            .cloned()
                            .collect(),
                    })
                    .collect(),
                from: Meta {
                    source: a.source,
                    folder: a.folder,
                    rev: String::new(),
                }
                .where_from(),
                id: a.id,
                name: a.name,
                version: a.version,
                description: a.description,
                rev: a.rev,
                hash: a.hash,
                approved_at: a.approved_at,
            });
        }
        let log = st.store.addon_log(LOG_LINES).await?;
        Ok::<_, StoreError>(json!({ "addons": out, "log": log, "never": addon_manifest::NEVER }))
    }
    .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize)]
struct PreviewBody {
    source: String,
    #[serde(default)]
    folder: String,
}

/// Fetches an add-on and answers with the one card the owner approves.
async fn preview(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Json(b): Json<PreviewBody>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("adding add-ons");
    }
    let Some(root) = root_of(&st) else {
        return no_files();
    };
    let folder = b.folder.trim().trim_matches('/').to_owned();
    let s = match fetch(&root, b.source.trim(), &folder).await {
        Ok(s) => s,
        Err(e) => return refused(e),
    };
    let kept = match st.store.addon(&s.manifest.id).await {
        Ok(k) => k,
        Err(e) => return ApiError(e).into_response(),
    };
    if let Some(k) = &kept {
        if let Err(e) = same_place(k, &s.meta) {
            return ApiError(StoreError::Duplicate(e)).into_response();
        }
        if k.hash == s.hash {
            return ApiError(StoreError::Duplicate(format!(
                "\"{}\" is already added, at this same version.",
                k.name
            )))
            .into_response();
        }
    }
    match card_for(&st, &s, kept.as_ref()).await {
        Ok(card) => Json(card).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize)]
struct ApproveBody {
    hash: String,
    /// For a new add-on: the apps to turn it on for.
    #[serde(default)]
    apps: Vec<i64>,
    #[serde(default)]
    receipt: Option<String>,
}

/// A tap that sends nothing but its receipt.
#[derive(Deserialize)]
struct TapBody {
    #[serde(default)]
    receipt: Option<String>,
}

/// The answer to a resent tap, if this receipt already went through.
async fn answered_before(st: &AppState, tap: &Tap<'_>) -> Option<Response> {
    match st.store.addon_answer(tap).await {
        Ok(Some(v)) => Some(Json(v).into_response()),
        Ok(None) => None,
        Err(e) => Some(ApiError(e).into_response()),
    }
}

/// The owner's Add (or Update) on the card: keeps exactly the files the
/// card was drawn from. Sending it again changes nothing.
async fn approve(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Json(b): Json<ApproveBody>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("adding add-ons");
    }
    let Some(root) = root_of(&st) else {
        return no_files();
    };
    let tap = Tap {
        receipt: b.receipt.as_deref(),
        action: "addon_add",
    };
    if let Some(first) = answered_before(&st, &tap).await {
        return first;
    }
    let s = match staged(&root, &b.hash).await {
        Ok(s) => s,
        Err(e) => {
            // A resend of an Add that already went through, from a page
            // that sent no receipt.
            if let Ok(all) = st.store.addons().await {
                if let Some(k) = all.into_iter().find(|k| k.hash == b.hash) {
                    return Json(json!({"id": k.id, "name": k.name, "version": k.version}))
                        .into_response();
                }
            }
            return ApiError(StoreError::Refused(e)).into_response();
        }
    };
    let kept = match st.store.addon(&s.manifest.id).await {
        Ok(k) => k,
        Err(e) => return ApiError(e).into_response(),
    };
    let mut apps = Vec::new();
    match &kept {
        Some(k) => {
            if let Err(e) = same_place(k, &s.meta) {
                return ApiError(StoreError::Duplicate(e)).into_response();
            }
            // The new version reaches every app the add-on is on for.
            let on = match st.store.addon_apps(&k.id).await {
                Ok(on) => on,
                Err(e) => return ApiError(e).into_response(),
            };
            if let Err(e) = fits_apps(&st, &s.manifest, &on, true).await {
                return e;
            }
        }
        None => {
            for id in b.apps {
                if !apps.contains(&id) {
                    apps.push(id);
                }
            }
            if let Err(e) = fits_apps(&st, &s.manifest, &apps, false).await {
                return e;
            }
        }
    }
    if let Err(e) = keep_files(&root, &s).await {
        tracing::error!(%e, "could not keep an add-on's files");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Decurion could not keep the add-on's files. Try again in a moment.",
        )
            .into_response();
    }
    let keep = Keep {
        manifest: &s.manifest,
        hash: &s.hash,
        meta: &s.meta,
        actor: OWNER,
        note: ", approved by the owner",
    };
    let m = &s.manifest;
    let answer = json!({"id": m.id, "name": m.name, "version": m.version});
    match st.store.keep_addon(&keep, &apps, &tap, answer).await {
        Ok(v) => {
            prune(&root, &m.id, &s.hash).await;
            Json(v).into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}

/// Looks for a newer version at the add-on's own link. One that asks for
/// nothing new is applied now; otherwise the answer is the card listing
/// only what is new, and the version kept now goes on running. Either way
/// it must still fit every app the add-on is on for.
async fn update(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
    Json(b): Json<TapBody>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("updating add-ons");
    }
    let Some(root) = root_of(&st) else {
        return no_files();
    };
    let tap = Tap {
        receipt: b.receipt.as_deref(),
        action: "addon_update",
    };
    if let Some(first) = answered_before(&st, &tap).await {
        return first;
    }
    let kept = match st.store.addon(&id).await {
        Ok(Some(k)) => k,
        Ok(None) => return ApiError(StoreError::NotFound).into_response(),
        Err(e) => return ApiError(e).into_response(),
    };
    let s = match fetch(&root, &kept.source, &kept.folder).await {
        Ok(s) => s,
        Err(e) => return refused(e),
    };
    if s.manifest.id != kept.id {
        return refused(format!(
            "The add-on at that link is now called {}, not {}. Remove this one and add that one instead.",
            s.manifest.id, kept.id
        ));
    }
    if s.hash == kept.hash {
        return Json(json!({"current": true, "version": kept.version})).into_response();
    }
    let on = match st.store.addon_apps(&id).await {
        Ok(on) => on,
        Err(e) => return ApiError(e).into_response(),
    };
    if let Err(e) = fits_apps(&st, &s.manifest, &on, true).await {
        return e;
    }
    if !addon_manifest::new_lines(&kept.grants, &s.manifest).is_empty() {
        return match card_for(&st, &s, Some(&kept)).await {
            Ok(card) => Json(json!({"card": card})).into_response(),
            Err(e) => ApiError(e).into_response(),
        };
    }
    if let Err(e) = keep_files(&root, &s).await {
        tracing::error!(%e, "could not keep an add-on's files");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Decurion could not keep the add-on's files. Try again in a moment.",
        )
            .into_response();
    }
    let keep = Keep {
        manifest: &s.manifest,
        hash: &s.hash,
        meta: &s.meta,
        actor: OWNER,
        note: ", asks for nothing new",
    };
    let answer = json!({"updated": true, "version": s.manifest.version});
    match st.store.keep_addon(&keep, &[], &tap, answer).await {
        Ok(v) => {
            prune(&root, &s.manifest.id, &s.hash).await;
            Json(v).into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}

fn tools_of(m: &Manifest) -> Vec<String> {
    m.tools.iter().map(|t| t.run.name.clone()).collect()
}

/// Checks the add-on `m` may reach each of `apps`: the app has every test
/// key it reads, and its tools clash with no other add-on on for the app
/// and stay within one run's limit. `update` is set when `m` is a new
/// version of an add-on already on for them, so the refusal says the
/// update was not applied.
async fn fits_apps(
    st: &AppState,
    m: &Manifest,
    apps: &[i64],
    update: bool,
) -> std::result::Result<(), Response> {
    let keys = m.keys();
    let tools = tools_of(m);
    for &id in apps {
        let app = st
            .store
            .app(id)
            .await
            .map_err(|e| ApiError(e).into_response())?;
        let have = st
            .store
            .test_key_names(id)
            .await
            .map_err(|e| ApiError(e).into_response())?;
        let why = match keys.iter().find(|k| !have.contains(k)) {
            Some(k) if update => Some(format!(
                "{} has no test key named {k}, which the new version needs. Add it in Settings, Test keys first, or turn \"{}\" off for {}.",
                app.name, m.name, app.name
            )),
            Some(k) => Some(format!(
                "{} has no test key named {k}. Add it in Settings, Test keys first, or leave {} out for now.",
                app.name, app.name
            )),
            None => clash(st, &m.id, &tools, id, &app.name)
                .await
                .map_err(|e| ApiError(e).into_response())?,
        };
        if let Some(why) = why {
            return Err(refused(if update {
                format!(
                    "Decurion did not update \"{}\" to version {}. {why}",
                    m.name, m.version
                )
            } else {
                why
            }));
        }
    }
    Ok(())
}

/// Why add-on `id`'s `tools` cannot be on for `app`, if they cannot: another
/// add-on on for it already gives agents a tool of one of those names (one
/// would take the other's calls), or the run would get too many tools.
async fn clash(
    st: &AppState,
    id: &str,
    tools: &[String],
    app: i64,
    app_name: &str,
) -> Result<Option<String>> {
    let others = st.store.claim_addons(app).await?;
    for o in others.iter().filter(|o| o.id != id) {
        if let Some(t) = o.tools.iter().find(|t| tools.contains(&t.name)) {
            return Ok(Some(format!(
                "\"{}\" already gives {app_name}'s agents a tool named {}. Turn it off for {app_name} first.",
                o.name, t.name
            )));
        }
    }
    let total = others
        .iter()
        .filter(|o| o.id != id)
        .map(|o| o.tools.len())
        .sum::<usize>()
        + tools.len();
    if total > addon::MAX_RUN_TOOLS {
        return Ok(Some(format!(
            "{app_name}'s agents would get more than {} add-on tools. Turn another add-on off for {app_name} first.",
            addon::MAX_RUN_TOOLS
        )));
    }
    Ok(None)
}

#[derive(Deserialize)]
struct OnBody {
    on: bool,
    #[serde(default)]
    receipt: Option<String>,
}

/// Turns an add-on on or off for one app, from the app's next piece of
/// work. Not a new approval: the owner already agreed to what it may do.
async fn set_app(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((id, app)): Path<(String, i64)>,
    Json(b): Json<OnBody>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("turning add-ons on or off");
    }
    let tap = Tap {
        receipt: b.receipt.as_deref(),
        action: "addon_app",
    };
    if let Some(first) = answered_before(&st, &tap).await {
        return first;
    }
    let kept = match st.store.addon(&id).await {
        Ok(Some(k)) => k,
        Ok(None) => return ApiError(StoreError::NotFound).into_response(),
        Err(e) => return ApiError(e).into_response(),
    };
    let a = match st.store.app(app).await {
        Ok(a) => a,
        Err(e) => return ApiError(e).into_response(),
    };
    if b.on {
        let have = match st.store.test_key_names(app).await {
            Ok(h) => h,
            Err(e) => return ApiError(e).into_response(),
        };
        if let Some(k) = kept.grants.keys.iter().find(|k| !have.contains(k)) {
            return refused(format!(
                "{} has no test key named {k}, which \"{}\" needs. Add it in Settings, Test keys first.",
                a.name, kept.name
            ));
        }
        let tools: Vec<String> = kept.tools.iter().map(|t| t.name.clone()).collect();
        match clash(&st, &id, &tools, app, &a.name).await {
            Ok(None) => {}
            Ok(Some(why)) => return refused(why),
            Err(e) => return ApiError(e).into_response(),
        }
    }
    match st.store.set_addon_app(&id, app, b.on, &tap).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// Removes an add-on: no new run uses it. Runs already going finish with
/// the copy they fetched. Its log stays.
async fn remove(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
    Json(b): Json<TapBody>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("removing add-ons");
    }
    let tap = Tap {
        receipt: b.receipt.as_deref(),
        action: "addon_remove",
    };
    if let Some(first) = answered_before(&st, &tap).await {
        return first;
    }
    let (removed, answer) = match st.store.remove_addon(&id, &tap).await {
        Ok(r) => r,
        Err(e) => return ApiError(e).into_response(),
    };
    if removed {
        if let Some(root) = root_of(&st) {
            if addon::check_id(&id) {
                let dir = root.join(&id);
                let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir)).await;
            }
        }
    }
    Json(answer).into_response()
}

/// A running piece of work's add-on files, for its runner only, after the
/// hub checked they still hash to what the owner approved.
async fn runner_bundle(
    State(st): State<AppState>,
    runner: Option<Extension<crate::workers::RunnerId>>,
    Path((thread, id)): Path<(i64, String)>,
) -> Response {
    let (r, files) = match holding(&st, runner, thread, Access::Write).await {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let actor = crate::store::runner_actor(&r.name).unwrap_or_else(|_| "runner".into());
    let result = async {
        let t = st.store.thread(thread).await?;
        let kept = st.store.addon(&id).await?.ok_or(StoreError::NotFound)?;
        if !st.store.addon_apps(&id).await?.contains(&t.app_id) {
            return Err(StoreError::NotFound);
        }
        Ok((t, kept))
    }
    .await;
    let (t, kept) = match result {
        Ok(x) => x,
        Err(e) => return ApiError(e).into_response(),
    };
    let dir = kept_dir(&files.addons_dir(), &kept.id, &kept.hash);
    let read = tokio::task::spawn_blocking(move || read_tree(&dir)).await;
    let files = match read {
        Ok(Ok(f)) if hash_of(&f) == kept.hash => f,
        other => {
            let why = match other {
                Ok(Ok(_)) => "the files no longer match their hash".to_owned(),
                Ok(Err(e)) => e,
                Err(e) => e.to_string(),
            };
            tracing::error!(addon = %kept.id, %why, "refused an add-on's changed files");
            let _ = st
                .store
                .log_addon(
                    &actor,
                    &kept.id,
                    "refused",
                    Some(t.app_id),
                    Some(thread),
                    &why,
                )
                .await;
            return ApiError(StoreError::Refused(CHANGED.into())).into_response();
        }
    };
    let detail = format!("version {}, files {}", kept.version, &kept.hash[..12]);
    if let Err(e) = st
        .store
        .log_addon(
            &actor,
            &kept.id,
            "used",
            Some(t.app_id),
            Some(thread),
            &detail,
        )
        .await
    {
        return ApiError(e).into_response();
    }
    for tool in &kept.tools {
        if let Some(k) = &tool.key {
            let detail = format!("test key {} for its tool {}", k.test_key, tool.name);
            if let Err(e) = st
                .store
                .log_addon(
                    &actor,
                    &kept.id,
                    "key_used",
                    Some(t.app_id),
                    Some(thread),
                    &detail,
                )
                .await
            {
                return ApiError(e).into_response();
            }
        }
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    Json(Bundle {
        id: kept.id,
        version: kept.version,
        hash: kept.hash,
        files: files
            .into_iter()
            .map(|(path, data)| BundleFile {
                path,
                data: b64.encode(data),
            })
            .collect(),
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_links_and_full_paths_are_fetched() {
        assert!(check_source("https://github.com/example/words").is_ok());
        assert!(check_source("https://user:token@github.com/x/y")
            .expect_err("refused")
            .contains("password"));
        assert!(check_source("http://github.com/x/y")
            .expect_err("refused")
            .contains("https"));
        for bad in [
            "",
            "github.com/x/y",
            "--upload-pack=x",
            "ext::sh -c x",
            "git@github.com:x/y",
            "https://a b",
        ] {
            assert!(check_source(bad).is_err(), "{bad}");
        }
        let here = std::env::temp_dir();
        assert!(check_source(&here.to_string_lossy()).is_ok());
    }

    #[test]
    fn a_tree_with_a_link_or_an_odd_name_is_refused() {
        let dir = std::env::temp_dir().join(format!("decurion-addon-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("lib")).expect("dir");
        std::fs::create_dir_all(dir.join(".git")).expect("dir");
        std::fs::write(dir.join(".git").join("HEAD"), "x").expect("write");
        std::fs::write(dir.join("lib").join("a.js"), "a").expect("write");
        std::fs::write(dir.join("b.txt"), "b").expect("write");
        let files = read_tree(&dir).expect("reads");
        let mut names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        names.sort();
        assert_eq!(names, ["b.txt", "lib/a.js"]);
        std::fs::write(dir.join("odd name.txt"), "c").expect("write");
        assert!(read_tree(&dir).expect_err("refused").contains("odd name"));
        std::fs::remove_file(dir.join("odd name.txt")).expect("rm");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", dir.join("lib").join("p")).expect("link");
            assert!(read_tree(&dir).expect_err("refused").contains("link"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn ls(lines: &[(&str, &str, u64, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (mode, kind, size, path) in lines {
            out.extend(format!("{mode} {kind} {OID} {size:>7}\t{path}\0").into_bytes());
        }
        out
    }

    #[test]
    fn the_list_of_files_is_checked_before_any_is_read() {
        let ok = check_listing(&ls(&[
            ("100644", "blob", 3, "decurion-addon.toml"),
            ("100755", "blob", 10, "bin/run.sh"),
        ]))
        .expect("plain files");
        assert_eq!(ok.len(), 2);
        assert_eq!((ok[1].path.as_str(), ok[1].size), ("bin/run.sh", 10));

        let refused =
            |lines: &[(&str, &str, u64, &str)]| check_listing(&ls(lines)).expect_err("refused");
        assert!(refused(&[("120000", "blob", 11, "p")]).contains("is a link"));
        assert!(refused(&[("160000", "commit", 0, "sub")]).contains("another repo"));
        assert!(refused(&[("100644", "blob", 1, "a b")]).contains("named"));
        assert!(refused(&[("100644", "blob", 1, "con.txt")]).contains("named"));
        assert!(refused(&[("100644", "blob", 1, "x/.git/config")]).contains("named"));
        assert!(refused(&[
            ("100644", "blob", 1, "A.txt"),
            ("100644", "blob", 1, "a.txt")
        ])
        .contains("capital letters"));
        let huge = addon::MAX_FILE_BYTES + 1;
        assert!(refused(&[("100644", "blob", huge, "zeros")]).contains("too big"));
        let each = addon::MAX_FILE_BYTES;
        let names: Vec<String> = (0..5).map(|i| format!("f{i}")).collect();
        let many: Vec<(&str, &str, u64, &str)> = names
            .iter()
            .map(|n| ("100644", "blob", each, n.as_str()))
            .collect();
        assert!(refused(&many).contains("too big"));
        let names: Vec<String> = (0..=addon::MAX_FILES).map(|i| format!("f{i}")).collect();
        let many: Vec<(&str, &str, u64, &str)> = names
            .iter()
            .map(|n| ("100644", "blob", 1, n.as_str()))
            .collect();
        assert!(refused(&many).contains("more than"));
    }

    #[test]
    fn blobs_must_be_exactly_what_the_list_said() {
        let list = || {
            vec![
                TreeFile {
                    path: "a".into(),
                    oid: OID.into(),
                    size: 2,
                },
                TreeFile {
                    path: "b".into(),
                    oid: OID.into(),
                    size: 0,
                },
            ]
        };
        let good = format!("{OID} blob 2\nhi\n{OID} blob 0\n\n");
        let files = split_blobs(list(), good.as_bytes()).expect("reads");
        assert_eq!(
            files,
            [
                ("a".to_owned(), b"hi".to_vec()),
                ("b".to_owned(), Vec::new())
            ]
        );
        for bad in [
            format!("{OID} blob 3\nhi!\n{OID} blob 0\n\n"),
            format!("{OID} blob 2\nhi\n"),
            format!("{OID} missing\n"),
            format!("{good}extra"),
        ] {
            assert!(split_blobs(list(), bad.as_bytes()).is_err(), "{bad:?}");
        }
    }
}
