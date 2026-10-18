//! The master plan and the page library: what each app's goals have done,
//! what is left, and every page agents drew, beside the goal and piece it
//! belongs to. Shapes and rules are in [`decurion_proto::plan`].
//!
//! Goal rows read their title, steps and state from the goal itself each
//! time, so the plan never falls behind the work. Every goal gets its row in
//! the same transaction that starts it ([`add_goal_row`]). Only the owner
//! changes the plan by hand; the project lead can only add to-do items under
//! the goal it is working on ([`add_lead_todos`]), and they do nothing until
//! the owner taps. Turning to-dos into a goal ([`Store::fix_todos`]) starts
//! one goal and links each item to it; closing that goal as done ticks them
//! in the same transaction ([`tick_fixed`]). Stopping it leaves them open,
//! so they can be fixed again.

use crate::store::{check_receipt, receipt_result, save_receipt, Result, Store, StoreError};
use crate::{created_or_resent, forbidden, ApiError, AppState, Caller};
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use axum::Json;
use decurion_proto::goal::{Goal, GoalState, PageKind, ReviewState};
use decurion_proto::plan::{
    Direction, EditTodo, FixTodos, GoalRef, ItemKind, ItemSource, ItemState, LeadTodo, LibraryPage,
    MoveItem, NewTodo, PlanItem, PlanStep, PlanView, StepState,
};
use decurion_proto::ThreadState;
use serde::Deserialize;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, Transaction};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::str::FromStr;

/// Most to-do items one goal may be started to fix.
pub const MAX_FIX: usize = 10;
const MAX_TITLE: usize = 200;
const MAX_DETAIL: usize = 1000;
const MAX_NOTE: usize = 2000;

/// Every column the library shows, joined from the page, its goal, its app
/// and its piece.
const PAGE_SELECT: &str = "SELECT gp.id, gp.goal_id, g.title AS goal_title, g.app_id, a.name AS app_name,
        gp.kind, gp.title, gp.thread_id, p.title AS piece_title, gp.created_at,
        COALESCE(json_array_length(q.questions), 0) AS questions,
        (q.page_id IS NOT NULL AND q.answers IS NULL AND g.state NOT IN ('done', 'stopped')) AS waiting
    FROM goal_pages gp
    JOIN goals g ON g.id = gp.goal_id
    JOIN apps a ON a.id = g.app_id
    LEFT JOIN goal_pieces p ON p.thread_id = gp.thread_id
    LEFT JOIN page_questions q ON q.page_id = gp.id";

/// The name a page goes by when it has no title of its own.
fn fallback_title(kind: PageKind, piece: Option<&str>) -> String {
    match (kind, piece) {
        (PageKind::Plan, _) => "Visual plan".to_owned(),
        (PageKind::Check, Some(p)) => format!("As built: {p}"),
        (PageKind::Check, None) => "As built".to_owned(),
    }
}

fn page_from(r: &sqlx::sqlite::SqliteRow) -> Result<LibraryPage> {
    let kind = if r.try_get::<String, _>("kind")? == "plan" {
        PageKind::Plan
    } else {
        PageKind::Check
    };
    let piece_title: Option<String> = r.try_get("piece_title")?;
    let title = r
        .try_get::<Option<String>, _>("title")?
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| fallback_title(kind, piece_title.as_deref()));
    Ok(LibraryPage {
        id: r.try_get("id")?,
        goal_id: r.try_get("goal_id")?,
        goal_title: r.try_get("goal_title")?,
        app_id: r.try_get("app_id")?,
        app_name: r.try_get("app_name")?,
        kind,
        title,
        thread_id: r.try_get("thread_id")?,
        piece_title,
        questions: usize::try_from(r.try_get::<i64, _>("questions")?).unwrap_or(0),
        waiting: r.try_get("waiting")?,
        created_at: r.try_get("created_at")?,
    })
}

impl Store {
    /// Pages agents drew, newest first, for one app, one goal, or all.
    pub async fn library(&self, app: Option<i64>, goal: Option<i64>) -> Result<Vec<LibraryPage>> {
        let rows = sqlx::query(&format!(
            "{PAGE_SELECT} WHERE (?1 IS NULL OR g.app_id = ?1) AND (?2 IS NULL OR gp.goal_id = ?2)
             ORDER BY gp.id DESC"
        ))
        .bind(app)
        .bind(goal)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(page_from).collect()
    }

    /// One page as the library lists it.
    pub async fn library_page(&self, goal: i64, page: i64) -> Result<LibraryPage> {
        let row = sqlx::query(&format!("{PAGE_SELECT} WHERE gp.id = ? AND gp.goal_id = ?"))
            .bind(page)
            .bind(goal)
            .fetch_optional(self.pool())
            .await?
            .ok_or(crate::store::StoreError::NotFound)?;
        page_from(&row)
    }
}

#[derive(Deserialize)]
pub(crate) struct LibraryQuery {
    #[serde(default)]
    app: Option<i64>,
    #[serde(default)]
    goal: Option<i64>,
}

/// `GET /api/library?app=&goal=`. Lists names only; opening a page still
/// goes through the owner-only page route.
pub(crate) async fn library(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Query(q): Query<LibraryQuery>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("listing pages");
    }
    match st.store.library(q.app, q.goal).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// A piece of a goal, as its step on the plan needs it.
#[derive(Debug, Clone)]
pub struct PieceStep {
    pub thread_id: i64,
    pub title: String,
    pub review: ReviewState,
    pub thread: ThreadState,
}

/// A goal's steps, worked out from where the goal is, whether a plan was
/// ever proposed, and its pieces. Pieces exist only once a plan was
/// approved. Pure, so every case is tested without a database.
pub fn goal_steps(state: GoalState, planned: bool, pieces: &[PieceStep]) -> Vec<PlanStep> {
    use GoalState as G;
    use StepState as S;
    let approved = !pieces.is_empty();
    let step = |title: &str, state| PlanStep {
        title: title.to_owned(),
        state,
        thread_id: None,
    };
    let talk = match state {
        G::Talking => S::Now,
        G::Stopped if !(planned || approved) => S::Later,
        _ => S::Done,
    };
    let approve = if approved {
        S::Done
    } else {
        match state {
            G::PlanReady => S::You,
            G::Talking => S::Later,
            G::Working | G::ForReview => S::Done,
            // Closed without a plan ever approved.
            G::Done | G::Stopped => S::Skipped,
        }
    };
    let mut steps = vec![
        step("Talk it through", talk),
        step("Approve the plan", approve),
    ];
    for p in pieces {
        let s = if p.thread == ThreadState::Done {
            S::Done
        } else {
            match p.review {
                ReviewState::Passed => S::Done,
                // The checker handed it to the owner: theirs to look at,
                // not a failure.
                ReviewState::NeedsALook => S::You,
                ReviewState::Failed => S::Problem,
                ReviewState::Building | ReviewState::Reviewing | ReviewState::Fixing => S::Now,
            }
        };
        steps.push(PlanStep {
            title: p.title.clone(),
            state: s,
            thread_id: Some(p.thread_id),
        });
    }
    steps.push(step(
        "Your review",
        match state {
            G::ForReview => S::You,
            G::Done => S::Done,
            _ => S::Later,
        },
    ));
    if state == G::Stopped {
        for s in &mut steps {
            if s.state != S::Done {
                s.state = S::Skipped;
            }
        }
    }
    steps
}

/// Adds the last step for an app that deploys: "Done" is not "live". A goal
/// whose merged pull requests are all in a release from Decurion ends with
/// "Live"; one with a merge not released yet ends with "Merged · ready to
/// deploy", the owner's to tap; an open goal shows "Live" still to come.
/// Apps that do not deploy, stopped goals, and goals closed with nothing
/// merged are left as they are. `live` comes from `Store::goals_live`.
pub fn with_live(
    mut steps: Vec<PlanStep>,
    state: GoalState,
    can_deploy: bool,
    live: Option<bool>,
) -> Vec<PlanStep> {
    let step = |title: &str, state| PlanStep {
        title: title.to_owned(),
        state,
        thread_id: None,
    };
    if !can_deploy || state == GoalState::Stopped {
        return steps;
    }
    match live {
        Some(true) => steps.push(step("Live", StepState::Done)),
        Some(false) => steps.push(step("Merged · ready to deploy", StepState::You)),
        None if state == GoalState::Done => {}
        None => steps.push(step("Live", StepState::Later)),
    }
    steps
}

/// A goal row's state: done when the goal was closed as done, dropped when
/// it was stopped.
fn goal_item_state(g: GoalState) -> ItemState {
    match g {
        GoalState::Done => ItemState::Done,
        GoalState::Stopped => ItemState::Dropped,
        _ => ItemState::Open,
    }
}

fn bad(msg: &str) -> StoreError {
    StoreError::Invalid(msg.to_owned())
}

/// Cuts `s` to at most `n` characters.
fn clip(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

fn clean_title(t: &str) -> Result<String> {
    let t = t.trim();
    if t.is_empty() {
        return Err(bad("Write what needs doing."));
    }
    if t.chars().count() > MAX_TITLE {
        return Err(bad(
            "Keep a to-do to 200 characters or fewer. Put the rest in its details.",
        ));
    }
    Ok(t.to_owned())
}

fn clean_detail(d: Option<&str>) -> Result<Option<String>> {
    let d = d.map(str::trim).filter(|d| !d.is_empty());
    if d.is_some_and(|d| d.chars().count() > MAX_DETAIL) {
        return Err(bad("Keep the details to 1000 characters or fewer."));
    }
    Ok(d.map(str::to_owned))
}

/// One `plan_items` row as stored.
struct ItemRow {
    id: i64,
    parent_id: Option<i64>,
    kind: ItemKind,
    goal_id: Option<i64>,
    title: String,
    detail: Option<String>,
    source: ItemSource,
    state: ItemState,
    fix_goal_id: Option<i64>,
    created_at: String,
    done_at: Option<String>,
}

const ITEM_COLS: &str = "id, parent_id, kind, goal_id, title, detail, source, state, fix_goal_id,
    created_at, done_at";

fn item_row(r: &SqliteRow) -> Result<ItemRow> {
    let kind = match r.try_get::<String, _>("kind")?.as_str() {
        "goal" => ItemKind::Goal,
        _ => ItemKind::Todo,
    };
    let source = match r.try_get::<String, _>("source")?.as_str() {
        "lead" => ItemSource::Lead,
        _ => ItemSource::Owner,
    };
    let state = match r.try_get::<String, _>("state")?.as_str() {
        "done" => ItemState::Done,
        "dropped" => ItemState::Dropped,
        _ => ItemState::Open,
    };
    Ok(ItemRow {
        id: r.try_get("id")?,
        parent_id: r.try_get("parent_id")?,
        kind,
        goal_id: r.try_get("goal_id")?,
        title: r.try_get("title")?,
        detail: r.try_get("detail")?,
        source,
        state,
        fix_goal_id: r.try_get("fix_goal_id")?,
        created_at: r.try_get("created_at")?,
        done_at: r.try_get("done_at")?,
    })
}

/// A to-do as the plan shows it, with the goal started to do it, if any.
fn todo_item(row: ItemRow, fix_goal: Option<GoalRef>) -> PlanItem {
    PlanItem {
        id: row.id,
        kind: ItemKind::Todo,
        title: row.title,
        detail: row.detail,
        source: row.source,
        state: row.state,
        goal: fix_goal,
        steps: Vec::new(),
        found: Vec::new(),
        pages: Vec::new(),
        created_at: row.created_at,
        done_at: row.done_at,
    }
}

/// A goal as a plan row refers to it, with when it last changed.
struct LiveGoal {
    goal: GoalRef,
    updated_at: String,
}

const LIVE_GOAL_COLS: &str = "id, title, state, updated_at";

fn live_goal(r: &SqliteRow) -> Result<LiveGoal> {
    let state: String = r.try_get("state")?;
    Ok(LiveGoal {
        goal: GoalRef {
            id: r.try_get("id")?,
            title: r.try_get("title")?,
            state: GoalState::from_str(&state).map_err(|e| StoreError::Invalid(e.to_string()))?,
        },
        updated_at: r.try_get("updated_at")?,
    })
}

/// A top-level row of the plan while it is put together.
enum Top {
    Goal(ItemRow),
    Todo(PlanItem),
}

/// Puts a goal on its app's master plan, at the end of the top level.
/// Called in the transaction that starts the goal. Returns the row's id.
pub(crate) async fn add_goal_row(
    tx: &mut Transaction<'_, Sqlite>,
    app_id: i64,
    goal_id: i64,
    title: &str,
) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "INSERT INTO plan_items (app_id, position, kind, goal_id, title, source)
         SELECT ?1, COALESCE(MAX(position), 0) + 1, 'goal', ?2, ?3, 'owner'
         FROM plan_items WHERE app_id = ?1 AND parent_id IS NULL
         RETURNING id",
    )
    .bind(app_id)
    .bind(goal_id)
    .bind(title)
    .fetch_one(&mut **tx)
    .await?)
}

/// Inserts a to-do at the end of the rows beside it.
async fn insert_todo(
    tx: &mut Transaction<'_, Sqlite>,
    app_id: i64,
    parent_id: Option<i64>,
    title: &str,
    detail: Option<&str>,
    source: ItemSource,
) -> Result<i64> {
    let source = match source {
        ItemSource::Owner => "owner",
        ItemSource::Lead => "lead",
    };
    Ok(sqlx::query_scalar(
        "INSERT INTO plan_items (app_id, parent_id, position, kind, title, detail, source)
         SELECT ?1, ?2, COALESCE(MAX(position), 0) + 1, 'todo', ?3, ?4, ?5
         FROM plan_items WHERE app_id = ?1 AND parent_id IS ?2
         RETURNING id",
    )
    .bind(app_id)
    .bind(parent_id)
    .bind(title)
    .bind(detail)
    .bind(source)
    .fetch_one(&mut **tx)
    .await?)
}

/// The project lead's to-do list, put under its goal's row. A title the
/// goal already has (same words, any case) is skipped, so a lead repeating
/// itself across turns adds nothing.
pub(crate) async fn add_lead_todos(
    tx: &mut Transaction<'_, Sqlite>,
    app_id: i64,
    goal_id: i64,
    goal_title: &str,
    todo: &[LeadTodo],
) -> Result<()> {
    if todo.is_empty() {
        return Ok(());
    }
    let parent: Option<i64> = sqlx::query_scalar("SELECT id FROM plan_items WHERE goal_id = ?")
        .bind(goal_id)
        .fetch_optional(&mut **tx)
        .await?;
    let parent = match parent {
        Some(p) => p,
        None => add_goal_row(tx, app_id, goal_id, goal_title).await?,
    };
    let mut seen: HashSet<String> =
        sqlx::query_scalar::<_, String>("SELECT title FROM plan_items WHERE parent_id = ?")
            .bind(parent)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|t| t.trim().to_lowercase())
            .collect();
    for t in todo {
        // Checked by `validate_todo` already; clipped again so a change there
        // can never store more than the owner's own limits.
        let title = clip(t.title.trim(), MAX_TITLE);
        if !seen.insert(title.to_lowercase()) {
            continue;
        }
        let detail = t
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(|d| clip(d, MAX_DETAIL));
        insert_todo(
            tx,
            app_id,
            Some(parent),
            &title,
            detail.as_deref(),
            ItemSource::Lead,
        )
        .await?;
    }
    Ok(())
}

/// Ticks the open to-dos a goal was started to fix. Called in the
/// transaction that closes the goal as done.
pub(crate) async fn tick_fixed(tx: &mut Transaction<'_, Sqlite>, goal_id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE plan_items SET state = 'done',
             done_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE fix_goal_id = ? AND kind = 'todo' AND state = 'open'",
    )
    .bind(goal_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn app_exists(tx: &mut Transaction<'_, Sqlite>, app_id: i64) -> Result<()> {
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM apps WHERE id = ?")
        .bind(app_id)
        .fetch_optional(&mut **tx)
        .await?;
    found.map(|_| ()).ok_or(StoreError::NotFound)
}

/// The goal's words when the owner turns to-dos into a goal: what to fix,
/// where it was found, and the owner's own note.
fn fix_text(
    found_in: Option<&str>,
    items: &[(String, Option<String>)],
    note: Option<&str>,
) -> String {
    let mut s = match found_in {
        Some(g) => format!("Please fix these, found while working on \"{g}\":\n"),
        None => "Please take care of these, from this app's master plan:\n".to_owned(),
    };
    for (title, detail) in items {
        match detail {
            Some(d) => {
                let _ = writeln!(s, "- {title}: {d}");
            }
            None => {
                let _ = writeln!(s, "- {title}");
            }
        }
    }
    if let Some(n) = note {
        let _ = write!(s, "\n{n}\n");
    }
    s.trim_end().to_owned()
}

/// A fix goal's title: the one item, or the first and how many more.
fn fix_title(items: &[(String, Option<String>)]) -> String {
    match items {
        [] => "Fix to-do items".to_owned(),
        [(one, _)] => clip(one, 80),
        [(first, _), rest @ ..] => format!("{} (and {} more)", clip(first, 60), rest.len()),
    }
}

impl Store {
    /// An app's master plan: top-level rows in the owner's order, each goal
    /// with its steps, what it found and its pages.
    pub async fn plan_view(&self, app_id: i64) -> Result<PlanView> {
        let app = self.app(app_id).await?;
        // Read before the transaction, which holds this task's one connection.
        let live = self.goals_live(Some(app_id)).await?;
        let mut tx = self.pool().begin().await?; // read-only: a reader never waits on writers
        let rows = sqlx::query(&format!(
            "SELECT {ITEM_COLS} FROM plan_items WHERE app_id = ? ORDER BY position, id"
        ))
        .bind(app_id)
        .fetch_all(&mut *tx)
        .await?;
        let rows: Vec<ItemRow> = rows.iter().map(item_row).collect::<Result<_>>()?;
        let mut goals: HashMap<i64, LiveGoal> = HashMap::new();
        for r in &sqlx::query(&format!(
            "SELECT {LIVE_GOAL_COLS} FROM goals WHERE app_id = ?"
        ))
        .bind(app_id)
        .fetch_all(&mut *tx)
        .await?
        {
            let g = live_goal(r)?;
            goals.insert(g.goal.id, g);
        }
        let planned: HashSet<i64> = sqlx::query_scalar::<_, i64>(
            "SELECT DISTINCT m.goal_id FROM goal_messages m JOIN goals g ON g.id = m.goal_id
             WHERE g.app_id = ? AND json_extract(m.body, '$.kind') = 'coordinator'
               AND json_extract(m.body, '$.plan') IS NOT NULL",
        )
        .bind(app_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
        let can_deploy: bool = sqlx::query_scalar("SELECT can_deploy FROM apps WHERE id = ?")
            .bind(app_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or(false);
        let mut pieces: HashMap<i64, Vec<PieceStep>> = HashMap::new();
        for r in &sqlx::query(
            "SELECT p.goal_id, p.thread_id, p.title, p.review, t.state
             FROM goal_pieces p JOIN threads t ON t.id = p.thread_id JOIN goals g ON g.id = p.goal_id
             WHERE g.app_id = ? ORDER BY p.thread_id",
        )
        .bind(app_id)
        .fetch_all(&mut *tx)
        .await?
        {
            pieces
                .entry(r.try_get("goal_id")?)
                .or_default()
                .push(PieceStep {
                    thread_id: r.try_get("thread_id")?,
                    title: r.try_get("title")?,
                    review: ReviewState::from_str(&r.try_get::<String, _>("review")?)
                        .map_err(|e| StoreError::Invalid(e.to_string()))?,
                    thread: ThreadState::from_str(&r.try_get::<String, _>("state")?)
                        .map_err(|e| StoreError::Invalid(e.to_string()))?,
                });
        }
        tx.commit().await?;
        // Newest first from the library; the plan shows them as they came.
        let mut pages: HashMap<i64, Vec<LibraryPage>> = HashMap::new();
        for p in self.library(Some(app_id), None).await?.into_iter().rev() {
            pages.entry(p.goal_id).or_default().push(p);
        }

        let (mut done, mut left) = (0, 0);
        let mut count = |s: ItemState| match s {
            ItemState::Done => done += 1,
            ItemState::Open => left += 1,
            ItemState::Dropped => {}
        };
        let mut children: HashMap<i64, Vec<PlanItem>> = HashMap::new();
        let mut top = Vec::new();
        for row in rows {
            match row.kind {
                ItemKind::Todo => {
                    count(row.state);
                    let fix = row
                        .fix_goal_id
                        .and_then(|id| goals.get(&id))
                        .map(|g| g.goal.clone());
                    let parent = row.parent_id;
                    let item = todo_item(row, fix);
                    match parent {
                        Some(p) => children.entry(p).or_default().push(item),
                        None => top.push(Top::Todo(item)),
                    }
                }
                ItemKind::Goal => {
                    if let Some(g) = row.goal_id.and_then(|id| goals.get(&id)) {
                        count(goal_item_state(g.goal.state));
                        top.push(Top::Goal(row));
                    }
                }
            }
        }
        let mut items = Vec::new();
        for entry in top {
            let row = match entry {
                Top::Todo(item) => {
                    items.push(item);
                    continue;
                }
                Top::Goal(row) => row,
            };
            let Some(g) = row.goal_id.and_then(|id| goals.get(&id)) else {
                continue;
            };
            let id = g.goal.id;
            items.push(PlanItem {
                id: row.id,
                kind: ItemKind::Goal,
                title: g.goal.title.clone(),
                detail: None,
                source: row.source,
                state: goal_item_state(g.goal.state),
                goal: Some(g.goal.clone()),
                steps: with_live(
                    goal_steps(
                        g.goal.state,
                        planned.contains(&id),
                        &pieces.remove(&id).unwrap_or_default(),
                    ),
                    g.goal.state,
                    can_deploy,
                    live.get(&id).copied(),
                ),
                found: children.remove(&row.id).unwrap_or_default(),
                pages: pages.remove(&id).unwrap_or_default(),
                created_at: row.created_at,
                done_at: (g.goal.state == GoalState::Done).then(|| g.updated_at.clone()),
            });
        }
        Ok(PlanView {
            app_id,
            app_name: app.name,
            items,
            done,
            left,
        })
    }

    /// One to-do as the plan shows it.
    async fn todo(&self, id: i64) -> Result<PlanItem> {
        let row = sqlx::query(&format!(
            "SELECT {ITEM_COLS} FROM plan_items WHERE id = ? AND kind = 'todo'"
        ))
        .bind(id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::NotFound)?;
        let row = item_row(&row)?;
        let fix = match row.fix_goal_id {
            Some(g) => sqlx::query(&format!("SELECT {LIVE_GOAL_COLS} FROM goals WHERE id = ?"))
                .bind(g)
                .fetch_optional(self.pool())
                .await?
                .map(|r| live_goal(&r))
                .transpose()?
                .map(|g| g.goal),
            None => None,
        };
        Ok(todo_item(row, fix))
    }

    /// The owner adds a to-do: under one of the app's goals, or at the end
    /// of its plan. Returns the to-do, and whether it is new rather than the
    /// answer to a resent receipt.
    pub async fn add_todo(&self, app_id: i64, t: &NewTodo) -> Result<(PlanItem, bool)> {
        let title = clean_title(&t.title)?;
        let detail = clean_detail(t.detail.as_deref())?;
        check_receipt(t.receipt.as_deref())?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(id) = receipt_result(&mut tx, t.receipt.as_deref(), "add_todo").await? {
            tx.commit().await?;
            return Ok((self.todo(id).await?, false));
        }
        app_exists(&mut tx, app_id).await?;
        if let Some(parent) = t.parent_id {
            let row: Option<(i64, String)> =
                sqlx::query_as("SELECT app_id, kind FROM plan_items WHERE id = ?")
                    .bind(parent)
                    .fetch_optional(&mut *tx)
                    .await?;
            if !matches!(row, Some((a, kind)) if a == app_id && kind == "goal") {
                return Err(bad(
                    "A to-do can only go under one of this app's goals. Pick a goal, or add it at the end.",
                ));
            }
        }
        let id = insert_todo(
            &mut tx,
            app_id,
            t.parent_id,
            &title,
            detail.as_deref(),
            ItemSource::Owner,
        )
        .await?;
        save_receipt(&mut tx, t.receipt.as_deref(), "add_todo", id).await?;
        tx.commit().await?;
        Ok((self.todo(id).await?, true))
    }

    /// The owner ticks, reopens, drops or rewords a to-do. Goal rows follow
    /// their goal and cannot be changed here.
    pub async fn edit_todo(&self, id: i64, e: &EditTodo) -> Result<PlanItem> {
        let title = e.title.as_deref().map(clean_title).transpose()?;
        // Some(None) clears the details.
        let detail = match e.detail.as_deref() {
            Some(d) => Some(clean_detail(Some(d))?),
            None => None,
        };
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let kind: String = sqlx::query_scalar("SELECT kind FROM plan_items WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::NotFound)?;
        if kind == "goal" {
            return Err(bad(
                "A goal is ticked off on its own when you close it. Open the goal to close or stop it.",
            ));
        }
        // In SQLite every SET reads the row as it was, so `state` below is
        // the old state.
        sqlx::query(
            "UPDATE plan_items SET
                 title = COALESCE(?1, title),
                 detail = CASE WHEN ?2 THEN ?3 ELSE detail END,
                 done_at = CASE WHEN ?4 IS NULL THEN done_at
                                WHEN ?4 = 'done' AND state = 'done' THEN done_at
                                WHEN ?4 = 'done' THEN strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                                ELSE NULL END,
                 state = COALESCE(?4, state),
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?5",
        )
        .bind(title)
        .bind(detail.is_some())
        .bind(detail.flatten())
        .bind(e.state.map(ItemState::as_str))
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.todo(id).await
    }

    /// Moves a row one place up or down among the rows beside it (same app,
    /// same parent), in one transaction. At either end nothing changes.
    /// Returns the plan.
    pub async fn move_item(&self, id: i64, direction: Direction) -> Result<PlanView> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let (app_id, parent, position): (i64, Option<i64>, i64) =
            sqlx::query_as("SELECT app_id, parent_id, position FROM plan_items WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::NotFound)?;
        let neighbour = match direction {
            Direction::Up => {
                "SELECT id, position FROM plan_items
                 WHERE app_id = ?1 AND parent_id IS ?2
                   AND (position < ?3 OR (position = ?3 AND id < ?4))
                 ORDER BY position DESC, id DESC LIMIT 1"
            }
            Direction::Down => {
                "SELECT id, position FROM plan_items
                 WHERE app_id = ?1 AND parent_id IS ?2
                   AND (position > ?3 OR (position = ?3 AND id > ?4))
                 ORDER BY position, id LIMIT 1"
            }
        };
        let other: Option<(i64, i64)> = sqlx::query_as(neighbour)
            .bind(app_id)
            .bind(parent)
            .bind(position)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        if let Some((other_id, other_pos)) = other {
            for (row, pos) in [(id, other_pos), (other_id, position)] {
                sqlx::query(
                    "UPDATE plan_items SET position = ?,
                         updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
                )
                .bind(pos)
                .bind(row)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        self.plan_view(app_id).await
    }

    /// Checks a fix request without changing anything, before its start is
    /// chosen (which may ask the app's git server): `Some` with the first
    /// answer to a resent receipt, an error for a request that would be
    /// refused, `None` for one ready to go. [`Store::fix_todos`] checks it
    /// all again in its own transaction.
    pub(crate) async fn fix_todos_first(
        &self,
        app_id: i64,
        f: &FixTodos,
    ) -> Result<Option<(Goal, bool)>> {
        let mut tx = self.pool().begin().await?; // read-only
        let checked = fix_checks(&mut tx, app_id, f).await;
        tx.rollback().await?;
        match checked? {
            FixChecked::Resent(id) => Ok(Some((self.goal(id).await?, false))),
            FixChecked::Ready { .. } => Ok(None),
        }
    }

    /// Starts one goal to do these to-do items, with the owner's note, and
    /// links each item to it, all in one transaction. Returns the goal and
    /// whether it is new rather than the answer to a resent receipt.
    /// `start_commit` was chosen by the caller before this, outside any
    /// transaction (see [`crate::coordinator::start_goal`]).
    pub async fn fix_todos(
        &self,
        app_id: i64,
        f: &FixTodos,
        start_commit: Option<&str>,
    ) -> Result<(Goal, bool)> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let (ids, items, found_in) = match fix_checks(&mut tx, app_id, f).await? {
            FixChecked::Resent(id) => {
                tx.commit().await?;
                return Ok((self.goal(id).await?, false));
            }
            FixChecked::Ready {
                ids,
                items,
                found_in,
            } => (ids, items, found_in),
        };
        let note = f.note.as_deref().map(str::trim).filter(|n| !n.is_empty());
        let text = fix_text(found_in.as_deref(), &items, note);
        let goal_id = crate::coordinator::start_goal(
            &mut tx,
            app_id,
            &fix_title(&items),
            &text,
            start_commit,
        )
        .await?;
        for id in &ids {
            sqlx::query(
                "UPDATE plan_items SET fix_goal_id = ?,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
            )
            .bind(goal_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        save_receipt(&mut tx, f.receipt.as_deref(), "fix_todos", goal_id).await?;
        tx.commit().await?;
        Ok((self.goal(goal_id).await?, true))
    }
}

/// What checking a fix request found.
enum FixChecked {
    /// A resent receipt: the goal the first one started.
    Resent(i64),
    Ready {
        ids: Vec<i64>,
        /// Each item's title and detail.
        items: Vec<(String, Option<String>)>,
        /// The goal they were all found in, if one.
        found_in: Option<String>,
    },
}

/// Every check of a fix request, inside the caller's transaction.
async fn fix_checks(
    tx: &mut Transaction<'_, Sqlite>,
    app_id: i64,
    f: &FixTodos,
) -> Result<FixChecked> {
    let mut ids: Vec<i64> = Vec::new();
    for id in &f.item_ids {
        if !ids.contains(id) {
            ids.push(*id);
        }
    }
    if ids.is_empty() {
        return Err(bad("Pick at least one to-do to fix."));
    }
    if ids.len() > MAX_FIX {
        return Err(bad(
            "Pick at most 10 to-dos at a time. Fix the rest in another goal.",
        ));
    }
    let note = f.note.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if note.is_some_and(|n| n.chars().count() > MAX_NOTE) {
        return Err(bad("Keep your note to 2000 characters or fewer."));
    }
    check_receipt(f.receipt.as_deref())?;
    if let Some(id) = receipt_result(tx, f.receipt.as_deref(), "fix_todos").await? {
        return Ok(FixChecked::Resent(id));
    }
    app_exists(tx, app_id).await?;
    let mut items = Vec::new();
    let mut parents = HashSet::new();
    for id in &ids {
        let row = sqlx::query(
            "SELECT p.app_id, p.kind, p.state, p.title, p.detail, p.parent_id,
                    g.state AS fix_state
             FROM plan_items p LEFT JOIN goals g ON g.id = p.fix_goal_id WHERE p.id = ?",
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
        let Some(row) = row.filter(|r| r.try_get::<i64, _>("app_id").ok() == Some(app_id)) else {
            return Err(bad(
                "One of those to-dos is not on this app's plan. Reload the plan and try again.",
            ));
        };
        let title: String = row.try_get("title")?;
        if row.try_get::<String, _>("kind")? != "todo" {
            return Err(bad("Pick to-do items to fix, not goals."));
        }
        if row.try_get::<String, _>("state")? != "open" {
            return Err(StoreError::Invalid(format!(
                "\"{}\" is already ticked off or dropped. Reopen it first.",
                clip(&title, 80)
            )));
        }
        // A fix goal that was stopped, or closed with the item reopened,
        // may be replaced; one still open may not.
        let fix_state: Option<String> = row.try_get("fix_state")?;
        if fix_state.is_some_and(|s| s != "done" && s != "stopped") {
            return Err(StoreError::Refused(format!(
                "\"{}\" is already being fixed in another goal.",
                clip(&title, 80)
            )));
        }
        parents.insert(row.try_get::<Option<i64>, _>("parent_id")?);
        items.push((title, row.try_get::<Option<String>, _>("detail")?));
    }
    // Named only when every item was found in the same goal.
    let found_in: Option<String> =
        match parents.into_iter().collect::<Vec<_>>().as_slice() {
            [Some(parent)] => sqlx::query_scalar(
                "SELECT g.title FROM plan_items p JOIN goals g ON g.id = p.goal_id WHERE p.id = ?",
            )
            .bind(parent)
            .fetch_optional(&mut **tx)
            .await?,
            _ => None,
        };
    Ok(FixChecked::Ready {
        ids,
        items,
        found_in,
    })
}

/// `GET /api/apps/{id}/plan`: the app's master plan.
pub(crate) async fn view(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(app_id): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("reading the plan");
    }
    match st.store.plan_view(app_id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// `POST /api/apps/{id}/plan/items` `NewTodo`: 201 with the new to-do, or
/// 200 with the first answer to a resent receipt.
pub(crate) async fn add_item(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(app_id): Path<i64>,
    Json(t): Json<NewTodo>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("adding to the plan");
    }
    match st.store.add_todo(app_id, &t).await {
        Ok((item, fresh)) => created_or_resent(item, fresh),
        Err(e) => ApiError(e).into_response(),
    }
}

/// `POST /api/plan/items/{id}` `EditTodo`: the changed to-do.
pub(crate) async fn edit_item(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    Json(e): Json<EditTodo>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("changing the plan");
    }
    match st.store.edit_todo(id, &e).await {
        Ok(item) => Json(item).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// `POST /api/plan/items/{id}/move` `MoveItem`: the whole plan, reordered.
pub(crate) async fn move_item(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    Json(m): Json<MoveItem>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("reordering the plan");
    }
    match st.store.move_item(id, m.direction).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// `POST /api/apps/{id}/plan/fix` `FixTodos`: 201 with the new goal, or 200
/// with the first answer to a resent receipt.
pub(crate) async fn fix(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(app_id): Path<i64>,
    Json(f): Json<FixTodos>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("starting a goal");
    }
    // Refused and resent requests are answered before anything waits on
    // the app's git server.
    match st.store.fix_todos_first(app_id, &f).await {
        Ok(Some((goal, fresh))) => return created_or_resent(goal, fresh),
        Ok(None) => {}
        Err(e) => return ApiError(e).into_response(),
    }
    let start = crate::upstream::for_new_goal(&st, app_id).await;
    match st.store.fix_todos(app_id, &f, start.as_deref()).await {
        Ok((goal, fresh)) => created_or_resent(goal, fresh),
        Err(e) => ApiError(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(id: i64, review: ReviewState, thread: ThreadState) -> PieceStep {
        PieceStep {
            thread_id: id,
            title: format!("Piece {id}"),
            review,
            thread,
        }
    }

    fn states(steps: &[PlanStep]) -> Vec<StepState> {
        steps.iter().map(|s| s.state).collect()
    }

    use StepState::*;

    #[test]
    fn steps_follow_a_goal_from_talking_to_done() {
        let talking = goal_steps(GoalState::Talking, false, &[]);
        let titles: Vec<&str> = talking.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Talk it through", "Approve the plan", "Your review"]
        );
        assert_eq!(states(&talking), [Now, Later, Later]);
        // Talking again after a plan the owner wanted changed.
        assert_eq!(
            states(&goal_steps(GoalState::Talking, true, &[])),
            [Now, Later, Later]
        );
        assert_eq!(
            states(&goal_steps(GoalState::PlanReady, true, &[])),
            [Done, You, Later]
        );
        let building = [piece(7, ReviewState::Building, ThreadState::Running)];
        let working = goal_steps(GoalState::Working, true, &building);
        assert_eq!(states(&working), [Done, Done, Now, Later]);
        assert_eq!(working[2].title, "Piece 7");
        assert_eq!(working[2].thread_id, Some(7));
        assert_eq!(working[0].thread_id, None);
        let passed = [piece(7, ReviewState::Passed, ThreadState::InReview)];
        assert_eq!(
            states(&goal_steps(GoalState::ForReview, true, &passed)),
            [Done, Done, Done, You]
        );
        assert_eq!(
            states(&goal_steps(GoalState::Done, true, &passed)),
            [Done, Done, Done, Done]
        );
    }

    #[test]
    fn each_piece_shows_where_it_is() {
        let pieces = [
            piece(1, ReviewState::Reviewing, ThreadState::InReview),
            piece(2, ReviewState::Fixing, ThreadState::Queued),
            piece(3, ReviewState::NeedsALook, ThreadState::InReview),
            piece(4, ReviewState::Failed, ThreadState::Failed),
            // Accepted by the owner before the checker was done.
            piece(5, ReviewState::Reviewing, ThreadState::Done),
        ];
        assert_eq!(
            states(&goal_steps(GoalState::Working, true, &pieces)),
            [Done, Done, Now, Now, You, Problem, Done, Later]
        );
    }

    #[test]
    fn an_app_that_deploys_ends_its_goals_with_live() {
        let base = || goal_steps(GoalState::Done, true, &[]);
        let last = |v: Vec<PlanStep>| v.last().map(|s| (s.title.clone(), s.state));
        assert_eq!(
            base(),
            with_live(base(), GoalState::Done, false, Some(false))
        );
        assert_eq!(
            last(with_live(base(), GoalState::Done, true, Some(false))),
            Some(("Merged · ready to deploy".into(), You))
        );
        assert_eq!(
            last(with_live(base(), GoalState::Done, true, Some(true))),
            Some(("Live".into(), Done))
        );
        assert_eq!(base(), with_live(base(), GoalState::Done, true, None));
        let open = goal_steps(GoalState::Working, true, &[]);
        assert_eq!(
            last(with_live(open.clone(), GoalState::Working, true, None)),
            Some(("Live".into(), Later))
        );
        let stopped = goal_steps(GoalState::Stopped, true, &[]);
        assert_eq!(
            stopped,
            with_live(stopped.clone(), GoalState::Stopped, true, None)
        );
    }

    #[test]
    fn a_stopped_goal_skips_what_it_never_did() {
        assert_eq!(
            states(&goal_steps(GoalState::Stopped, false, &[])),
            [Skipped, Skipped, Skipped]
        );
        assert_eq!(
            states(&goal_steps(GoalState::Stopped, true, &[])),
            [Done, Skipped, Skipped]
        );
        let pieces = [
            piece(1, ReviewState::Passed, ThreadState::InReview),
            piece(2, ReviewState::Failed, ThreadState::Failed),
        ];
        assert_eq!(
            states(&goal_steps(GoalState::Stopped, true, &pieces)),
            [Done, Done, Done, Skipped, Skipped]
        );
        // Closed as done while still talking: no plan was ever approved.
        assert_eq!(
            states(&goal_steps(GoalState::Done, false, &[])),
            [Done, Skipped, Done]
        );
    }

    #[test]
    fn fix_goals_say_where_the_items_were_found() {
        let items = vec![
            (
                "Check-in skips the insurance card".to_owned(),
                Some("The form never asks for it.".to_owned()),
            ),
            ("Visit notes save twice".to_owned(), None),
        ];
        let t = fix_text(
            Some("Audit the patient visit workflow"),
            &items,
            Some("Start with check-in."),
        );
        assert_eq!(
            t,
            "Please fix these, found while working on \"Audit the patient visit workflow\":\n\
             - Check-in skips the insurance card: The form never asks for it.\n\
             - Visit notes save twice\n\nStart with check-in."
        );
        assert!(fix_text(None, &items, None).starts_with("Please take care of these"));
        assert_eq!(
            fix_title(&items),
            "Check-in skips the insurance card (and 1 more)"
        );
        assert_eq!(fix_title(&items[1..]), "Visit notes save twice");
    }

    #[test]
    fn owner_words_are_checked_in_plain_words() {
        assert_eq!(
            clean_title("  Tidy the menu ").expect("fine"),
            "Tidy the menu"
        );
        assert!(clean_title(" ").is_err());
        assert!(clean_title(&"x".repeat(201)).is_err());
        assert_eq!(clean_detail(Some("  ")).expect("fine"), None);
        assert!(clean_detail(Some(&"x".repeat(1001))).is_err());
    }
}
