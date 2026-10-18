//! Questions inside pages agents drew: a designer's visual plan or a
//! checker's "as built" page may ask the owner yes or no, a choice, or
//! something in words, and the owner answers right beside the page.
//!
//! Pages still run no scripts and hold no forms. The hub draws the answer
//! buttons around the page, never inside it; inside, it only swaps each
//! marker the agent left (`<div data-question="q1"></div>`) for an inert
//! note made by the hub, with the question's text escaped. The answers are
//! checked against the questions actually asked, recorded once, and reach the
//! project lead as an owner message in the same transaction, exactly as if
//! the owner had typed them in the chat.
//!
//! Bad or missing questions never cost the owner a page: they are dropped
//! and the page is shown without them.

use crate::store::{check_receipt, receipt_result, save_receipt, Result, Store, StoreError};
use crate::{forbidden, ApiError, AppState, Caller};
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use decurion_proto::goal::{
    last_json_object, Answer, GoalMessageBody, GoalState, Question, ReviewVerdict,
};
use decurion_proto::plan::{validate_page_questions, PageAnswers, PageAsks, PageQuestions};
use sqlx::{Row, Sqlite, Transaction};
use std::fmt::Write as _;
use std::str::FromStr;

/// Longest page title kept, in characters.
const MAX_TITLE_CHARS: usize = 120;
/// Longest answer in words kept, in characters.
const MAX_WORDS: usize = 2000;
/// The receipt action for answering a page.
const ANSWER_ACTION: &str = "page_answer";

/// Finds `needle` (ASCII) in `hay` from byte `from`, ignoring ASCII case.
fn find_ci(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (from..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// Turns the few entities a title is likely to hold back into characters.
/// Anything else is left as written.
fn decode_entities(s: &str) -> String {
    const ENTITIES: [(&str, char); 5] = [
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&#39;", '\''),
    ];
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        match ENTITIES.iter().find(|(e, _)| rest.starts_with(e)) {
            Some((e, c)) => {
                out.push(*c);
                rest = &rest[e.len()..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The page's own name: the text of its first `<title>`, entities decoded,
/// spaces collapsed, at most 120 characters. `None` when it has none.
pub fn page_title(html: &str) -> Option<String> {
    let mut from = 0;
    let start = loop {
        let i = find_ci(html, "<title", from)?;
        let after = i + "<title".len();
        match html.as_bytes().get(after) {
            Some(b'>') => break after + 1,
            Some(b) if b.is_ascii_whitespace() => break after + html[after..].find('>')? + 1,
            // `<titles>` or the like: keep looking.
            _ => from = after,
        }
    };
    let end = find_ci(html, "</title", start)?;
    let text = decode_entities(&html[start..end]);
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text: String = text.chars().take(MAX_TITLE_CHARS).collect();
    let text = text.trim_end();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Escapes text for HTML, attributes included.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Reads a question marker starting at `at` (which points at `<div`):
/// `<div data-question="ID"></div>`, single or double quotes, spaces allowed
/// inside the tags, nothing between them but spaces. Returns the id and the
/// byte just past the marker.
fn read_marker(html: &str, at: usize) -> Option<(&str, usize)> {
    let b = html.as_bytes();
    let skip_ws = |mut i: usize| {
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        i
    };
    let expect_ci = |i: usize, word: &str| {
        let end = i + word.len();
        (b.get(i..end)?.eq_ignore_ascii_case(word.as_bytes())).then_some(end)
    };
    let mut i = expect_ci(at, "<div")?;
    let after_name = skip_ws(i);
    if after_name == i {
        return None;
    }
    i = expect_ci(after_name, "data-question")?;
    i = skip_ws(i);
    if b.get(i) != Some(&b'=') {
        return None;
    }
    i = skip_ws(i + 1);
    let quote = *b.get(i).filter(|q| **q == b'"' || **q == b'\'')?;
    let id_start = i + 1;
    let id_len = html[id_start..].find(quote as char)?;
    let id = &html[id_start..id_start + id_len];
    i = skip_ws(id_start + id_len + 1);
    if b.get(i) != Some(&b'>') {
        return None;
    }
    i = skip_ws(i + 1);
    i = expect_ci(i, "</div")?;
    i = skip_ws(i);
    if b.get(i) != Some(&b'>') {
        return None;
    }
    Some((id, i + 1))
}

/// The inert note the hub puts where a question belongs. Inline styles only:
/// the page may not run scripts, and the hub's own stylesheet is not loaded.
fn question_note(n: usize, text: &str) -> String {
    format!(
        "<div style=\"margin:16px 0;padding:12px 16px;border:2px dashed #b45309;\
border-radius:8px;background:#fffbeb;color:#78350f;font:600 15px/1.45 system-ui,sans-serif\">\
Question {n}: {} · Answer it beside this page.</div>",
        escape(text)
    )
}

/// Swaps each marker for a question this page asks for the hub's note.
/// Markers for ids the page did not ask about are left as they are.
pub fn mark_questions(html: &str, questions: &[Question]) -> String {
    if questions.is_empty() {
        return html.to_owned();
    }
    let mut out = String::with_capacity(html.len() + 256);
    let mut copied = 0;
    let mut from = 0;
    while let Some(at) = find_ci(html, "<div", from) {
        let found = read_marker(html, at).and_then(|(id, end)| {
            questions
                .iter()
                .position(|q| q.id == id)
                .map(|n| (n, &questions[n].text, end))
        });
        match found {
            Some((n, text, end)) => {
                out.push_str(&html[copied..at]);
                out.push_str(&question_note(n + 1, text));
                copied = end;
                from = end;
            }
            None => from = at + 1,
        }
    }
    out.push_str(&html[copied..]);
    out
}

/// Attaches questions to a page that was just saved. An empty list asks
/// nothing.
pub(crate) async fn ask_in_page(
    tx: &mut Transaction<'_, Sqlite>,
    page_id: i64,
    questions: &[Question],
) -> Result<()> {
    if questions.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string(questions).map_err(|e| StoreError::Invalid(e.to_string()))?;
    sqlx::query("INSERT INTO page_questions (page_id, questions) VALUES (?, ?)")
        .bind(page_id)
        .bind(json)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The questions a designer ended its answer with, if any, attached to its
/// page. Missing or badly shaped questions are skipped: the page stands.
pub(crate) async fn ask_from_answer(
    tx: &mut Transaction<'_, Sqlite>,
    page_id: i64,
    answer: &str,
) -> Result<()> {
    let asks = match last_json_object::<PageAsks>(answer) {
        Ok(a) => a,
        Err(_) => {
            tracing::debug!(page_id, "the designer asked no questions in its page");
            return Ok(());
        }
    };
    if let Err(_why) = validate_page_questions(&asks.questions) {
        // The reason quotes the agent's own ids, so it stays out of the log.
        tracing::warn!(
            page_id,
            "the designer's questions were not in the right shape; showing the page without them"
        );
        return Ok(());
    }
    ask_in_page(tx, page_id, &asks.questions).await
}

/// Checks a checker's verdict. Bad questions alone do not throw away a good
/// check: they are dropped and the verdict stands without them.
pub(crate) fn checked_verdict(mut v: ReviewVerdict) -> std::result::Result<ReviewVerdict, String> {
    match v.validate() {
        Ok(()) => Ok(v),
        Err(why) if v.questions.is_empty() => Err(why),
        Err(why) => {
            v.questions.clear();
            v.validate().map_err(|_| why)?;
            tracing::warn!("the checker's questions were not in the right shape; dropped them");
            Ok(v)
        }
    }
}

fn bad(msg: &str) -> StoreError {
    StoreError::Invalid(msg.to_owned())
}

/// Cuts `s` to at most `n` characters.
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Checks the owner's answers against the questions asked and tidies them:
/// words trimmed and clipped, empty words dropped.
fn clean_answers(questions: &[Question], given: &[Answer]) -> Result<Vec<Answer>> {
    let mut clean: Vec<Answer> = Vec::new();
    for a in given {
        let q = questions
            .iter()
            .find(|q| q.id == a.question_id)
            .ok_or_else(|| {
                bad("That question is not on this page. Reload the page and try again.")
            })?;
        if a.choice.is_some_and(|c| c >= q.choices.len()) {
            return Err(bad(
                "That is not one of the choices. Pick one of the choices shown.",
            ));
        }
        let other = a
            .other
            .as_deref()
            .map(str::trim)
            .filter(|o| !o.is_empty())
            .map(|o| clip(o, MAX_WORDS));
        if q.choices.is_empty() && other.is_none() {
            return Err(bad(
                "This question needs an answer in words. Write a few words, then send.",
            ));
        }
        if a.choice.is_none() && other.is_none() {
            return Err(bad("Pick a choice or write an answer."));
        }
        if clean.iter().any(|c| c.question_id == a.question_id) {
            return Err(bad("Answer each question once."));
        }
        clean.push(Answer {
            question_id: a.question_id.clone(),
            choice: a.choice,
            other,
        });
    }
    if clean.is_empty() {
        return Err(bad("Answer at least one question."));
    }
    Ok(clean)
}

/// The owner's answers in plain words, as the project lead reads them.
fn answers_text(page_title: &str, questions: &[Question], answers: &[Answer]) -> String {
    let mut s = format!("My answers on the page \"{page_title}\":");
    for a in answers {
        let Some(q) = questions.iter().find(|q| q.id == a.question_id) else {
            continue;
        };
        let chosen = a.choice.and_then(|c| q.choices.get(c));
        let said = match (chosen, a.other.as_deref()) {
            (Some(c), Some(o)) => format!("{c}. Also: {o}"),
            (Some(c), None) => c.clone(),
            (None, Some(o)) => o.to_owned(),
            (None, None) => continue,
        };
        let _ = write!(s, "\n- {} → {said}", q.text);
    }
    s
}

struct Asked {
    questions: Vec<Question>,
    answers: Option<Vec<Answer>>,
    answered_at: Option<String>,
}

async fn asked_in(tx: &mut Transaction<'_, Sqlite>, page: i64) -> Result<Option<Asked>> {
    let row =
        sqlx::query("SELECT questions, answers, answered_at FROM page_questions WHERE page_id = ?")
            .bind(page)
            .fetch_optional(&mut **tx)
            .await?;
    let Some(r) = row else {
        return Ok(None);
    };
    let questions = serde_json::from_str(&r.try_get::<String, _>("questions")?)
        .map_err(|e| StoreError::Invalid(e.to_string()))?;
    let answers = r
        .try_get::<Option<String>, _>("answers")?
        .map(|a| serde_json::from_str(&a))
        .transpose()
        .map_err(|e| StoreError::Invalid(e.to_string()))?;
    Ok(Some(Asked {
        questions,
        answers,
        answered_at: r.try_get("answered_at")?,
    }))
}

impl Store {
    /// The questions a page asks, in order; none for most pages.
    pub async fn questions_of(&self, page: i64) -> Result<Vec<Question>> {
        let mut tx = self.pool().begin().await?;
        let asked = asked_in(&mut tx, page).await?;
        tx.commit().await?;
        Ok(asked.map(|a| a.questions).unwrap_or_default())
    }

    /// A page as the library lists it, with its questions and any answers.
    pub async fn page_questions(&self, goal: i64, page: i64) -> Result<PageQuestions> {
        let info = self.library_page(goal, page).await?;
        let mut tx = self.pool().begin().await?;
        let asked = asked_in(&mut tx, page).await?;
        tx.commit().await?;
        Ok(match asked {
            Some(a) => PageQuestions {
                page: info,
                questions: a.questions,
                answers: a.answers,
                answered_at: a.answered_at,
            },
            None => PageQuestions {
                page: info,
                questions: vec![],
                answers: None,
                answered_at: None,
            },
        })
    }

    /// Records the owner's answers to a page's questions, once, and hands
    /// them to the project lead as an owner message, in one transaction.
    pub async fn answer_page(
        &self,
        goal: i64,
        page: i64,
        a: &PageAnswers,
    ) -> Result<PageQuestions> {
        check_receipt(a.receipt.as_deref())?;
        // Read before the write lock: the title does not change.
        let info = self.library_page(goal, page).await?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(id) = receipt_result(&mut tx, a.receipt.as_deref(), ANSWER_ACTION).await? {
            tx.commit().await?;
            return self.page_questions(goal, id).await;
        }
        let state: String = sqlx::query_scalar("SELECT state FROM goals WHERE id = ?")
            .bind(goal)
            .fetch_one(&mut *tx)
            .await?;
        if GoalState::from_str(&state).is_ok_and(GoalState::is_closed) {
            return Err(StoreError::Refused(
                "This goal is closed, so its questions can no longer be answered.".into(),
            ));
        }
        let asked = asked_in(&mut tx, page)
            .await?
            .filter(|q| !q.questions.is_empty())
            .ok_or_else(|| StoreError::Refused("This page asks no questions.".into()))?;
        if asked.answers.is_some() {
            return Err(StoreError::Duplicate(
                "those questions are already answered".into(),
            ));
        }
        let clean = clean_answers(&asked.questions, &a.answers)?;
        let text = answers_text(&info.title, &asked.questions, &clean);
        let message_id =
            Store::owner_message_in(&mut tx, goal, GoalMessageBody::Owner { text }).await?;
        sqlx::query(
            "UPDATE page_questions SET answers = ?, message_id = ?,
                 answered_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE page_id = ? AND answers IS NULL",
        )
        .bind(serde_json::to_string(&clean).map_err(|e| StoreError::Invalid(e.to_string()))?)
        .bind(message_id)
        .bind(page)
        .execute(&mut *tx)
        .await?;
        save_receipt(&mut tx, a.receipt.as_deref(), ANSWER_ACTION, page).await?;
        tx.commit().await?;
        self.page_questions(goal, page).await
    }
}

fn reply<T: serde::Serialize>(r: Result<T>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// `GET /api/goals/{goal}/pages/{page}/questions`, for the owner's viewer.
pub(crate) async fn questions(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((goal, page)): Path<(i64, i64)>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("reading a page's questions");
    }
    reply(st.store.page_questions(goal, page).await)
}

/// `POST /api/goals/{goal}/pages/{page}/answer`, the owner's answers.
pub(crate) async fn answer(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((goal, page)): Path<(i64, i64)>,
    Json(b): Json<PageAnswers>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("answering a page's questions");
    }
    reply(st.store.answer_page(goal, page, &b).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(id: &str, text: &str, choices: &[&str]) -> Question {
        Question {
            id: id.into(),
            text: text.into(),
            choices: choices.iter().map(|c| (*c).to_owned()).collect(),
            recommended: None,
        }
    }

    #[test]
    fn titles_come_from_the_first_title_tag() {
        assert_eq!(
            page_title("<html><head><TITLE>Sign-in &amp; reset</TITLE></head>").as_deref(),
            Some("Sign-in & reset")
        );
        assert_eq!(
            page_title("<title lang=\"en\">\n  Two\t\twords  </title><title>Second</title>")
                .as_deref(),
            Some("Two words")
        );
        assert_eq!(
            page_title("<title>&lt;b&gt; &quot;A&quot; &#39;B&#39; &amp;lt; &nbsp;</title>")
                .as_deref(),
            Some("<b> \"A\" 'B' &lt; &nbsp;"),
            "decoded once, unknown ones kept"
        );
        assert_eq!(page_title("<title>   </title>"), None);
        assert_eq!(page_title("<h1>No title</h1>"), None);
        assert_eq!(page_title("<title>never closed"), None);
        assert_eq!(page_title("<titles>x</titles>"), None);
        let long = format!("<title>{}</title>", "é".repeat(300));
        assert_eq!(page_title(&long).map(|t| t.chars().count()), Some(120));
    }

    #[test]
    fn markers_become_the_hubs_own_escaped_note() {
        let qs = [
            q("q1", "Layout A or B?", &["A", "B"]),
            q("q2", "<script>alert(1)</script> & \"more\"", &[]),
        ];
        let html = "<p>x</p><div data-question=\"q1\"></div>\
<DIV  data-question = 'q2' >\n </div >\
<div data-question=\"q9\"></div><div data-question=\"q1\">text</div>";
        let out = mark_questions(html, &qs);
        assert!(out.starts_with("<p>x</p><div style="), "{out}");
        assert!(out.contains("Question 1: Layout A or B? · Answer it beside this page."));
        assert!(out.contains(
            "Question 2: &lt;script&gt;alert(1)&lt;/script&gt; &amp; &quot;more&quot; · Answer"
        ));
        assert!(!out.contains("<script>"));
        assert!(!out.contains("data-question=\"q1\"></div>"));
        assert!(!out.contains("data-question = 'q2'"));
        assert!(
            out.contains("<div data-question=\"q9\"></div>"),
            "unknown ids stay"
        );
        assert!(
            out.contains("<div data-question=\"q1\">text</div>"),
            "only empty markers"
        );
        assert_eq!(mark_questions(html, &[]), html);
        assert_eq!(mark_questions("<div", &qs), "<div");
        assert_eq!(
            mark_questions("<div data-question=\"q1\"></div", &qs),
            "<div data-question=\"q1\"></div"
        );
    }

    #[test]
    fn answers_are_checked_against_the_questions() {
        let qs = [
            q("q1", "Layout?", &["A", "B"]),
            q("q2", "Anything else?", &[]),
        ];
        let ans = |id: &str, choice: Option<usize>, other: Option<&str>| Answer {
            question_id: id.into(),
            choice,
            other: other.map(str::to_owned),
        };
        assert!(clean_answers(&qs, &[]).is_err());
        assert!(clean_answers(&qs, &[ans("q9", Some(0), None)]).is_err());
        assert!(clean_answers(&qs, &[ans("q1", Some(2), None)]).is_err());
        assert!(clean_answers(&qs, &[ans("q1", None, Some("  "))]).is_err());
        assert!(clean_answers(&qs, &[ans("q2", Some(0), None)]).is_err());
        assert!(clean_answers(&qs, &[ans("q2", None, None)]).is_err());
        assert!(clean_answers(&qs, &[ans("q1", Some(0), None), ans("q1", Some(1), None)]).is_err());
        let long = "w".repeat(3000);
        let ok = clean_answers(
            &qs,
            &[
                ans("q1", Some(1), Some(" bigger ")),
                ans("q2", None, Some(&long)),
            ],
        )
        .expect("good answers");
        assert_eq!(ok[0].other.as_deref(), Some("bigger"));
        assert_eq!(ok[1].other.as_ref().map(|o| o.chars().count()), Some(2000));
        let text = answers_text("Visual plan", &qs, &ok[..1]);
        assert_eq!(
            text,
            "My answers on the page \"Visual plan\":\n- Layout? → B. Also: bigger"
        );
    }

    #[test]
    fn bad_questions_do_not_spoil_a_check() {
        let mut v = ReviewVerdict {
            pass: true,
            summary: "Works".into(),
            issues: vec![],
            questions: vec![q("q1", "Keep it?", &["Only"])],
        };
        let kept = checked_verdict(v.clone()).expect("the check stands");
        assert!(kept.questions.is_empty());
        v.summary = String::new();
        assert!(checked_verdict(v).is_err(), "a bad check is still bad");
    }
}
