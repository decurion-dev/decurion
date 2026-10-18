//! Dismissing finished work and closed goals, so they stop lingering on the
//! board, the phone and the goal list.
//!
//! Dismissing only hides: it sets `dismissed_at` and nothing else. The work,
//! its branch, its pull request and its history all stay, and every screen
//! keeps a Dismissed view where one tap brings an item back. Only finished
//! things can be dismissed (work that is Done or Failed, goals that are Done
//! or Stopped), so nothing that still needs the owner can be swept away.
//! Work that moves again, such as a retry of failed work, shows again: every
//! state move clears the mark (`checked_move` in `store.rs`).
//!
//! The taps carry no receipt: hiding something already hidden, or bringing
//! back something already shown, changes nothing, so a resend is harmless.
//! Both routes are owner-only and not in `cloud_reach`.

use crate::store::{Result, Store, StoreError};
use crate::{forbidden, ApiError, AppState, Caller};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;

/// The most items one tap may dismiss: "Dismiss all" on a long Done column.
pub const MAX_AT_ONCE: usize = 500;

#[derive(Debug, Deserialize)]
pub struct Dismiss {
    /// The work (or goals) to hide or bring back.
    pub ids: Vec<i64>,
    /// `true` hides them; `false` brings them back.
    #[serde(default = "yes")]
    pub dismissed: bool,
}

fn yes() -> bool {
    true
}

/// Which table, and which of its states count as finished.
#[derive(Clone, Copy)]
enum Kind {
    Work,
    Goals,
}

impl Kind {
    fn table(self) -> &'static str {
        match self {
            Kind::Work => "threads",
            Kind::Goals => "goals",
        }
    }

    fn finished(self, state: &str) -> bool {
        match self {
            Kind::Work => matches!(state, "done" | "failed"),
            Kind::Goals => matches!(state, "done" | "stopped"),
        }
    }

    fn not_finished(self) -> &'static str {
        match self {
            Kind::Work => {
                "Only finished work can be dismissed. This work is still going, so it stays on the board."
            }
            Kind::Goals => {
                "Only finished goals can be dismissed. This goal is still going, so it stays on the list."
            }
        }
    }
}

impl Store {
    /// Hides finished work from every screen, or brings it back. All or
    /// nothing: if one item is missing or still going, none change.
    pub async fn dismiss_work(&self, ids: &[i64], dismissed: bool) -> Result<()> {
        self.dismiss(Kind::Work, ids, dismissed).await
    }

    /// Hides closed goals from the goal list, or brings them back.
    pub async fn dismiss_goals(&self, ids: &[i64], dismissed: bool) -> Result<()> {
        self.dismiss(Kind::Goals, ids, dismissed).await
    }

    async fn dismiss(&self, kind: Kind, ids: &[i64], dismissed: bool) -> Result<()> {
        if ids.is_empty() {
            return Err(StoreError::Invalid(
                "Pick something to dismiss first.".into(),
            ));
        }
        if ids.len() > MAX_AT_ONCE {
            return Err(StoreError::Invalid(format!(
                "That is too many at once. Dismiss at most {MAX_AT_ONCE} at a time."
            )));
        }
        let table = kind.table();
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        for &id in ids {
            let state: String =
                sqlx::query_scalar(&format!("SELECT state FROM {table} WHERE id = ?"))
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or(StoreError::NotFound)?;
            // Bringing back is always allowed: it only shows what was hidden.
            if dismissed && !kind.finished(&state) {
                return Err(StoreError::Refused(kind.not_finished().into()));
            }
            // An item dismissed twice keeps the time it was first dismissed.
            sqlx::query(&format!(
                "UPDATE {table} SET dismissed_at = CASE WHEN ?
                     THEN COALESCE(dismissed_at, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                     ELSE NULL END
                 WHERE id = ?"
            ))
            .bind(dismissed)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

/// `POST /api/threads/dismiss`: hide finished work, or bring it back.
pub(crate) async fn work(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Json(d): Json<Dismiss>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("dismissing work");
    }
    match st.store.dismiss_work(&d.ids, d.dismissed).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// `POST /api/goals/dismiss`: hide closed goals, or bring them back.
pub(crate) async fn goals(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Json(d): Json<Dismiss>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("dismissing goals");
    }
    match st.store.dismiss_goals(&d.ids, d.dismissed).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}
