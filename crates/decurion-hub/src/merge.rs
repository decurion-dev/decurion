//! Merging a pull request on the owner's Merge tap.
//!
//! Decurion merges nothing by itself. For an app whose rules allow it
//! ([`decurion_proto::AppRules::merge`], set only by the owner), a pull
//! request Decurion opened gets a Merge button, and one tap merges it on
//! GitHub. That tap is the stop the hard rules ask for (PLAN.md D7, AGENTS.md
//! walls 6 and 8): one tap, one pull request. Rules that must stay true:
//!
//! - **Only the owner's tap.** The route is owner-only, takes a receipt (a
//!   resent tap merges nothing twice) and is not in `cloud_reach`. No agent,
//!   checker, coordinator, runner key, timer or voice answer can reach it,
//!   and nothing else in Decurion asks GitHub to merge.
//! - **Only where the rules allow.** The app's rule is checked here, in the
//!   hub, at the tap, not only by hiding the button.
//! - **Only Decurion's own pull requests**, still open, and only the commit
//!   Decurion last pushed to them: GitHub is asked to merge that exact
//!   commit (`sha`), so anything pushed there since makes it refuse.
//! - **Only when GitHub says it is ready.** Its checks passed, its reviews
//!   are in and nothing conflicts (`mergeable_state` "clean"). Anything else
//!   is refused with plain words, and the owner can still merge on GitHub.
//! - **The token stays in the request header**, as for every GitHub call.

use crate::github::{Repo, Sender, Token};
use crate::pr_watch::Watched;
use crate::store::{check_receipt, receipt_result, save_receipt, Result, Store, StoreError};
use crate::{forbidden, ApiError, AppState, Caller};
use axum::extract::{Path as UrlPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use sqlx::Row;

/// A watched pull request, as the Merge tap needs it.
struct ToMerge {
    app: String,
    can_merge: bool,
    state: String,
    repo: Repo,
    number: i64,
    /// The commit Decurion last pushed to it.
    head: String,
}

/// What GitHub says about a pull request just before merging.
#[derive(Debug, PartialEq, Eq)]
pub enum Ready {
    Yes,
    AlreadyMerged,
    Closed,
    /// Someone pushed to it since Decurion did.
    Moved,
    /// Not ready; the words say why and what to do.
    No(String),
}

/// Reads GitHub's answer about one pull request against the commit Decurion
/// pushed. Pure, so every refusal can be tested.
pub fn ready(pr: &serde_json::Value, head: &str) -> Ready {
    if pr["merged"].as_bool() == Some(true) || pr["merged_at"].is_string() {
        return Ready::AlreadyMerged;
    }
    if pr["state"].as_str() == Some("closed") {
        return Ready::Closed;
    }
    if pr["head"]["sha"].as_str() != Some(head) {
        return Ready::Moved;
    }
    if pr["draft"].as_bool() == Some(true) {
        return Ready::No(
            "It is still a draft on GitHub. Mark it ready for review there first.".into(),
        );
    }
    let words = match pr["mergeable_state"].as_str() {
        Some("clean") | Some("has_hooks") if pr["mergeable"].as_bool() == Some(true) => {
            return Ready::Yes
        }
        Some("unstable") => "Some of its checks on GitHub failed or are still running. Wait for them to pass, or merge it on GitHub if you're sure.",
        Some("blocked") => "GitHub says it isn't ready yet: a review or a required check is still missing.",
        Some("dirty") => "It conflicts with newer changes on the main branch. Fix the conflict first, then tap Merge again.",
        Some("behind") => "It needs the newest main branch first. Update the branch on GitHub, then tap Merge again.",
        Some("draft") => "It is still a draft on GitHub. Mark it ready for review there first.",
        _ => "GitHub is still checking it. Try again in a minute.",
    };
    Ready::No(words.into())
}

/// The way of merging this repo allows, preferring one commit per pull
/// request. GitHub leaves the settings out for tokens that cannot see
/// them; squash is then tried.
pub fn merge_method(repo: &serde_json::Value) -> &'static str {
    let allowed = |k: &str| repo[k].as_bool() != Some(false);
    if allowed("allow_squash_merge") {
        "squash"
    } else if allowed("allow_merge_commit") {
        "merge"
    } else {
        "rebase"
    }
}

fn refused(words: impl Into<String>) -> StoreError {
    StoreError::Refused(words.into())
}

fn merge_refused(status: reqwest::StatusCode) -> StoreError {
    refused(match status.as_u16() {
        401 => "GitHub did not accept the saved token. Save a new one in Settings, GitHub, or merge it on GitHub.".to_owned(),
        403 | 404 => "Your GitHub token may not merge on this repo. Give it Contents: Read and write for this repo in GitHub's token settings, or merge it on GitHub.".to_owned(),
        405 => "GitHub would not merge it yet. Open it on GitHub to see what it waits for.".to_owned(),
        409 => "Someone pushed to this pull request since Decurion did. Look at it on GitHub and merge it there.".to_owned(),
        n => format!("GitHub said {n} when asked to merge. Try again, or merge it on GitHub."),
    })
}

async fn get(
    s: &Sender,
    t: &Token,
    path: &str,
) -> std::result::Result<serde_json::Value, StoreError> {
    let resp =
        s.gh.req(reqwest::Method::GET, path, t)
            .send()
            .await
            .map_err(|_| refused("Could not reach GitHub. Try again in a minute."))?;
    if !resp.status().is_success() {
        return Err(merge_refused(resp.status()));
    }
    resp.json()
        .await
        .map_err(|_| refused("GitHub's answer could not be read. Try again in a minute."))
}

impl Store {
    async fn to_merge(&self, thread_id: i64) -> Result<ToMerge> {
        let r = sqlx::query(
            "SELECT a.name AS app, a.can_merge, w.state, w.owner, w.repo, w.number, w.head
             FROM pr_watches w JOIN apps a ON a.id = w.app_id WHERE w.thread_id = ?",
        )
        .bind(thread_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::NotFound)?;
        Ok(ToMerge {
            app: r.try_get("app")?,
            can_merge: r.try_get("can_merge")?,
            state: r.try_get("state")?,
            repo: Repo {
                owner: r.try_get("owner")?,
                name: r.try_get("repo")?,
            },
            number: r.try_get("number")?,
            head: r.try_get("head")?,
        })
    }

    /// Whether this receipt already merged something, and what.
    async fn merge_receipt(&self, receipt: Option<&str>) -> Result<Option<i64>> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let done = receipt_result(&mut tx, receipt, "pr_merge").await?;
        tx.commit().await?;
        Ok(done)
    }

    /// Records a merge (Decurion's own, or one GitHub already had) and the
    /// receipt that asked for it, in one transaction. Returns whether the
    /// watch was still open, so the merge's follow-ups run once.
    async fn record_merge(
        &self,
        thread_id: i64,
        event: &str,
        receipt: Option<&str>,
    ) -> Result<bool> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let done = sqlx::query(
            "UPDATE pr_watches SET state = 'merged', event = ?,
                 event_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                 checked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE thread_id = ? AND state = 'open'",
        )
        .bind(event)
        .bind(thread_id)
        .execute(&mut *tx)
        .await?;
        if receipt_result(&mut tx, receipt, "pr_merge")
            .await?
            .is_none()
        {
            save_receipt(&mut tx, receipt, "pr_merge", thread_id).await?;
        }
        tx.commit().await?;
        Ok(done.rows_affected() == 1)
    }

    async fn record_closed(&self, thread_id: i64) -> Result<()> {
        sqlx::query(
            "UPDATE pr_watches SET state = 'closed', event = 'Closed on GitHub without merging',
                 event_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE thread_id = ? AND state = 'open'",
        )
        .bind(thread_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The watched pull request opened by `thread_id`, as the board sees it.
    async fn watched_one(&self, thread_id: i64) -> Result<Watched> {
        self.watched_prs()
            .await?
            .into_iter()
            .find(|w| w.thread_id == thread_id)
            .ok_or(StoreError::NotFound)
    }
}

/// The owner's Merge tap on the pull request that `thread_id` opened.
/// Returns the pull request as the board now sees it.
pub async fn merge(s: &Sender, thread_id: i64, receipt: Option<&str>) -> Result<Watched> {
    check_receipt(receipt)?;
    if let Some(done) = s.store.merge_receipt(receipt).await? {
        if done != thread_id {
            return Err(StoreError::Duplicate(crate::store::RECEIPT_REUSED.into()));
        }
        return s.store.watched_one(thread_id).await;
    }
    let m = s.store.to_merge(thread_id).await?;
    if !m.can_merge {
        return Err(refused(format!(
            "Your rules for {} say people merge its pull requests on GitHub. Merge it there, or change the rule on {}'s page in Settings.",
            m.app, m.app
        )));
    }
    match m.state.as_str() {
        "merged" => return s.store.watched_one(thread_id).await,
        "closed" => {
            return Err(refused(
                "This pull request was closed without merging, so there is nothing to merge.",
            ))
        }
        _ => {}
    }
    let Some(t) = s.gh.tokens.get().await else {
        return Err(refused(
            "Decurion needs your GitHub token to merge. Add one in Settings, GitHub, or merge it on GitHub.",
        ));
    };
    let base = format!("/repos/{}/{}", m.repo.owner, m.repo.name);
    let pr = get(s, &t, &format!("{base}/pulls/{}", m.number)).await?;
    match ready(&pr, &m.head) {
        Ready::Yes => {}
        Ready::AlreadyMerged => {
            if s.store
                .record_merge(thread_id, "Merged on GitHub", receipt)
                .await?
            {
                crate::pr_watch::merged(s, thread_id).await;
            }
            return s.store.watched_one(thread_id).await;
        }
        Ready::Closed => {
            s.store.record_closed(thread_id).await?;
            return Err(refused(
                "This pull request was closed on GitHub without merging, so there is nothing to merge.",
            ));
        }
        Ready::Moved => return Err(merge_refused(reqwest::StatusCode::CONFLICT)),
        Ready::No(words) => return Err(refused(words)),
    }
    let repo = get(s, &t, &base).await?;
    let resp =
        s.gh.req(
            reqwest::Method::PUT,
            &format!("{base}/pulls/{}/merge", m.number),
            &t,
        )
        .json(&serde_json::json!({
            "sha": m.head,
            "merge_method": merge_method(&repo),
        }))
        .send()
        .await
        .map_err(|_| refused("Could not reach GitHub to merge. Try again in a minute."))?;
    if !resp.status().is_success() {
        return Err(merge_refused(resp.status()));
    }
    tracing::info!(
        thread = thread_id,
        number = m.number,
        "the owner merged a pull request from Decurion"
    );
    if s.store
        .record_merge(thread_id, "Merged from Decurion", receipt)
        .await?
    {
        crate::pr_watch::merged(s, thread_id).await;
    }
    s.store.watched_one(thread_id).await
}

#[derive(Deserialize)]
pub(crate) struct MergeTap {
    #[serde(default)]
    receipt: Option<String>,
}

/// The owner's Merge tap. Owner only, and not in `cloud_reach`.
pub(crate) async fn tap(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    UrlPath(id): UrlPath<i64>,
    Json(b): Json<MergeTap>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("merging a pull request");
    }
    let Some(s) = st.sender() else {
        return ApiError(refused(
            "This hub cannot reach GitHub, so merge it on GitHub instead.",
        ))
        .into_response();
    };
    match merge(&s, id, b.receipt.as_deref()).await {
        Ok(w) => Json(w).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

    fn pr(state: &str, mergeable: bool) -> serde_json::Value {
        json!({ "state": "open", "merged": false, "head": { "sha": HEAD },
            "mergeable": mergeable, "mergeable_state": state })
    }

    #[test]
    fn only_a_clean_pull_request_at_decurions_commit_is_merged() {
        assert_eq!(ready(&pr("clean", true), HEAD), Ready::Yes);
        assert_eq!(ready(&pr("has_hooks", true), HEAD), Ready::Yes);
        // GitHub still working it out.
        assert!(matches!(ready(&pr("clean", false), HEAD), Ready::No(_)));
        assert!(matches!(ready(&pr("unknown", false), HEAD), Ready::No(_)));
        for (state, says) in [
            ("unstable", "checks"),
            ("blocked", "review"),
            ("dirty", "conflicts"),
            ("behind", "newest main"),
            ("draft", "draft"),
        ] {
            match ready(&pr(state, true), HEAD) {
                Ready::No(w) => {
                    assert!(w.contains(says), "{state}: {w}");
                    assert!(!w.contains('\u{2014}'), "no long dashes: {w}");
                }
                other => panic!("{state} must not merge: {other:?}"),
            }
        }
        // Someone else pushed since Decurion did.
        assert_eq!(ready(&pr("clean", true), "fff"), Ready::Moved);
        let mut done = pr("clean", true);
        done["merged"] = json!(true);
        assert_eq!(ready(&done, HEAD), Ready::AlreadyMerged);
        let mut closed = pr("clean", true);
        closed["state"] = json!("closed");
        assert_eq!(ready(&closed, HEAD), Ready::Closed);
    }

    #[test]
    fn the_merge_keeps_to_what_the_repo_allows() {
        assert_eq!(merge_method(&json!({})), "squash");
        assert_eq!(merge_method(&json!({"allow_squash_merge": false})), "merge");
        assert_eq!(
            merge_method(&json!({"allow_squash_merge": false, "allow_merge_commit": false})),
            "rebase"
        );
    }
}
