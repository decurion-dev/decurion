//! Messages from the owner to an agent. A message is stored first and handed
//! to the agent by the runner, which then marks it delivered, so a message
//! sent while the runner is down or between runs still reaches the agent.

use crate::store::{
    checked_move, insert_event, receipt_result, save_receipt, state_in, Result, Store, StoreError,
    OWNER, RECEIPT_REUSED,
};
use decurion_proto::{Live, Message, MessageKind, NewMessage, Place, RunEvent, ThreadState};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, SqliteConnection, Transaction};
use std::str::FromStr;

/// Longest message, in characters. Enough for a paragraph of direction; more
/// belongs in new work.
pub const MAX_CHARS: usize = 4000;

/// Most ids marked delivered in one call.
const MAX_IDS: usize = 500;

const COLS: &str = "id, thread_id, kind, text, at, delivered_at";

fn message_from(row: &SqliteRow) -> Result<Message> {
    let kind: String = row.try_get("kind")?;
    Ok(Message {
        id: row.try_get("id")?,
        thread_id: row.try_get("thread_id")?,
        kind: MessageKind::from_str(&kind).map_err(|e| StoreError::Invalid(e.to_string()))?,
        text: row.try_get("text")?,
        at: row.try_get("at")?,
        delivered_at: row.try_get("delivered_at")?,
    })
}

/// A thread's messages, oldest first; with `waiting`, only those the agent
/// does not have yet.
pub(crate) async fn list(
    conn: &mut SqliteConnection,
    thread_id: i64,
    waiting: bool,
) -> Result<Vec<Message>> {
    let only = if waiting {
        " AND delivered_at IS NULL"
    } else {
        ""
    };
    let rows = sqlx::query(&format!(
        "SELECT {COLS} FROM messages WHERE thread_id = ?{only} ORDER BY id"
    ))
    .bind(thread_id)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(message_from).collect()
}

/// Whether the agent still owes the owner something: a steer or follow-up
/// it has not been given. Notes never hold work back.
pub(crate) async fn owed(tx: &mut Transaction<'_, Sqlite>, thread_id: i64) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM messages WHERE thread_id = ? AND delivered_at IS NULL
                 AND kind IN ('steer', 'follow_up'))",
    )
    .bind(thread_id)
    .fetch_one(&mut **tx)
    .await?)
}

impl Store {
    /// Stores the owner's message and logs it with the run's events. A steer
    /// or follow-up for work that is waiting on the owner, in review or
    /// failed sends it back to the queue, since the owner has said what to do
    /// next. With a receipt already used for a message, nothing is stored and
    /// the first message is returned; the flag is false then.
    pub async fn add_message(&self, thread_id: i64, m: &NewMessage) -> Result<(Message, bool)> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let out = add_message_in(&mut tx, thread_id, m, OWNER, "sent back with a message").await?;
        tx.commit().await?;
        Ok(out)
    }

    /// Every message of a thread, oldest first.
    pub async fn messages(&self, thread_id: i64) -> Result<Vec<Message>> {
        let mut tx = self.pool().begin().await?; // read-only: a reader never waits on writers
        state_in(&mut tx, thread_id).await?;
        let all = list(&mut tx, thread_id, false).await?;
        tx.commit().await?;
        Ok(all)
    }

    /// What the runner checks during a run, read together so a message is
    /// never seen against a state it was not sent in.
    pub async fn live(&self, thread_id: i64) -> Result<Live> {
        let mut tx = self.pool().begin().await?; // read-only: a reader never waits on writers
        let state = state_in(&mut tx, thread_id).await?;
        let place: String = sqlx::query_scalar("SELECT place FROM threads WHERE id = ?")
            .bind(thread_id)
            .fetch_one(&mut *tx)
            .await?;
        let place = Place::from_str(&place).map_err(|e| StoreError::Invalid(e.to_string()))?;
        let messages = list(&mut tx, thread_id, true).await?;
        tx.commit().await?;
        Ok(Live {
            state,
            place,
            messages,
            // Filled in by the hub's live endpoint, which knows who is watching.
            browser: None,
        })
    }

    /// Marks messages as handed to the agent. Ids that are not this thread's
    /// or were already marked are skipped, so a retried call is harmless.
    pub async fn mark_delivered(&self, thread_id: i64, ids: &[i64]) -> Result<()> {
        if ids.len() > MAX_IDS {
            return Err(StoreError::Invalid(format!(
                "at most {MAX_IDS} messages at a time"
            )));
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        state_in(&mut tx, thread_id).await?;
        for id in ids {
            sqlx::query(
                "UPDATE messages SET delivered_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE id = ? AND thread_id = ? AND delivered_at IS NULL",
            )
            .bind(id)
            .bind(thread_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

/// [`Store::add_message`] inside the caller's transaction, sent by `actor`
/// (the owner, or the coordinator passing on a checker's notes); `why` goes
/// in the audit log if it sends the work back.
pub(crate) async fn add_message_in(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
    m: &NewMessage,
    actor: &str,
    why: &str,
) -> Result<(Message, bool)> {
    let text = m.text.trim();
    if !(1..=MAX_CHARS).contains(&text.chars().count()) {
        return Err(StoreError::Invalid(format!(
            "a message must be 1 to {MAX_CHARS} characters"
        )));
    }
    crate::store::check_receipt(m.receipt.as_deref())?;
    if let Some(id) = receipt_result(tx, m.receipt.as_deref(), "message").await? {
        let row = sqlx::query(&format!("SELECT {COLS} FROM messages WHERE id = ?"))
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
        let first = message_from(&row)?;
        if first.thread_id != thread_id {
            return Err(StoreError::Duplicate(RECEIPT_REUSED.into()));
        }
        return Ok((first, false));
    }
    let state = state_in(tx, thread_id).await?;
    if state == ThreadState::Done {
        return Err(StoreError::Refused(
            "this work is finished; add new work instead".into(),
        ));
    }
    let row = sqlx::query(&format!(
        "INSERT INTO messages (thread_id, kind, text) VALUES (?, ?, ?) RETURNING {COLS}"
    ))
    .bind(thread_id)
    .bind(m.kind.as_str())
    .bind(text)
    .fetch_one(&mut **tx)
    .await?;
    let msg = message_from(&row)?;
    let logged = RunEvent::Owner {
        via: m.kind,
        text: text.to_owned(),
    };
    insert_event(tx, thread_id, &logged).await?;
    let sends_back = matches!(
        state,
        ThreadState::NeedsYou | ThreadState::InReview | ThreadState::Failed
    );
    if m.kind != MessageKind::Note && sends_back {
        checked_move(
            tx,
            thread_id,
            Some(state),
            ThreadState::Queued,
            actor,
            Some(why),
        )
        .await?;
    }
    save_receipt(tx, m.receipt.as_deref(), "message", msg.id).await?;
    Ok((msg, true))
}
