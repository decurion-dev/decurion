//! Removing an app from Decurion, so the owner can tidy the board.
//!
//! Removing only hides: it sets `removed_at` and renames the row to
//! `<name>~removed-<id>` so the name can be added again (`apps.name` is
//! unique, and `~` is never allowed in a real name). The app's work, goals,
//! audit rows and spend all stay, so money totals keep counting past spend.
//! The owner's folder, branches, pull requests and GitHub are never touched.
//!
//! An app with work or goals still going, an open deploy or a running
//! preview is refused, so nothing in flight loses its app. Every other
//! screen and route treats a removed app as not found (`store.rs`).
//!
//! The route is owner-only, carries a receipt, and is not in `cloud_reach`.

use crate::store::{check_receipt, receipt_result, save_receipt, Result, Store, StoreError};
use crate::{forbidden, ApiError, AppState, Caller};
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct Remove {
    #[serde(default)]
    pub receipt: Option<String>,
}

impl Store {
    /// Hides an app from every screen and frees its name. Refused while
    /// anything of the app is still going. With a receipt already used, or
    /// an app already removed, nothing changes.
    pub async fn remove_app(&self, app_id: i64, receipt: Option<&str>) -> Result<()> {
        check_receipt(receipt)?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(done) = receipt_result(&mut tx, receipt, "app_remove").await? {
            if done != app_id {
                return Err(StoreError::Duplicate(crate::store::RECEIPT_REUSED.into()));
            }
            tx.commit().await?;
            return Ok(());
        }
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT name, removed_at FROM apps WHERE id = ?")
                .bind(app_id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((name, removed)) = row else {
            return Err(StoreError::NotFound);
        };
        if removed.is_some() {
            tx.commit().await?;
            return Ok(());
        }
        let busy: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM threads WHERE app_id = ?1 AND state NOT IN ('done', 'failed'))
                 OR EXISTS (SELECT 1 FROM goals WHERE app_id = ?1 AND state NOT IN ('done', 'stopped'))
                 OR EXISTS (SELECT 1 FROM deploys WHERE app_id = ?1
                            AND state IN ('waiting', 'deploying', 'failed'))
                 OR EXISTS (SELECT 1 FROM browser_sessions WHERE app_id = ?1
                            AND kind = 'preview' AND state IN ('pending', 'live'))",
        )
        .bind(app_id)
        .fetch_one(&mut *tx)
        .await?;
        if busy {
            return Err(StoreError::Refused(format!(
                "{name} still has work going. Finish or stop it first."
            )));
        }
        sqlx::query(
            "UPDATE apps SET removed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                 name = name || '~removed-' || id
             WHERE id = ?",
        )
        .bind(app_id)
        .execute(&mut *tx)
        .await?;
        save_receipt(&mut tx, receipt, "app_remove", app_id).await?;
        tx.commit().await?;
        Ok(())
    }
}

/// The owner removes an app (`POST /api/apps/{id}/remove`). Owner-only.
pub async fn remove(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    body: Option<Json<Remove>>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("removing an app");
    }
    let receipt = body.and_then(|Json(b)| b.receipt);
    match st.store.remove_app(id, receipt.as_deref()).await {
        Ok(()) => {
            tracing::info!(app = id, "the owner removed an app");
            Json(serde_json::json!({ "removed": true })).into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}
