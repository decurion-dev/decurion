//! API handlers for goals and the helper agents. The logic lives in
//! [`crate::coordinator`]; these only check who is calling.

use crate::coordinator::News;
use crate::protected::Hold;
use crate::workers::RunnerId;
use crate::{forbidden, ApiError, AppState, Caller};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use decurion_proto::goal::{
    ApprovePlan, HelperResult, NewGoal, OwnerAnswers, OwnerSays, ResearchReport,
};
use serde::Deserialize;

/// Pages agents drew are shown as-is but can run nothing and load nothing:
/// `sandbox` without `allow-scripts` also gives them an origin of their own,
/// so they cannot reach the board or its cookie. Only the hub's own screens
/// may frame them, so the owner can answer a page's questions beside it.
const PAGE_CSP: &str = "sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; form-action 'none'; frame-ancestors 'self'";

fn owner_only(c: Caller, what: &str) -> Option<Response> {
    (c != Caller::Owner).then(|| forbidden(what))
}

fn runner_only(c: Caller) -> Option<Response> {
    (c != Caller::Runner).then(|| forbidden("runner calls are runner-only; this"))
}

fn tell(st: &AppState, news: Option<News>) {
    if let (Some(p), Some(n)) = (st.push.clone(), news) {
        let store = st.store.clone();
        tokio::spawn(async move { p.send(&store, &n).await });
    }
}

fn reply<T: serde::Serialize>(r: crate::store::Result<T>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

pub(crate) async fn list(State(st): State<AppState>) -> Response {
    reply(st.store.goals().await)
}

pub(crate) async fn add(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Json(g): Json<NewGoal>,
) -> Response {
    if let Some(r) = owner_only(c, "starting a goal") {
        return r;
    }
    match st.store.add_goal(&g).await {
        Ok(g) => (StatusCode::CREATED, Json(g)).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

pub(crate) async fn view(State(st): State<AppState>, Path(id): Path<i64>) -> Response {
    reply(st.store.goal_view(id).await)
}

pub(crate) async fn say(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    Json(b): Json<OwnerSays>,
) -> Response {
    if let Some(r) = owner_only(c, "talking to the coordinator") {
        return r;
    }
    reply(st.store.goal_say(id, &b.text).await)
}

pub(crate) async fn answer(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    Json(b): Json<OwnerAnswers>,
) -> Response {
    if let Some(r) = owner_only(c, "answering the coordinator") {
        return r;
    }
    reply(st.store.goal_answer(id, &b).await)
}

pub(crate) async fn approve(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    Json(b): Json<ApprovePlan>,
) -> Response {
    if let Some(r) = owner_only(c, "approving a plan") {
        return r;
    }
    reply(st.store.approve_plan(id, b.plan_message_id).await)
}

pub(crate) async fn retry(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if let Some(r) = owner_only(c, "retrying the coordinator") {
        return r;
    }
    reply(st.store.retry_goal(id).await)
}

pub(crate) async fn close(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if let Some(r) = owner_only(c, "closing a goal") {
        return r;
    }
    reply(st.store.close_goal(id).await)
}

pub(crate) async fn stop(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if let Some(r) = owner_only(c, "stopping a goal") {
        return r;
    }
    reply(st.store.stop_goal(id).await)
}

#[derive(Deserialize)]
pub(crate) struct PageQuery {
    /// Set by the phone, which opens the page in place of the goal and so
    /// needs a way back to it.
    back: Option<String>,
}

/// The phone's way back from a page an agent drew. The page can run no
/// script, so this is a plain link with fixed words that points only at the
/// goal the page belongs to and loads nothing; the page's CSP stays as it is.
/// It goes after the agent's HTML, where the browser still puts it in the
/// page's body, and its inline `!important` style wins over the page's own.
fn back_link(goal: i64) -> String {
    format!(
        "<a href=\"/goals#goal-{goal}\" style=\"all:initial !important;\
         position:fixed !important;z-index:2147483647 !important;left:16px !important;\
         bottom:calc(16px + env(safe-area-inset-bottom)) !important;\
         display:inline-flex !important;align-items:center !important;\
         min-height:48px !important;padding:0 20px !important;border-radius:999px !important;\
         background:#2553d6 !important;color:#fff !important;\
         font:600 16px/1 system-ui,-apple-system,'Segoe UI',Roboto,sans-serif !important;\
         text-decoration:none !important;cursor:pointer !important;\
         box-shadow:0 6px 20px rgba(0,0,0,.28) !important\">&lsaquo;&nbsp;Back to the goal</a>"
    )
}

/// A page an agent drew, for the owner only.
pub(crate) async fn page(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((goal, page)): Path<(i64, i64)>,
    Query(q): Query<PageQuery>,
) -> Response {
    if let Some(r) = owner_only(c, "opening a plan page") {
        return r;
    }
    let html = match st.store.page_html(goal, page).await {
        // Where the page marked its questions, the hub's own note goes.
        Ok(html) => match st.store.questions_of(page).await {
            Ok(qs) => Ok(crate::pages::mark_questions(&html, &qs)),
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    };
    sandboxed(goal, html, &q)
}

/// An agent's page of goal `goal`, as the owner sees it: in the sandbox, with
/// the phone's way back when asked for. Every page an agent wrote goes out
/// through here, so they all get the same headers.
fn sandboxed(goal: i64, html: crate::store::Result<String>, q: &PageQuery) -> Response {
    match html {
        Ok(mut html) => {
            if q.back.is_some() {
                html.push_str(&back_link(goal));
            }
            (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CONTENT_SECURITY_POLICY, PAGE_CSP),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                    (header::CACHE_CONTROL, "no-store"),
                    (header::REFERRER_POLICY, "no-referrer"),
                ],
                html,
            )
                .into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}

/// A research piece's report, for the owner only, in the same sandbox as the
/// other pages agents write.
pub(crate) async fn report(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((goal, thread)): Path<(i64, i64)>,
    Query(q): Query<PageQuery>,
) -> Response {
    if let Some(r) = owner_only(c, "opening a research report") {
        return r;
    }
    sandboxed(goal, st.store.report_html(goal, thread).await, &q)
}

/// The runner hands in a research piece's report before it moves the work
/// to review. Only the runner running that piece may.
pub(crate) async fn save_report(
    State(st): State<AppState>,
    runner: Option<Extension<RunnerId>>,
    Path(id): Path<i64>,
    Json(report): Json<ResearchReport>,
) -> Response {
    let Some(Extension(r)) = runner else {
        return forbidden("runner calls are runner-only; this");
    };
    match st.store.save_report(id, &r.name, &report).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

pub(crate) async fn notes(State(st): State<AppState>, Path(id): Path<i64>) -> Response {
    reply(st.store.app_notes(id).await)
}

pub(crate) async fn remove_note(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((app, note)): Path<(i64, i64)>,
) -> Response {
    if let Some(r) = owner_only(c, "removing a note") {
        return r;
    }
    match st.store.remove_app_note(app, note).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct RunnerName {
    runner: String,
}

pub(crate) async fn helper_claim(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Json(r): Json<RunnerName>,
) -> Response {
    if let Some(r) = runner_only(c) {
        return r;
    }
    match st.store.claim_helper(&r.runner).await {
        Ok(out) => {
            // One alert, however many goals it stopped.
            tell(&st, out.news.into_iter().next());
            match out.work {
                Some(claim) => Json(claim).into_response(),
                None => StatusCode::NO_CONTENT.into_response(),
            }
        }
        Err(e) => ApiError(e).into_response(),
    }
}

pub(crate) async fn helper_started(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if let Some(r) = runner_only(c) {
        return r;
    }
    match st.store.helper_started(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

pub(crate) async fn helper_finish(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    Json(res): Json<HelperResult>,
) -> Response {
    if let Some(r) = runner_only(c) {
        return r;
    }
    // Before the answer is recorded, and outside its transaction: this runs
    // git. A hub without a saved-work folder has no copy of any code to look
    // at; the real hub always has one.
    let hold = match &st.files {
        Some(f) => crate::protected::hold_for(&st.store, f, id, &res).await,
        None => Hold::Clear,
    };
    match st.store.finish_helper(id, &res, &hold).await {
        Ok(news) => {
            tell(&st, news);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}
