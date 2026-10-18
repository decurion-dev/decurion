//! The master plan and the page library: what each app's goals have done,
//! what is left, and every page agents drew, beside the goal and piece it
//! belongs to. Shapes and rules are in [`decurion_proto::plan`].
//!
//! Goal rows read their title, steps and state from the goal itself each
//! time, so the plan never falls behind the work. Only the owner changes the
//! plan by hand; the project lead can only add to-do items under the goal it
//! is working on, and they do nothing until the owner taps.

use crate::store::{Result, Store};
use crate::{forbidden, ApiError, AppState, Caller};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use axum::Json;
use decurion_proto::goal::PageKind;
use decurion_proto::plan::LibraryPage;
use serde::Deserialize;
use sqlx::Row;

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
