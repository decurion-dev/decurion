//! Research pieces' reports: the page a research piece's agent leaves for
//! the owner instead of commits.
//!
//! Only the runner holding the running research piece may save its report,
//! checked in the same transaction as the save, like a run's events. A page
//! that is missing, blank or too big is replaced by one the hub makes from
//! the agent's own final summary, escaped, so the owner always has something
//! to read. Reports are shown only to the owner, in the same sandbox as the
//! pages agents draw for a goal (`goals::page`), and nothing from research is
//! ever sent to GitHub (`github::want_send`).

use crate::store::{runner_actor, Result, Store, StoreError};
use decurion_proto::goal::{ResearchReport, MAX_PAGE_BYTES};
use sqlx::Row;

/// Makes `s` safe to put in HTML text or a quoted attribute.
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

/// A calm, self-contained page for a research piece whose agent drew none:
/// what it said when it finished, or that it said nothing. No scripts and
/// nothing loaded from anywhere, so it shows the same inside the sandbox.
pub(crate) fn fallback_page(title: &str, summary: Option<&str>) -> String {
    let title = escape(title.trim());
    let body = match summary.map(str::trim).filter(|s| !s.is_empty()) {
        Some(said) => format!(
            "<p class=\"note\">Your research agent did not draw a report page, so this is what it \
said when it finished.</p>\n<div class=\"said\">{}</div>",
            escape(said)
        ),
        None => {
            "<p class=\"note\">Your research agent finished without leaving a report or saying \
what it found. You can send this piece back to its agent from the board to try again.</p>"
                .to_owned()
        }
    };
    format!(
        "<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>{title}</title>
<style>
:root {{ color-scheme: light dark; --bg: #faf9f6; --fg: #1d2330; --muted: #5b6372; --card: #ffffff; --line: #e3e1db; }}
@media (prefers-color-scheme: dark) {{
  :root {{ --bg: #16181d; --fg: #e8e6e1; --muted: #a2a8b3; --card: #1f2228; --line: #33373f; }}
}}
body {{ margin: 0; background: var(--bg); color: var(--fg);
  font: 16px/1.6 system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif; }}
main {{ max-width: 42rem; margin: 0 auto; padding: 32px 16px 96px; }}
h1 {{ font-size: 1.5rem; line-height: 1.3; margin: 0 0 12px; }}
.note {{ color: var(--muted); margin: 0 0 20px; }}
.said {{ white-space: pre-wrap; overflow-wrap: anywhere; background: var(--card);
  border: 1px solid var(--line); border-radius: 12px; padding: 16px 18px; }}
</style>
</head>
<body>
<main>
<h1>{title}</h1>
{body}
</main>
</body>
</html>
"
    )
}

impl Store {
    /// Saves the report of a research piece running on `runner`, replacing
    /// the one from an earlier run. Refused unless the thread is running on
    /// that runner, is research, and is a piece of a goal.
    pub async fn save_report(
        &self,
        thread_id: i64,
        runner: &str,
        report: &ResearchReport,
    ) -> Result<()> {
        let actor = runner_actor(runner)?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(
            "SELECT t.state, t.runner, t.research, p.goal_id, p.title
             FROM threads t LEFT JOIN goal_pieces p ON p.thread_id = t.id WHERE t.id = ?",
        )
        .bind(thread_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound)?;
        let state: String = row.try_get("state")?;
        let holder: Option<String> = row.try_get("runner")?;
        let research: bool = row.try_get("research")?;
        let goal_id: Option<i64> = row.try_get("goal_id")?;
        let title: Option<String> = row.try_get("title")?;
        if state != "running" || holder.as_deref() != Some(actor.as_str()) {
            return Err(StoreError::Conflict);
        }
        let (true, Some(goal_id), Some(title)) = (research, goal_id, title) else {
            return Err(StoreError::Invalid(
                "Only a research piece of a goal writes a report.".into(),
            ));
        };
        let html = match report
            .html
            .as_deref()
            .filter(|h| !h.trim().is_empty() && h.len() <= MAX_PAGE_BYTES)
        {
            Some(h) => h.to_owned(),
            None => {
                let summary: Option<String> = sqlx::query_scalar(
                    "SELECT json_extract(body, '$.summary') FROM events WHERE thread_id = ?
                         AND json_extract(body, '$.kind') = 'finished'
                         AND json_extract(body, '$.ok') = 1
                     ORDER BY id DESC LIMIT 1",
                )
                .bind(thread_id)
                .fetch_optional(&mut *tx)
                .await?
                .flatten();
                fallback_page(&title, summary.as_deref())
            }
        };
        sqlx::query(
            "INSERT INTO piece_reports (thread_id, goal_id, html) VALUES (?, ?, ?)
             ON CONFLICT (thread_id) DO UPDATE SET html = excluded.html,
                 created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(thread_id)
        .bind(goal_id)
        .bind(html)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The report page of piece `thread_id` of goal `goal_id`.
    pub async fn report_html(&self, goal_id: i64, thread_id: i64) -> Result<String> {
        sqlx::query_scalar("SELECT html FROM piece_reports WHERE thread_id = ? AND goal_id = ?")
            .bind(thread_id)
            .bind(goal_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(StoreError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fallback_page_escapes_what_the_agent_said() {
        let p = fallback_page(
            "Check <logins>",
            Some("a < b & \"c\" 'd' <script>x</script>"),
        );
        assert!(p.contains("<title>Check &lt;logins&gt;</title>"), "{p}");
        assert!(
            p.contains("a &lt; b &amp; &quot;c&quot; &#39;d&#39; &lt;script&gt;"),
            "{p}"
        );
        assert!(!p.contains("<script"), "{p}");
        assert!(p.contains("did not draw a report page"), "{p}");
        assert!(p.contains("white-space: pre-wrap"), "{p}");
        let none = fallback_page("Check", Some("  "));
        assert!(none.contains("without leaving a report"), "{none}");
        assert!(!none.contains("src=") && !none.contains("http"), "{none}");
    }
}
