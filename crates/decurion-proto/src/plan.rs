//! The Roadmap, the page library and questions inside pages.
//!
//! Each app has one Roadmap: an ordered checklist of its goals and the
//! to-do items found along the way, so the owner always sees what is done and
//! what is left. Goal rows are never edited by hand: their title, steps and
//! state are read from the goal each time, so the plan cannot fall behind the
//! work. To-do rows come from the owner, or from the project lead (what an
//! audit found, follow-ups it suggests); the owner ticks them, or turns them
//! into a goal, and that goal closing as done ticks them.
//!
//! Pages agents drew (visual plans, pieces as built, audit write-ups) are
//! listed in the library beside the goal and piece they belong to. A designer
//! or checker may ask the owner questions about its page; the hub draws the
//! answer buttons around the page, never inside it, because pages run no
//! scripts. Answers reach the project lead as an owner message.

use crate::goal::{Answer, GoalState, PageKind, Question};
use serde::{Deserialize, Serialize};

/// Most to-do items the project lead may add in one answer.
pub const MAX_TODO: usize = 10;
/// Most questions one page may ask.
pub const MAX_PAGE_QUESTIONS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Goal,
    Todo,
}

/// Where a row stands. For a goal row: done when the goal is closed as done,
/// dropped when it was stopped, open otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    Open,
    Done,
    Dropped,
}

impl ItemState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
            Self::Dropped => "dropped",
        }
    }
}

/// Who put a row on the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemSource {
    Owner,
    /// The project lead: findings from finished work, or follow-ups it
    /// suggests. Shown as such, and never acted on without the owner's tap.
    Lead,
}

/// One step of a goal, worked out from the goal and its pieces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Done,
    /// An agent is on it now.
    Now,
    /// It waits for the owner: a plan to approve, work to look at.
    You,
    /// Not started yet.
    Later,
    /// It went wrong or needs a look.
    Problem,
    /// The goal was stopped before this step.
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    /// Plain words: "Talk it through", "Approve the plan", a piece's title,
    /// "Your review".
    pub title: String,
    pub state: StepState,
    /// For a piece: its thread, so the board can open it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,
}

/// A goal as the plan shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalRef {
    pub id: i64,
    pub title: String,
    pub state: GoalState,
}

/// A page agents drew, as the library lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryPage {
    pub id: i64,
    pub goal_id: i64,
    pub goal_title: String,
    pub app_id: i64,
    pub app_name: String,
    pub kind: PageKind,
    /// The page's own title, or a name made from its kind and piece.
    pub title: String,
    /// For a check page: the piece it shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piece_title: Option<String>,
    /// Questions the page asks, and whether they still wait for an answer.
    pub questions: usize,
    pub waiting: bool,
    pub created_at: String,
}

/// One row of the Roadmap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: i64,
    pub kind: ItemKind,
    /// A goal row: the goal's own title. A to-do: its text.
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub source: ItemSource,
    pub state: ItemState,
    /// A goal row: its goal. A to-do: the goal started to do it, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalRef>,
    /// A goal row's steps, in order: one chip per piece, so each links to
    /// its piece.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<PlanStep>,
    /// A goal row's journey: its state words (`label`) are the ones every
    /// other screen shows for the goal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journey: Option<crate::goal::Journey>,
    /// A goal row's to-do items: what it found, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub found: Vec<PlanItem>,
    /// A goal row's pages, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<LibraryPage>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<String>,
}

/// An app's Roadmap, top-level rows in the owner's order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanView {
    pub app_id: i64,
    pub app_name: String,
    pub items: Vec<PlanItem>,
    /// Rows done and rows left, counting goals and to-dos, findings included,
    /// but not dropped ones.
    pub done: usize,
    pub left: usize,
}

/// A to-do the owner adds. With `parent_id` it goes under that goal's row;
/// without, at the end of the plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTodo {
    pub title: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub receipt: Option<String>,
}

/// The owner changes a to-do: ticks it, drops it, or rewords it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EditTodo {
    #[serde(default)]
    pub state: Option<ItemState>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Up,
    Down,
}

/// Moves a row one place among the rows beside it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveItem {
    pub direction: Direction,
}

/// Starts one goal to do these to-do items, with the owner's own words added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixTodos {
    pub item_ids: Vec<i64>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub receipt: Option<String>,
}

/// A to-do the project lead suggests, in its JSON answer (`todo`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeadTodo {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

fn check_len(what: &str, s: &str, max: usize) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if s.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    Ok(())
}

/// Checks the project lead's to-do list. The message names what is wrong, so
/// it can be handed back to the lead to fix.
pub fn validate_todo(todo: &[LeadTodo]) -> Result<(), String> {
    if todo.len() > MAX_TODO {
        return Err(format!("at most {MAX_TODO} to-do items at a time"));
    }
    for t in todo {
        check_len("to-do title", &t.title, 200)?;
        if let Some(d) = &t.detail {
            check_len("to-do detail", d, 1000)?;
        }
    }
    Ok(())
}

/// What a designer or checker may end its answer with to ask the owner about
/// its page: `{"questions": [...]}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageAsks {
    #[serde(default)]
    pub questions: Vec<Question>,
}

/// A page question is a goal question that may also have no choices at all:
/// then the owner answers in words. Yes or no is two choices.
pub fn validate_page_questions(qs: &[Question]) -> Result<(), String> {
    if qs.len() > MAX_PAGE_QUESTIONS {
        return Err(format!("at most {MAX_PAGE_QUESTIONS} questions in a page"));
    }
    let mut ids = std::collections::HashSet::new();
    for q in qs {
        check_len("question id", &q.id, 20)?;
        if !q
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!(
                "question id {} may use only letters, digits, - and _",
                q.id
            ));
        }
        if !ids.insert(q.id.as_str()) {
            return Err(format!("question id {} is used twice", q.id));
        }
        check_len("question text", &q.text, 500)?;
        if q.choices.len() == 1 || q.choices.len() > crate::goal::MAX_CHOICES {
            return Err(format!(
                "question {} needs no choices, or 2 to {} choices",
                q.id,
                crate::goal::MAX_CHOICES
            ));
        }
        for c in &q.choices {
            check_len("choice", c, 200)?;
        }
        if q.recommended.is_some_and(|r| r >= q.choices.len()) {
            return Err(format!(
                "question {} recommends a choice it does not have",
                q.id
            ));
        }
    }
    Ok(())
}

/// A page's questions and, once given, the owner's answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageQuestions {
    pub page: LibraryPage,
    pub questions: Vec<Question>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<Vec<Answer>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<String>,
}

/// The owner's answers to a page's questions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageAnswers {
    pub answers: Vec<Answer>,
    #[serde(default)]
    pub receipt: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(id: &str, choices: &[&str]) -> Question {
        Question {
            id: id.into(),
            text: "Which one?".into(),
            choices: choices.iter().map(|c| (*c).to_owned()).collect(),
            recommended: None,
        }
    }

    #[test]
    fn page_questions_may_take_words_only() {
        assert!(validate_page_questions(&[q("q1", &[])]).is_ok());
        assert!(validate_page_questions(&[q("q1", &["Yes", "No"])]).is_ok());
        assert!(validate_page_questions(&[q("q1", &["Only"])]).is_err());
        assert!(validate_page_questions(&[q("q1", &[]), q("q1", &[])]).is_err());
        assert!(validate_page_questions(&[q("q 1", &[])]).is_err(), "spaces");
        assert!(
            validate_page_questions(&[q("q\"><b>", &[])]).is_err(),
            "marks"
        );
        let mut r = q("q1", &["a", "b"]);
        r.recommended = Some(2);
        assert!(validate_page_questions(&[r]).is_err());
        let many: Vec<Question> = (0..5).map(|i| q(&format!("q{i}"), &[])).collect();
        assert!(validate_page_questions(&many).is_err());
    }

    #[test]
    fn lead_todo_lists_are_checked() {
        let t = |title: &str| LeadTodo {
            title: title.into(),
            detail: None,
        };
        assert!(validate_todo(&[t("Check-in skips the insurance card")]).is_ok());
        assert!(validate_todo(&[t("  ")]).is_err());
        let many: Vec<LeadTodo> = (0..11).map(|i| t(&format!("item {i}"))).collect();
        assert!(validate_todo(&many).is_err());
    }
}
