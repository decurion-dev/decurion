//! Files the owner attaches for agents to read (see
//! [`decurion_proto::context`] for what they are and who gets them): storing,
//! listing and removing them, taking the text out of Word, Excel and
//! PowerPoint files, and packing a run's copy as one tar.
//!
//! The files live in the hub's saved-work folder, at
//! `context/app-<app>/<id>`, with `<id>.txt` beside it when the hub made a
//! text version; the `context_files` table names them.
//!
//! Rules that must stay true:
//! - The owner's routes are owner-only. The runner gets a thread's files only
//!   through [`crate::holding`], like the thread's code, and a helper's files
//!   only on the PC: that route is not in [`crate::cloud_reach`], and its
//!   handler refuses cloud workers too.
//! - Two files of an app never clash ([`clashes`]): a second "image.jpg"
//!   becomes "image (2).jpg", chosen in the transaction that adds it.
//! - An upload is checked (app, goal, name, limits) before its body
//!   is read, and again in the transaction that adds it, so two uploads at
//!   once cannot break the limits. The body is streamed to disk and cut off
//!   past [`MAX_FILE_BYTES`]; it is never held in memory.
//! - The hub never runs or opens a file the owner attached. It reads Word,
//!   Excel and PowerPoint files only as zip archives of XML, with every part
//!   it reads capped ([`MAX_PART_BYTES`], [`MAX_UNPACKED_BYTES`],
//!   [`MAX_PARTS`]) so a crafted file cannot fill memory. A file it cannot
//!   read still uploads, with a note. PDFs are read by the runner, inside a
//!   sealed box, never here.
//! - A file goes back to the owner only as a download (`attachment`,
//!   `nosniff`, a `sandbox` policy), so an attached page or picture can never
//!   run as a page of the hub.
//! - Names in a run's tar are the cleaned names the store holds, so nothing
//!   in it can land outside the folder it is unpacked in.

use crate::checkpoints::Files;
use crate::store::{
    check_receipt, receipt_result, save_receipt, Result, Store, StoreError, RECEIPT_REUSED,
};
use crate::workers::RunnerId;
use crate::{
    created_or_resent, forbidden, holding, not_runner, runner_only, stream_file, Access, ApiError,
    AppState, Caller, RemoveOnDrop,
};
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::RequestExt;
use axum::{Extension, Json};
use decurion_proto::context::{
    clashes, clean_name, text_name, ContextFile, ContextRef, FileKind, MAX_APP_BYTES, MAX_FILES,
    MAX_FILE_BYTES, MAX_NAME, MAX_TEXT_BYTES,
};
use decurion_proto::Place;
use quick_xml::events::{BytesStart, Event};
use quick_xml::XmlVersion;
use serde::Deserialize;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, Transaction};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

/// The most of any one part of a Word, Excel or PowerPoint file the hub
/// unpacks. Real documents are far smaller; a zip bomb is not.
const MAX_PART_BYTES: u64 = 64 << 20;

/// The most the hub unpacks from one file, all parts together.
const MAX_UNPACKED_BYTES: u64 = 256 << 20;

/// The most sheets or slides read from one file.
const MAX_PARTS: usize = 500;

/// A file listing more parts than this is not an office document.
const MAX_ARCHIVE_ENTRIES: usize = 20_000;

/// Leftovers of uploads and run copies older than this are cleared away.
const LEFTOVER_AGE: Duration = Duration::from_secs(3600);

const ADD: &str = "add_context_file";
const REMOVE: &str = "remove_context_file";

const COLS: &str = "id, app_id, goal_id, name, size, kind, text_chars, note, added_at";

const NO_GOAL: &str = "That goal does not exist. Reload the page and try again.";
const OTHER_APP: &str = "That goal belongs to another app. Attach the file to the goal's own app.";
const EMPTY: &str = "This file is empty. Choose another one.";
const NO_FOLDER: &str =
    "This hub has no folder to keep files in, so it cannot take attachments. Restart it with the Decurion start script.";
const OTHER_NOTE: &str = "Agents get this file as it is, but cannot read it. Save it as .docx, .xlsx or .pdf if they need to read it.";

/// What the owner is told when a file is over [`MAX_FILE_BYTES`].
pub fn too_large_words() -> String {
    format!(
        "This file is larger than {} MB. Make it smaller, or split it, and try again.",
        MAX_FILE_BYTES >> 20
    )
}

impl Files {
    fn context_app_dir(&self, app_id: i64) -> PathBuf {
        self.context_dir().join(format!("app-{app_id}"))
    }

    fn context_original(&self, app_id: i64, id: i64) -> PathBuf {
        self.context_app_dir(app_id).join(id.to_string())
    }

    fn context_text(&self, app_id: i64, id: i64) -> PathBuf {
        self.context_app_dir(app_id).join(format!("{id}.txt"))
    }

    /// Where a run's tar is built before it is sent.
    fn context_runs(&self) -> PathBuf {
        self.context_dir().join("runs")
    }
}

fn kind_str(k: FileKind) -> &'static str {
    match k {
        FileKind::Pdf => "pdf",
        FileKind::Word => "word",
        FileKind::Sheet => "sheet",
        FileKind::Slides => "slides",
        FileKind::Image => "image",
        FileKind::Text => "text",
        FileKind::Other => "other",
    }
}

fn kind_from(s: &str) -> Result<FileKind> {
    serde_json::from_value(serde_json::Value::String(s.to_owned()))
        .map_err(|e| StoreError::Invalid(format!("unknown file kind {s}: {e}")))
}

fn file_from(row: &SqliteRow) -> Result<ContextFile> {
    let kind: String = row.try_get("kind")?;
    Ok(ContextFile {
        id: row.try_get("id")?,
        app_id: row.try_get("app_id")?,
        goal_id: row.try_get("goal_id")?,
        name: row.try_get("name")?,
        size: u64::try_from(row.try_get::<i64, _>("size")?).unwrap_or(0),
        kind: kind_from(&kind)?,
        text_chars: row
            .try_get::<Option<i64>, _>("text_chars")?
            .and_then(|n| u64::try_from(n).ok()),
        note: row.try_get("note")?,
        added_at: row.try_get("added_at")?,
    })
}

/// A disk error while keeping a file: logged in full, told to the owner in
/// plain words.
fn disk(e: std::io::Error) -> StoreError {
    tracing::error!(error = %e, "could not keep an attached file");
    StoreError::Refused("Decurion could not save the file on this PC. Try again.".into())
}

/// A file that arrived and is ready to be added: its bytes, and any text
/// version, wait under temporary names until the row is written.
pub(crate) struct Arrived {
    pub app_id: i64,
    pub goal_id: Option<i64>,
    pub name: String,
    pub size: u64,
    pub reading: Reading,
    pub tmp: PathBuf,
    pub tmp_text: Option<PathBuf>,
}

/// A stored file as a run's copy needs it.
struct Stored {
    id: i64,
    app_id: i64,
    name: String,
    size: u64,
    kind: FileKind,
    text: bool,
    for_goal: bool,
}

impl Stored {
    fn as_ref(&self) -> ContextRef {
        ContextRef {
            name: self.name.clone(),
            size: self.size,
            kind: self.kind,
            text: self.text,
            for_goal: self.for_goal,
        }
    }
}

fn stored_from(row: &SqliteRow) -> Result<Stored> {
    let f = file_from(row)?;
    Ok(Stored {
        id: f.id,
        app_id: f.app_id,
        name: f.name,
        size: f.size,
        kind: f.kind,
        text: f.text_chars.is_some(),
        for_goal: f.goal_id.is_some(),
    })
}

/// The checks every upload passes, before its body is read and again when
/// it is added. `size` is left out when it is not known yet. Returns the
/// cleaned name, and the name the file gets: the cleaned one, or one made
/// free with [`free_name`] when another file of the app has it.
async fn check_in(
    tx: &mut Transaction<'_, Sqlite>,
    app_id: i64,
    goal_id: Option<i64>,
    raw_name: &str,
    size: Option<u64>,
) -> Result<(String, String)> {
    let app: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM apps WHERE id = ?)")
        .bind(app_id)
        .fetch_one(&mut **tx)
        .await?;
    if !app {
        return Err(StoreError::NotFound);
    }
    if let Some(g) = goal_id {
        let owner: Option<i64> = sqlx::query_scalar("SELECT app_id FROM goals WHERE id = ?")
            .bind(g)
            .fetch_optional(&mut **tx)
            .await?;
        match owner {
            None => return Err(StoreError::Invalid(NO_GOAL.into())),
            Some(a) if a != app_id => return Err(StoreError::Invalid(OTHER_APP.into())),
            Some(_) => {}
        }
    }
    let name = clean_name(raw_name).map_err(|e| StoreError::Invalid(e.into()))?;
    match size {
        Some(0) => return Err(StoreError::Invalid(EMPTY.into())),
        Some(n) if n > MAX_FILE_BYTES => return Err(StoreError::Invalid(too_large_words())),
        _ => {}
    }
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT name, size FROM context_files WHERE app_id = ? ORDER BY id")
            .bind(app_id)
            .fetch_all(&mut **tx)
            .await?;
    if rows.len() >= MAX_FILES {
        return Err(StoreError::Refused(format!(
            "This app already has {MAX_FILES} files, the most it can hold. Remove one you no longer need first."
        )));
    }
    let used: u64 = rows
        .iter()
        .map(|(_, s)| u64::try_from(*s).unwrap_or(0))
        .sum();
    if used + size.unwrap_or(0) > MAX_APP_BYTES {
        return Err(StoreError::Refused(format!(
            "There is no room for this file: an app's files can add up to {} MB together. Remove some you no longer need first.",
            MAX_APP_BYTES >> 20
        )));
    }
    let taken: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
    let free = free_name(&name, &taken).ok_or_else(|| {
        StoreError::Duplicate(format!(
            "There are already too many files called {name}. Rename yours and try again."
        ))
    })?;
    Ok((name, free))
}

/// A name for `name` that clashes with none of `taken`: itself if it can,
/// else with " (2)", " (3)", ... before the extension, as a computer does
/// when a phone sends a second "image.jpg". The stem is cut to stay within
/// [`MAX_NAME`].
fn free_name(name: &str, taken: &[&str]) -> Option<String> {
    let free = |n: &str| !taken.iter().any(|t| clashes(n, t));
    if free(name) {
        return Some(name.to_owned());
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 10 => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    // Every file blocks at most two names (its own and its text version's).
    for n in 2..=(2 * MAX_FILES + 2) {
        let tag = format!(" ({n})");
        let keep = MAX_NAME
            .saturating_sub(tag.len() + ext.len())
            .min(stem.len());
        let stem = stem[..keep].trim_end_matches(['.', ' ', '-']);
        if stem.is_empty() {
            return None;
        }
        let candidate = format!("{stem}{tag}{ext}");
        if free(&candidate) {
            return Some(candidate);
        }
    }
    None
}

impl Store {
    /// Every file attached to an app, its goals' files included, newest first.
    pub async fn context_files(&self, app_id: i64) -> Result<Vec<ContextFile>> {
        self.app(app_id).await?;
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM context_files WHERE app_id = ? ORDER BY id DESC"
        ))
        .bind(app_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(file_from).collect()
    }

    /// One file of an app.
    pub async fn context_file(&self, app_id: i64, id: i64) -> Result<ContextFile> {
        let row = sqlx::query(&format!(
            "SELECT {COLS} FROM context_files WHERE id = ? AND app_id = ?"
        ))
        .bind(id)
        .bind(app_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::NotFound)?;
        file_from(&row)
    }

    async fn context_file_by_id(&self, id: i64) -> Result<ContextFile> {
        let row = sqlx::query(&format!("SELECT {COLS} FROM context_files WHERE id = ?"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(StoreError::NotFound)?;
        file_from(&row)
    }

    /// Whether an upload may go ahead, before its bytes are read. `size` is
    /// the size the browser announced, if any. Returns the cleaned name; the
    /// name the file finally gets is chosen when it is added.
    pub async fn check_context_file(
        &self,
        app_id: i64,
        goal_id: Option<i64>,
        raw_name: &str,
        size: Option<u64>,
    ) -> Result<String> {
        let mut tx = self.pool().begin().await?; // read-only: a reader never waits on writers
        let (name, _) = check_in(&mut tx, app_id, goal_id, raw_name, size).await?;
        tx.commit().await?;
        Ok(name)
    }

    /// The file a resent upload tap added the first time, if this receipt
    /// was used for one.
    pub async fn resent_context_file(&self, receipt: Option<&str>) -> Result<Option<ContextFile>> {
        check_receipt(receipt)?;
        let mut tx = self.pool().begin().await?; // read-only: a reader never waits on writers
        let done = receipt_result(&mut tx, receipt, ADD).await?;
        tx.commit().await?;
        match done {
            Some(id) => Ok(Some(self.context_file_by_id(id).await?)),
            None => Ok(None),
        }
    }

    /// Adds a file that arrived, checking everything again in the same
    /// transaction as the row, and moves its bytes into place. With a
    /// receipt already used for an upload, nothing is added and the first
    /// file is returned; the flag is false then. The temporary files are
    /// the caller's to clear away whatever happens.
    pub(crate) async fn add_context_file(
        &self,
        files: &Files,
        a: &Arrived,
        receipt: Option<&str>,
    ) -> Result<(ContextFile, bool)> {
        check_receipt(receipt)?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(id) = receipt_result(&mut tx, receipt, ADD).await? {
            tx.commit().await?;
            return Ok((self.context_file_by_id(id).await?, false));
        }
        // The free name is chosen here, in the transaction that takes it.
        let (_, name) = check_in(&mut tx, a.app_id, a.goal_id, &a.name, Some(a.size)).await?;
        let size = i64::try_from(a.size).map_err(|_| StoreError::Invalid(too_large_words()))?;
        let chars = a.reading.chars.and_then(|n| i64::try_from(n).ok());
        let row = sqlx::query(&format!(
            "INSERT INTO context_files (app_id, goal_id, name, size, kind, text_chars, note)
             VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING {COLS}"
        ))
        .bind(a.app_id)
        .bind(a.goal_id)
        .bind(&name)
        .bind(size)
        .bind(kind_str(FileKind::of(&name)))
        .bind(chars.filter(|_| a.tmp_text.is_some()))
        .bind(&a.reading.note)
        .fetch_one(&mut *tx)
        .await?;
        let file = file_from(&row)?;
        let (original, text) = (
            files.context_original(a.app_id, file.id),
            files.context_text(a.app_id, file.id),
        );
        let placed = (|| {
            if let Some(t) = &a.tmp_text {
                std::fs::rename(t, &text)?;
            }
            std::fs::rename(&a.tmp, &original)
        })();
        let saved = match placed {
            Ok(()) => save_receipt(&mut tx, receipt, ADD, file.id).await,
            Err(e) => Err(disk(e)),
        };
        if let Err(e) = saved {
            let _ = std::fs::remove_file(&original);
            let _ = std::fs::remove_file(&text);
            return Err(e);
        }
        if let Err(e) = tx.commit().await {
            let _ = std::fs::remove_file(&original);
            let _ = std::fs::remove_file(&text);
            return Err(e.into());
        }
        Ok((file, true))
    }

    /// Removes a file: its row and both of its copies on disk, in one go. A
    /// resend of the same tap does nothing again.
    pub async fn remove_context_file(
        &self,
        files: &Files,
        app_id: i64,
        id: i64,
        receipt: Option<&str>,
    ) -> Result<()> {
        check_receipt(receipt)?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(done) = receipt_result(&mut tx, receipt, REMOVE).await? {
            if done != id {
                return Err(StoreError::Duplicate(RECEIPT_REUSED.into()));
            }
            tx.commit().await?;
            return Ok(());
        }
        let gone = sqlx::query("DELETE FROM context_files WHERE id = ? AND app_id = ?")
            .bind(id)
            .bind(app_id)
            .execute(&mut *tx)
            .await?;
        if gone.rows_affected() != 1 {
            return Err(StoreError::NotFound);
        }
        for p in [
            files.context_original(app_id, id),
            files.context_text(app_id, id),
        ] {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(disk(e)),
            }
        }
        save_receipt(&mut tx, receipt, REMOVE, id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The files a piece of work gets: the app's own, and those of the goal
    /// it is a piece of (or that a task above it is a piece of). By name.
    async fn stored_for_thread(&self, thread_id: i64) -> Result<Vec<Stored>> {
        let rows = sqlx::query(&format!(
            "WITH RECURSIVE up (id, parent) AS (
                 SELECT id, parent_id FROM threads WHERE id = ?
                 UNION ALL
                 SELECT t.id, t.parent_id FROM threads t JOIN up ON t.id = up.parent
             )
             SELECT {COLS} FROM context_files
             WHERE app_id = (SELECT app_id FROM threads WHERE id = ?)
               AND (goal_id IS NULL OR goal_id IN (
                   SELECT p.goal_id FROM goal_pieces p JOIN up ON p.thread_id = up.id))
             ORDER BY name, id"
        ))
        .bind(thread_id)
        .bind(thread_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(stored_from).collect()
    }

    /// The files a goal's helpers get: the app's own and the goal's. By name.
    async fn stored_for_goal(&self, goal_id: i64) -> Result<Vec<Stored>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM context_files
             WHERE app_id = (SELECT app_id FROM goals WHERE id = ?)
               AND (goal_id IS NULL OR goal_id = ?)
             ORDER BY name, id"
        ))
        .bind(goal_id)
        .bind(goal_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(stored_from).collect()
    }

    /// What a claim for a piece of work names; see [`decurion_proto::Claim::context`].
    pub async fn thread_context(&self, thread_id: i64) -> Result<Vec<ContextRef>> {
        Ok(self
            .stored_for_thread(thread_id)
            .await?
            .iter()
            .map(Stored::as_ref)
            .collect())
    }

    /// What a helper claim for a goal names.
    pub async fn goal_context(&self, goal_id: i64) -> Result<Vec<ContextRef>> {
        Ok(self
            .stored_for_goal(goal_id)
            .await?
            .iter()
            .map(Stored::as_ref)
            .collect())
    }

    /// The goal of a helper job running on `runner`.
    async fn running_helper_goal(&self, job_id: i64, runner: &str) -> Result<i64> {
        let actor = crate::store::runner_actor(runner)?;
        let row: Option<(i64, String, Option<String>)> =
            sqlx::query_as("SELECT goal_id, state, runner FROM helper_jobs WHERE id = ?")
                .bind(job_id)
                .fetch_optional(self.pool())
                .await?;
        match row {
            None => Err(StoreError::NotFound),
            Some((goal, state, holder)) if state == "running" && holder == Some(actor) => Ok(goal),
            Some(_) => Err(StoreError::Refused(
                "that helper job is not running on this runner".into(),
            )),
        }
    }
}

// ---------- Text versions ----------

/// What the hub could take out of a file for agents to read.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Reading {
    pub text: Option<String>,
    /// Characters in `text`.
    pub chars: Option<u64>,
    /// Plain words for the owner; see [`ContextFile::note`].
    pub note: String,
}

impl Reading {
    fn noted(note: impl Into<String>) -> Self {
        Self {
            note: note.into(),
            ..Self::default()
        }
    }
}

/// Why the text could not be taken out of a file. Never shown as it is.
#[derive(Debug, thiserror::Error)]
enum Unreadable {
    #[error("{0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Shape(&'static str),
}

/// What the owner calls this kind of file.
fn kind_words(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Word => "Word file",
        FileKind::Sheet => "spreadsheet",
        FileKind::Slides => "slide deck",
        _ => "file",
    }
}

fn unreadable_note(kind: FileKind) -> String {
    format!(
        "Decurion could not read the text in this {}, so agents get the file as it is. If it has a password, remove it and attach it again.",
        kind_words(kind)
    )
}

/// Makes the text version of a file, away from the async threads. A panic
/// or an error becomes a note; the upload goes ahead either way.
pub(crate) async fn read_in_background(kind: FileKind, name: String, path: PathBuf) -> Reading {
    match tokio::task::spawn_blocking(move || read_text(kind, &name, &path)).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "reading an attached file failed");
            Reading::noted(unreadable_note(kind))
        }
    }
}

/// The text version of the file at `path`, called `name`, if the hub makes
/// one for its kind; see the module rules.
pub(crate) fn read_text(kind: FileKind, name: &str, path: &std::path::Path) -> Reading {
    let xlsx = name.to_ascii_lowercase().ends_with(".xlsx");
    let pulled = match kind {
        FileKind::Word => office(path, docx_text),
        FileKind::Sheet if xlsx => office(path, xlsx_text),
        FileKind::Slides => office(path, pptx_text),
        FileKind::Other if looks_like_text(path) => return Reading::default(),
        FileKind::Other => return Reading::noted(OTHER_NOTE),
        // Already text, a picture, or a PDF the runner reads in a box.
        _ => return Reading::default(),
    };
    match pulled {
        Ok(out) => finish_text(kind, out),
        Err(e) => {
            tracing::info!(error = %e, kind = kind_str(kind), "could not read an attached file");
            Reading::noted(unreadable_note(kind))
        }
    }
}

/// Whether a file of no known kind is plain text anyway (a `.py` or `.ini`
/// file): its start is UTF-8 with no zero bytes.
fn looks_like_text(path: &std::path::Path) -> bool {
    let mut head = Vec::new();
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    if f.take(8 << 10).read_to_end(&mut head).is_err() || head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(&head) {
        Ok(_) => true,
        // Only a character cut off at the end of the sample.
        Err(e) => e.error_len().is_none(),
    }
}

/// Tidies extracted text (no trailing spaces, at most one empty line in a
/// row) and says what the owner should know about it.
fn finish_text(kind: FileKind, out: Out) -> Reading {
    let mut text = String::with_capacity(out.s.len());
    let mut blank = 0;
    for line in out.s.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank += 1;
            if blank > 1 || text.is_empty() {
                continue;
            }
        } else {
            blank = 0;
        }
        text.push_str(line);
        text.push('\n');
    }
    let text = text.trim_end().to_owned();
    if text.is_empty() {
        return Reading::noted(format!(
            "Decurion found no text in this {}. Agents get the file as it is.",
            kind_words(kind)
        ));
    }
    let note = if out.cut {
        format!(
            "This file has a lot of text: agents get the first {} MB of it as text, and the whole file as it is.",
            MAX_TEXT_BYTES >> 20
        )
    } else {
        String::new()
    };
    Reading {
        chars: Some(text.chars().count() as u64),
        text: Some(text),
        note,
    }
}

/// Text being gathered, cut at [`MAX_TEXT_BYTES`] on a character boundary.
#[derive(Default)]
struct Out {
    s: String,
    cut: bool,
}

impl Out {
    fn push(&mut self, t: &str) {
        if self.cut {
            return;
        }
        let room = MAX_TEXT_BYTES - self.s.len();
        if t.len() <= room {
            self.s.push_str(t);
            return;
        }
        let mut n = room;
        while !t.is_char_boundary(n) {
            n -= 1;
        }
        self.s.push_str(&t[..n]);
        self.cut = true;
    }

    fn full(&self) -> bool {
        self.cut
    }
}

/// An office file opened as the zip archive it is, unpacking through caps.
struct Parts<R: Read + std::io::Seek> {
    zip: zip::ZipArchive<R>,
    unpacked: u64,
}

impl<R: Read + std::io::Seek> Parts<R> {
    fn names(&self) -> Vec<String> {
        self.zip
            .file_names()
            .filter_map(|n| n.ok().map(|n| n.into_owned()))
            .collect()
    }

    /// One part as text, or nothing if the file has no such part. A part
    /// over the caps is cut there; its XML then just ends early.
    fn read(&mut self, name: &str) -> std::result::Result<Option<String>, Unreadable> {
        let Some(i) = self.zip.index_for_name(name) else {
            return Ok(None);
        };
        let budget = MAX_PART_BYTES.min(MAX_UNPACKED_BYTES.saturating_sub(self.unpacked));
        if budget == 0 {
            return Err(Unreadable::Shape("too much to unpack"));
        }
        let mut buf = Vec::new();
        self.zip.by_index(i)?.take(budget).read_to_end(&mut buf)?;
        self.unpacked += buf.len() as u64;
        Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
    }
}

type Pull =
    fn(&mut Parts<std::io::BufReader<std::fs::File>>) -> std::result::Result<Out, Unreadable>;

fn office(path: &std::path::Path, pull: Pull) -> std::result::Result<Out, Unreadable> {
    let f = std::io::BufReader::new(std::fs::File::open(path)?);
    let zip = zip::ZipArchive::new(f)?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(Unreadable::Shape("too many parts"));
    }
    pull(&mut Parts { zip, unpacked: 0 })
}

/// One step through an XML part, with names already stripped of their
/// prefix. An empty element (`<w:tab/>`) comes as a start and an end.
enum Tag<'a> {
    Start(&'a BytesStart<'a>),
    End(&'a str),
    Text(&'a str),
}

fn local<'a>(e: &'a BytesStart<'a>) -> &'a str {
    e.local_name().into_inner()
}

/// An attribute's value by its name without prefix.
fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .find(|a| a.key.local_name().into_inner() == name)
        .and_then(|a| {
            a.normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

/// Walks an XML part, calling `f` for each step until it returns false. A
/// part that stops making sense (cut off, or broken) just ends there.
fn walk(xml: &str, mut f: impl FnMut(Tag<'_>) -> bool) {
    let mut r = quick_xml::Reader::from_str(xml);
    loop {
        let go = match r.read_event() {
            Ok(Event::Start(e)) => f(Tag::Start(&e)),
            Ok(Event::Empty(e)) => f(Tag::Start(&e)) && f(Tag::End(local(&e))),
            Ok(Event::End(e)) => f(Tag::End(e.local_name().into_inner())),
            Ok(Event::Text(t)) => f(Tag::Text(&t.xml10_content())),
            Ok(Event::CData(t)) => f(Tag::Text(&t.xml10_content())),
            Ok(Event::GeneralRef(r)) => {
                let c = match r.resolve_char_ref() {
                    Ok(Some(c)) => Some(c.to_string()),
                    Ok(None) => quick_xml::escape::resolve_predefined_entity(&r).map(str::to_owned),
                    Err(_) => None,
                };
                match c {
                    Some(c) => f(Tag::Text(&c)),
                    None => true,
                }
            }
            Ok(Event::Eof) | Err(_) => return,
            Ok(_) => true,
        };
        if !go {
            return;
        }
    }
}

/// Word: paragraphs as lines, tabs as tabs, and table rows as lines with
/// their cells tab-separated.
fn docx_text<R: Read + std::io::Seek>(p: &mut Parts<R>) -> std::result::Result<Out, Unreadable> {
    let xml = p
        .read("word/document.xml")?
        .ok_or(Unreadable::Shape("no document part"))?;
    let mut out = Out::default();
    let (mut in_t, mut runs, mut cells) = (false, 0usize, 0usize);
    walk(&xml, |tag| {
        match tag {
            Tag::Start(e) => match local(e) {
                "t" => in_t = true,
                "r" => runs += 1,
                "tc" => cells += 1,
                // Outside a run, a tab is a tab stop's setting, not text.
                "tab" if runs > 0 => out.push("\t"),
                "br" | "cr" if runs > 0 => out.push("\n"),
                _ => {}
            },
            Tag::End(name) => match name {
                "t" => in_t = false,
                "r" => runs = runs.saturating_sub(1),
                "p" if cells > 0 => out.push(" "),
                "p" => out.push("\n"),
                "tc" => {
                    cells = cells.saturating_sub(1);
                    // The space after the cell's last paragraph.
                    if out.s.ends_with(' ') {
                        out.s.pop();
                    }
                    out.push("\t");
                }
                "tr" => out.push("\n"),
                "tbl" => out.push("\n"),
                _ => {}
            },
            Tag::Text(t) if in_t => out.push(t),
            Tag::Text(_) => {}
        }
        !out.full()
    });
    Ok(out)
}

/// The number in a part name such as `ppt/slides/slide12.xml`.
fn part_number(name: &str, prefix: &str) -> Option<u32> {
    name.strip_prefix(prefix)?
        .strip_suffix(".xml")?
        .parse()
        .ok()
}

/// PowerPoint: each slide in number order under a "Slide N" heading, its
/// paragraphs as lines.
fn pptx_text<R: Read + std::io::Seek>(p: &mut Parts<R>) -> std::result::Result<Out, Unreadable> {
    let mut slides: Vec<(u32, String)> = p
        .names()
        .into_iter()
        .filter_map(|n| part_number(&n, "ppt/slides/slide").map(|k| (k, n)))
        .collect();
    if slides.is_empty() {
        return Err(Unreadable::Shape("no slides"));
    }
    slides.sort();
    slides.truncate(MAX_PARTS);
    let mut out = Out::default();
    for (n, part) in slides {
        let Some(xml) = p.read(&part)? else { continue };
        out.push(&format!("Slide {n}\n"));
        let mut in_t = false;
        walk(&xml, |tag| {
            match tag {
                Tag::Start(e) if local(e) == "t" => in_t = true,
                Tag::Start(e) if local(e) == "br" => out.push("\n"),
                Tag::End("t") => in_t = false,
                Tag::End("p") => out.push("\n"),
                Tag::Text(t) if in_t => out.push(t),
                _ => {}
            }
            !out.full()
        });
        out.push("\n");
        if out.full() {
            break;
        }
    }
    Ok(out)
}

/// The sheets of a workbook in the order Excel shows them: (name, part).
/// Falls back to the sheet parts in number order.
fn sheets<R: Read + std::io::Seek>(
    p: &mut Parts<R>,
) -> std::result::Result<Vec<(String, String)>, Unreadable> {
    let mut targets = std::collections::HashMap::new();
    if let Some(xml) = p.read("xl/_rels/workbook.xml.rels")? {
        walk(&xml, |tag| {
            if let Tag::Start(e) = tag {
                if let (Some(id), Some(t)) = (attr(e, "Id"), attr(e, "Target")) {
                    let part = match t.strip_prefix('/') {
                        Some(abs) => abs.to_owned(),
                        None => format!("xl/{t}"),
                    };
                    if !part.contains("..") {
                        targets.insert(id, part);
                    }
                }
            }
            true
        });
    }
    let mut list = Vec::new();
    if let Some(xml) = p.read("xl/workbook.xml")? {
        walk(&xml, |tag| {
            if let Tag::Start(e) = tag {
                if local(e) == "sheet" {
                    if let (Some(name), Some(part)) = (
                        attr(e, "name"),
                        attr(e, "id").and_then(|id| targets.get(&id)),
                    ) {
                        list.push((name, part.clone()));
                    }
                }
            }
            list.len() < MAX_PARTS
        });
    }
    if list.is_empty() {
        let mut parts: Vec<(u32, String)> = p
            .names()
            .into_iter()
            .filter_map(|n| part_number(&n, "xl/worksheets/sheet").map(|k| (k, n)))
            .collect();
        parts.sort();
        parts.truncate(MAX_PARTS);
        list = parts
            .into_iter()
            .map(|(k, n)| (format!("Sheet{k}"), n))
            .collect();
    }
    Ok(list)
}

/// The column of a cell reference such as `C7`, from 0.
fn column(cell: &str) -> Option<usize> {
    let letters = cell.bytes().take_while(u8::is_ascii_alphabetic);
    let mut n = 0usize;
    let mut any = false;
    for b in letters {
        any = true;
        n = n * 26 + usize::from(b.to_ascii_uppercase() - b'A' + 1);
        if n > 16_384 {
            return None;
        }
    }
    any.then(|| n - 1)
}

/// Excel: each sheet under a "Sheet: <name>" heading, a line per row with
/// its cells tab-separated. Shows the values Excel last saved, not formulas.
fn xlsx_text<R: Read + std::io::Seek>(p: &mut Parts<R>) -> std::result::Result<Out, Unreadable> {
    let mut shared: Vec<String> = Vec::new();
    if let Some(xml) = p.read("xl/sharedStrings.xml")? {
        let (mut cur, mut in_t, mut phonetic) = (String::new(), false, false);
        walk(&xml, |tag| {
            match tag {
                Tag::Start(e) => match local(e) {
                    "t" => in_t = true,
                    "rPh" => phonetic = true,
                    _ => {}
                },
                Tag::End("t") => in_t = false,
                Tag::End("rPh") => phonetic = false,
                Tag::End("si") => shared.push(std::mem::take(&mut cur)),
                Tag::Text(t) if in_t && !phonetic => cur.push_str(t),
                _ => {}
            }
            true
        });
    }
    let list = sheets(p)?;
    if list.is_empty() {
        return Err(Unreadable::Shape("no sheets"));
    }
    let mut out = Out::default();
    for (name, part) in list {
        let Some(xml) = p.read(&part)? else { continue };
        out.push(&format!("Sheet: {name}\n"));
        let mut row: Vec<String> = Vec::new();
        let (mut kind, mut col, mut value, mut in_v) =
            (String::new(), 0usize, String::new(), false);
        walk(&xml, |tag| {
            match tag {
                Tag::Start(e) => match local(e) {
                    "row" => row.clear(),
                    "c" => {
                        kind = attr(e, "t").unwrap_or_default();
                        col = attr(e, "r").and_then(|r| column(&r)).unwrap_or(row.len());
                        value.clear();
                    }
                    // A plain value, or the text of an inline string.
                    "v" | "t" => in_v = true,
                    _ => {}
                },
                Tag::End(name) => match name {
                    "v" | "t" => in_v = false,
                    "c" => {
                        let shown = match kind.as_str() {
                            "s" => value
                                .trim()
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| shared.get(i).cloned())
                                .unwrap_or_default(),
                            "b" => match value.trim() {
                                "1" => "TRUE".to_owned(),
                                _ => "FALSE".to_owned(),
                            },
                            _ => std::mem::take(&mut value),
                        };
                        // Tabs and line breaks inside a cell would break the row.
                        let shown = shown.replace(['\t', '\n', '\r'], " ");
                        if col >= row.len() {
                            row.resize(col, String::new());
                            row.push(shown);
                        } else {
                            row[col] = shown;
                        }
                    }
                    "row" => {
                        while row.last().is_some_and(|c| c.trim().is_empty()) {
                            row.pop();
                        }
                        if !row.is_empty() {
                            out.push(&row.join("\t"));
                            out.push("\n");
                        }
                        row.clear();
                    }
                    _ => {}
                },
                Tag::Text(t) if in_v => value.push_str(t),
                Tag::Text(_) => {}
            }
            !out.full()
        });
        out.push("\n");
        if out.full() {
            break;
        }
    }
    Ok(out)
}

// ---------- A run's copy ----------

/// Fills an octal field of a tar header, ending in a zero byte.
fn octal(field: &mut [u8], v: u64) -> std::io::Result<()> {
    let w = field.len() - 1;
    let s = format!("{v:0w$o}");
    if s.len() > w {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "number too large for a tar header",
        ));
    }
    field[..w].copy_from_slice(s.as_bytes());
    field[w] = 0;
    Ok(())
}

/// A ustar header for a plain, read-only file with no owner and no date.
fn tar_header(name: &str, size: u64) -> std::io::Result<[u8; 512]> {
    let plain = !name.is_empty()
        && name.len() <= 100
        && name.is_ascii()
        && !name.contains(['/', '\\', '\0'])
        && name != "."
        && name != "..";
    if !plain {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a plain file name",
        ));
    }
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut h[100..108], 0o444)?;
    octal(&mut h[108..116], 0)?;
    octal(&mut h[116..124], 0)?;
    octal(&mut h[124..136], size)?;
    octal(&mut h[136..148], 0)?;
    h[148..156].fill(b' ');
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    Ok(h)
}

/// Writes a plain tar of `entries` (name in the tar, file on disk) to `out`:
/// regular files only, then the two empty blocks that end a tar.
pub(crate) fn write_tar(
    out: &mut impl Write,
    entries: &[(String, PathBuf)],
) -> std::io::Result<()> {
    for (name, path) in entries {
        let mut f = std::fs::File::open(path)?;
        let len = f.metadata()?.len();
        out.write_all(&tar_header(name, len)?)?;
        let copied = std::io::copy(&mut (&mut f).take(len), out)?;
        if copied != len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "a file got shorter while it was packed",
            ));
        }
        let pad = (512 - (len % 512) as usize) % 512;
        out.write_all(&[0u8; 512][..pad])?;
    }
    out.write_all(&[0u8; 1024])
}

/// Clears away files in `dir` older than [`LEFTOVER_AGE`] whose names start
/// with `prefix`: what an upload or a run's copy left behind when the hub
/// stopped halfway.
fn sweep(dir: &std::path::Path, prefix: &str) {
    let Ok(list) = std::fs::read_dir(dir) else {
        return;
    };
    for e in list.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > LEFTOVER_AGE);
        if old && e.file_name().to_string_lossy().starts_with(prefix) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Packs `list` into a tar in a temporary file and streams it; the file is
/// removed once sent, or once the caller hangs up.
async fn send_tar(files: &Files, list: Vec<Stored>) -> Response {
    let runs = files.context_runs();
    let path = runs.join(format!("run-{}.tar", uuid::Uuid::new_v4().simple()));
    let mut entries = Vec::new();
    for s in &list {
        entries.push((s.name.clone(), files.context_original(s.app_id, s.id)));
        if s.text {
            entries.push((text_name(&s.name), files.context_text(s.app_id, s.id)));
        }
    }
    let target = path.clone();
    let built = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&runs)?;
        sweep(&runs, "run-");
        let mut w = std::io::BufWriter::new(std::fs::File::create(&target)?);
        write_tar(&mut w, &entries)?;
        w.into_inner().map_err(|e| e.into_error())?.sync_all()
    })
    .await;
    let failed = |why: String| {
        tracing::error!(error = %why, "could not pack attached files");
        let _ = std::fs::remove_file(&path);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not pack the attached files",
        )
            .into_response()
    };
    match built {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return failed(e.to_string()),
        Err(e) => return failed(e.to_string()),
    }
    match tokio::fs::File::open(&path).await {
        Ok(f) => stream_file(f, None, Some(RemoveOnDrop(path))).await,
        Err(e) => failed(e.to_string()),
    }
}

// ---------- Handlers ----------

fn no_folder() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, NO_FOLDER).into_response()
}

fn too_large() -> Response {
    (StatusCode::PAYLOAD_TOO_LARGE, too_large_words()).into_response()
}

pub(crate) async fn list(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(app_id): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing attached files");
    }
    match st.store.context_files(app_id).await {
        Ok(f) => Json(f).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct UploadQuery {
    #[serde(default)]
    name: String,
    /// A goal of the app; empty or left out for the whole app.
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    receipt: Option<String>,
}

/// Streams an upload to `path`, refusing it past [`MAX_FILE_BYTES`]. Returns
/// its size. Nothing is left behind on a refusal.
// Responses are what axum handlers return; boxing them buys nothing here.
#[allow(clippy::result_large_err)]
async fn receive(body: Body, path: &std::path::Path) -> std::result::Result<u64, Response> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let broke = || (StatusCode::BAD_REQUEST, "The upload broke off. Try again.").into_response();
    let mut f = tokio::fs::File::create(path)
        .await
        .map_err(|e| ApiError(disk(e)).into_response())?;
    let mut stream = body.into_data_stream();
    let mut total: u64 = 0;
    let result = async {
        while let Some(chunk) = stream.next().await {
            let chunk: Bytes = chunk.map_err(|e| {
                // The route's own limit, set a little above ours.
                if std::error::Error::source(&e)
                    .is_some_and(|s| s.is::<http_body_util::LengthLimitError>())
                {
                    too_large()
                } else {
                    broke()
                }
            })?;
            total += chunk.len() as u64;
            if total > MAX_FILE_BYTES {
                return Err(too_large());
            }
            f.write_all(&chunk)
                .await
                .map_err(|e| ApiError(disk(e)).into_response())?;
        }
        f.flush()
            .await
            .map_err(|e| ApiError(disk(e)).into_response())?;
        Ok(total)
    }
    .await;
    drop(f);
    if result.is_err() {
        let _ = tokio::fs::remove_file(path).await;
    }
    result
}

/// Attaches a file: the body is the file itself, its name and goal are in
/// the query. Answers 201 with the file, or 200 with the first file for a
/// resent receipt.
pub(crate) async fn upload(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(app_id): Path<i64>,
    Query(q): Query<UploadQuery>,
    req: Request,
) -> Response {
    if c != Caller::Owner {
        return forbidden("attaching files");
    }
    let Some(files) = st.files.clone() else {
        return no_folder();
    };
    let goal_id = match q.goal.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(g) => match g.parse::<i64>() {
            Ok(g) => Some(g),
            Err(_) => return ApiError(StoreError::Invalid(NO_GOAL.into())).into_response(),
        },
    };
    let receipt = q.receipt.as_deref();
    // A resend of a tap that already worked: the first answer, without
    // taking the bytes again.
    match st.store.resent_context_file(receipt).await {
        Ok(Some(f)) => return created_or_resent(f, false),
        Ok(None) => {}
        Err(e) => return ApiError(e).into_response(),
    }
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > MAX_FILE_BYTES) {
        return too_large();
    }
    let name = match st
        .store
        .check_context_file(app_id, goal_id, &q.name, declared)
        .await
    {
        Ok(n) => n,
        Err(e) => return ApiError(e).into_response(),
    };
    let dir = files.context_app_dir(app_id);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return ApiError(disk(e)).into_response();
    }
    let sweep_dir = dir.clone();
    let _ = tokio::task::spawn_blocking(move || sweep(&sweep_dir, "incoming-")).await;
    let tmp = dir.join(format!("incoming-{}", uuid::Uuid::new_v4().simple()));
    let size = match receive(req.into_limited_body(), &tmp).await {
        Ok(n) => n,
        Err(resp) => return resp,
    };
    let kind = FileKind::of(&name);
    let mut reading = read_in_background(kind, name.clone(), tmp.clone()).await;
    let mut tmp_text = None;
    if let Some(text) = reading.text.take() {
        let t = tmp.with_extension("txt");
        match tokio::fs::write(&t, text.as_bytes()).await {
            Ok(()) => tmp_text = Some(t),
            Err(e) => {
                tracing::warn!(error = %e, "could not save the text version of a file");
                reading = Reading::noted(unreadable_note(kind));
            }
        }
    }
    let arrived = Arrived {
        app_id,
        goal_id,
        name,
        size,
        reading,
        tmp,
        tmp_text,
    };
    let added = st.store.add_context_file(&files, &arrived, receipt).await;
    // Moved into place on success; left over otherwise.
    let _ = tokio::fs::remove_file(&arrived.tmp).await;
    if let Some(t) = &arrived.tmp_text {
        let _ = tokio::fs::remove_file(t).await;
    }
    match added {
        Ok((f, fresh)) => created_or_resent(f, fresh),
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct RemoveBody {
    #[serde(default)]
    receipt: Option<String>,
}

/// Removes a file. The receipt may come in a JSON body or the query.
pub(crate) async fn remove(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((app_id, id)): Path<(i64, i64)>,
    Query(q): Query<RemoveBody>,
    body: Bytes,
) -> Response {
    if c != Caller::Owner {
        return forbidden("removing attached files");
    }
    let Some(files) = st.files.clone() else {
        return no_folder();
    };
    let b = if body.iter().all(u8::is_ascii_whitespace) {
        RemoveBody::default()
    } else {
        match serde_json::from_slice::<Option<RemoveBody>>(&body) {
            Ok(b) => b.unwrap_or_default(),
            Err(_) => {
                return (StatusCode::BAD_REQUEST, "send a receipt, or nothing").into_response()
            }
        }
    };
    let receipt = b.receipt.or(q.receipt);
    match st
        .store
        .remove_context_file(&files, app_id, id, receipt.as_deref())
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// Hands the owner a file back, only ever as a download: whatever it holds,
/// a browser never shows it as a page of the hub.
pub(crate) async fn download(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((app_id, id)): Path<(i64, i64)>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("opening attached files");
    }
    let Some(files) = st.files.clone() else {
        return no_folder();
    };
    let f = match st.store.context_file(app_id, id).await {
        Ok(f) => f,
        Err(e) => return ApiError(e).into_response(),
    };
    let Ok(file) = tokio::fs::File::open(files.context_original(app_id, f.id)).await else {
        return (
            StatusCode::NOT_FOUND,
            "This file is missing from the PC. Remove it and attach it again.",
        )
            .into_response();
    };
    let mut resp = stream_file(file, None, None).await;
    let h = resp.headers_mut();
    // Cleaned names hold no quotes or line breaks.
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", f.name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    resp
}

/// A piece of work's files as one tar, for the runner holding it.
pub(crate) async fn runner_thread(
    State(st): State<AppState>,
    runner: Option<Extension<RunnerId>>,
    Path(id): Path<i64>,
) -> Response {
    let (_, files) = match holding(&st, runner, id, Access::Read).await {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(e) = st.store.thread(id).await {
        return ApiError(e).into_response();
    }
    match st.store.stored_for_thread(id).await {
        Ok(list) => send_tar(&files, list).await,
        Err(e) => ApiError(e).into_response(),
    }
}

/// A helper job's files (its goal's and the app's) as one tar. Helpers run
/// on the PC only, so a cloud worker never gets here.
pub(crate) async fn runner_helper(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    runner: Option<Extension<RunnerId>>,
    Path(job): Path<i64>,
) -> Response {
    if c != Caller::Runner {
        return not_runner("fetching a helper's files");
    }
    let r = match runner_only(runner, None) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    // Already refused by cloud_reach; this keeps it so if that list grows.
    if r.place != Place::Pc {
        return (
            StatusCode::FORBIDDEN,
            "cloud workers only run their own work",
        )
            .into_response();
    }
    let Some(files) = st.files.clone() else {
        return no_folder();
    };
    let goal = match st.store.running_helper_goal(job, &r.name).await {
        Ok(g) => g,
        Err(e) => return ApiError(e).into_response(),
    };
    match st.store.stored_for_goal(goal).await {
        Ok(list) => send_tar(&files, list).await,
        Err(e) => ApiError(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    /// A tiny office file, its parts stored without compression.
    fn office_file(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in parts {
            w.start_file(*name, opts).expect("start part");
            w.write_all(body.as_bytes()).expect("write part");
        }
        w.finish().expect("finish zip").into_inner()
    }

    fn read_bytes(kind: FileKind, name: &str, bytes: &[u8]) -> Reading {
        let dir = std::env::temp_dir().join(format!("decurion-ctx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("f");
        std::fs::write(&path, bytes).expect("write");
        let r = read_text(kind, name, &path);
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

    #[test]
    fn word_paragraphs_tabs_and_tables_become_lines() {
        let doc = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><w:document {W}><w:body>
<w:p><w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs></w:pPr><w:r><w:t>Price list</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">Tea </w:t></w:r><w:r><w:tab/><w:t>&amp; cake &#233;</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Item</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Cost</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>Scone</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>2.50</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:p><w:r><w:delText>gone</w:delText><w:t>End</w:t></w:r></w:p>
</w:body></w:document>"#
        );
        let bytes = office_file(&[("word/document.xml", &doc)]);
        let r = read_bytes(FileKind::Word, "Prices.docx", &bytes);
        assert_eq!(
            r.text.as_deref(),
            Some("Price list\nTea \t& cake é\nItem\tCost\nScone\t2.50\n\nEnd")
        );
        assert_eq!(
            r.chars,
            Some(r.text.as_ref().expect("text").chars().count() as u64)
        );
        assert_eq!(r.note, "");
    }

    #[test]
    fn sheets_come_in_workbook_order_with_their_names() {
        let wb = r#"<workbook xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>
<sheet name="Prices &amp; more" sheetId="2" r:id="rId2"/><sheet name="Notes" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        let rels = r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Target="/xl/worksheets/sheet2.xml"/></Relationships>"#;
        let ss = r#"<sst><si><t>Item</t></si><si><r><t>Co</t></r><r><t>st</t></r><rPh><t>x</t></rPh></si><si><t>Hello</t></si></sst>"#;
        let s1 = r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>2</v></c></row></sheetData></worksheet>"#;
        let s2 = r#"<worksheet><sheetData>
<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
<row r="2"><c r="A2" t="inlineStr"><is><t>Tea</t></is></c><c r="C2"><v>3.5</v></c><c r="D2" t="b"><v>1</v></c></row>
<row r="3"><c r="A3"/></row></sheetData></worksheet>"#;
        let bytes = office_file(&[
            ("xl/workbook.xml", wb),
            ("xl/_rels/workbook.xml.rels", rels),
            ("xl/sharedStrings.xml", ss),
            ("xl/worksheets/sheet1.xml", s1),
            ("xl/worksheets/sheet2.xml", s2),
        ]);
        let r = read_bytes(FileKind::Sheet, "book.xlsx", &bytes);
        assert_eq!(
            r.text.as_deref(),
            Some("Sheet: Prices & more\nItem\tCost\nTea\t\t3.5\tTRUE\n\nSheet: Notes\nHello")
        );
        // A csv is already text: no copy.
        assert_eq!(
            read_bytes(FileKind::Sheet, "a.csv", b"a,b\n"),
            Reading::default()
        );
    }

    #[test]
    fn slides_come_in_number_order() {
        let slide = |t: &str| {
            format!(
                r#"<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="p"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>{t}</a:t></a:r></a:p><a:p><a:r><a:t>more</a:t></a:r><a:br/><a:r><a:t>line</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
            )
        };
        let (s1, s2, s10) = (slide("One"), slide("Two"), slide("Ten"));
        let bytes = office_file(&[
            ("ppt/slides/slide10.xml", &s10),
            ("ppt/slides/slide2.xml", &s2),
            ("ppt/slides/slide1.xml", &s1),
            ("ppt/slides/_rels/slide1.xml.rels", "<x/>"),
        ]);
        let r = read_bytes(FileKind::Slides, "deck.pptx", &bytes);
        assert_eq!(
            r.text.as_deref(),
            Some(
                "Slide 1\nOne\nmore\nline\n\nSlide 2\nTwo\nmore\nline\n\nSlide 10\nTen\nmore\nline"
            )
        );
    }

    #[test]
    fn files_that_cannot_be_read_still_come_with_words() {
        let r = read_bytes(FileKind::Word, "x.docx", b"not a zip at all");
        assert_eq!(r.text, None);
        assert!(
            r.note.contains("could not read the text in this Word file"),
            "{}",
            r.note
        );
        let r = read_bytes(FileKind::Other, "old.doc", &[0xd0, 0xcf, 0x11, 0xe0, 0, 0]);
        assert_eq!(r.note, OTHER_NOTE);
        // Code or settings with an unknown ending is text anyway.
        assert_eq!(
            read_bytes(FileKind::Other, "a.py", b"print(1)\n"),
            Reading::default()
        );
        // PDFs are read by the runner, pictures are looked at.
        assert_eq!(
            read_bytes(FileKind::Pdf, "a.pdf", b"%PDF-1.4"),
            Reading::default()
        );
        assert_eq!(
            read_bytes(FileKind::Image, "a.png", b"\x89PNG"),
            Reading::default()
        );
        let empty = office_file(&[(
            "word/document.xml",
            &format!("<w:document {W}><w:body><w:p/></w:body></w:document>"),
        )]);
        let r = read_bytes(FileKind::Word, "e.docx", &empty);
        assert_eq!(r.text, None);
        assert!(r.note.contains("found no text"), "{}", r.note);
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        let mut out = Out::default();
        out.push(&"a".repeat(MAX_TEXT_BYTES - 1));
        out.push("é and more");
        assert!(out.full());
        assert_eq!(out.s.len(), MAX_TEXT_BYTES - 1);
        let r = finish_text(FileKind::Word, out);
        assert!(r.note.contains("a lot of text"), "{}", r.note);
    }

    #[test]
    fn huge_parts_are_only_unpacked_up_to_the_cap() {
        // Highly compressible: a few hundred kilobytes in the file.
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file(
            "word/document.xml",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )
        .expect("start");
        let chunk = vec![b' '; 1 << 20];
        for _ in 0..80 {
            w.write_all(&chunk).expect("write");
        }
        let bytes = w.finish().expect("finish").into_inner();
        assert!(bytes.len() < 2 << 20, "{}", bytes.len());
        let mut parts = Parts {
            zip: zip::ZipArchive::new(Cursor::new(bytes)).expect("zip"),
            unpacked: 0,
        };
        let part = parts
            .read("word/document.xml")
            .expect("read")
            .expect("part");
        assert_eq!(part.len() as u64, MAX_PART_BYTES);
    }

    /// Reads a tar back: (name, mode, size, body) per entry, checking each
    /// header's checksum and the end blocks.
    fn untar(t: &[u8]) -> Vec<(String, u64, Vec<u8>)> {
        let field = |h: &[u8], a: usize, b: usize| {
            let s = std::str::from_utf8(&h[a..b]).expect("ascii");
            u64::from_str_radix(s.trim_matches(['\0', ' ']), 8).expect("octal")
        };
        assert_eq!(t.len() % 512, 0);
        let mut out = Vec::new();
        let mut at = 0;
        loop {
            let h = &t[at..at + 512];
            if h.iter().all(|&b| b == 0) {
                assert!(t[at..].iter().all(|&b| b == 0));
                assert_eq!(t.len() - at, 1024, "two zero blocks end it");
                return out;
            }
            let mut sum: u64 = h.iter().map(|&b| u64::from(b)).sum();
            sum -= h[148..156].iter().map(|&b| u64::from(b)).sum::<u64>();
            sum += 8 * u64::from(b' ');
            assert_eq!(field(h, 148, 155), sum, "checksum");
            assert_eq!(&h[257..265], b"ustar\x0000");
            assert_eq!(h[156], b'0');
            assert_eq!(field(h, 100, 108), 0o444);
            assert_eq!(field(h, 136, 148), 0);
            assert_eq!(field(h, 108, 116), 0);
            let name_end = h[..100].iter().position(|&b| b == 0).unwrap_or(100);
            let name = String::from_utf8(h[..name_end].to_vec()).expect("name");
            let size = field(h, 124, 136);
            let body = t[at + 512..at + 512 + size as usize].to_vec();
            out.push((name, size, body));
            at += 512 + (size as usize).div_ceil(512) * 512;
        }
    }

    #[test]
    fn a_runs_tar_holds_plain_read_only_files() {
        let dir = std::env::temp_dir().join(format!("decurion-tar-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let a = dir.join("1");
        let b = dir.join("2");
        std::fs::write(&a, b"hello").expect("write");
        std::fs::write(&b, vec![7u8; 1024]).expect("write");
        let long = format!("{}.docx", "n".repeat(75));
        let entries = vec![
            ("Spec v2.pdf".to_owned(), a.clone()),
            (long.clone(), b.clone()),
            (text_name(&long), a.clone()),
        ];
        let mut t = Vec::new();
        write_tar(&mut t, &entries).expect("tar");
        let got = untar(&t);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], ("Spec v2.pdf".to_owned(), 5, b"hello".to_vec()));
        assert_eq!(got[1], (long.clone(), 1024, vec![7u8; 1024]));
        assert_eq!(got[2].0, format!("{long}.txt"));
        // If tar is on this computer, it reads it too.
        let path = dir.join("run.tar");
        std::fs::write(&path, &t).expect("write tar");
        if let Ok(o) = std::process::Command::new("tar")
            .arg("-tvf")
            .arg(&path)
            .output()
        {
            if o.status.success() {
                let list = String::from_utf8_lossy(&o.stdout);
                assert!(list.contains("Spec v2.pdf"), "{list}");
                assert!(list.contains("r--r--r--"), "{list}");
            }
        }
        // Nothing that could land outside the folder.
        for bad in ["../x", "a/b", "", "."] {
            assert!(tar_header(bad, 1).is_err(), "{bad}");
        }
        assert!(tar_header(&"x".repeat(101), 1).is_err());
        let mut empty = Vec::new();
        write_tar(&mut empty, &[]).expect("empty tar");
        assert_eq!(empty, vec![0u8; 1024]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        assert_eq!(free_name("image.jpg", &[]).as_deref(), Some("image.jpg"));
        assert_eq!(
            free_name("image.jpg", &["IMAGE.JPG"]).as_deref(),
            Some("image (2).jpg")
        );
        assert_eq!(
            free_name("image.jpg", &["image.jpg", "image (2).jpg.txt"]).as_deref(),
            Some("image (3).jpg")
        );
        // A text version's name is taken too.
        assert_eq!(
            free_name("a.docx.txt", &["a.docx"]).as_deref(),
            Some("a.docx (2).txt")
        );
        assert_eq!(
            free_name("README", &["readme"]).as_deref(),
            Some("README (2)")
        );
        let long = format!("{}.xlsx", "a".repeat(MAX_NAME - 5));
        let got = free_name(&long, &[&long]).expect("a free name");
        assert_eq!(got.len(), MAX_NAME);
        assert!(got.ends_with(" (2).xlsx"), "{got}");
        assert_eq!(clean_name(&got).as_deref(), Ok(got.as_str()));
    }

    #[test]
    fn columns_come_from_cell_references() {
        assert_eq!(column("A1"), Some(0));
        assert_eq!(column("C7"), Some(2));
        assert_eq!(column("AA10"), Some(26));
        assert_eq!(column("7"), None);
        assert_eq!(column("ZZZZ1"), None);
    }
}
