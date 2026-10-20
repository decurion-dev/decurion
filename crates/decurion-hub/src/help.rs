//! Owner help, served by the hub itself at `/help`.
//!
//! The help pages in `docs/help/src` are built into the hub, so Help opens
//! readable pages from the hub's own address on every install, with no
//! outside website and nothing to set up. The mdBook built from the same
//! files is only published on release; these are the same words.
//!
//! Rules: the pages are Decurion's own text, never an agent's, so only here
//! may links be clickable (`help.html` draws them with `D.richText` and its
//! `links` option, which only allows other help pages and outside https
//! addresses). The page needs no sign-in: it holds nothing but the help
//! that is public anyway. Every file in `docs/help/src` must be listed in
//! `PAGES` (a test checks it), in the order `SUMMARY.md` gives.

use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};

macro_rules! page {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!("../../../docs/help/src/", $name)),
        )
    };
}

/// The contents, as the book's own `SUMMARY.md` writes it.
pub const SUMMARY: &str = include_str!("../../../docs/help/src/SUMMARY.md");

/// Every help page: its file name and its text.
pub const PAGES: &[(&str, &str)] = &[
    page!("README.md"),
    page!("install.md"),
    page!("connect-claude-accounts.md"),
    page!("add-your-first-app.md"),
    page!("choose-your-rules.md"),
    page!("choose-websites.md"),
    page!("add-test-keys.md"),
    page!("save-website-sign-ins.md"),
    page!("change-an-app.md"),
    page!("connect-your-phone.md"),
    page!("check-your-sign-ins.md"),
    page!("give-your-first-goal.md"),
    page!("research-and-ask-why.md"),
    page!("follow-your-roadmap.md"),
    page!("attach-files.md"),
    page!("answer-requests.md"),
    page!("talk-to-a-running-agent.md"),
    page!("talk-and-listen.md"),
    page!("take-over-the-browser.md"),
    page!("run-your-app.md"),
    page!("review-and-merge.md"),
    page!("deploy-your-app.md"),
    page!("dismiss-finished-work.md"),
    page!("see-how-much-a-goal-used.md"),
    page!("when-an-account-runs-out.md"),
    page!("when-something-fails.md"),
    page!("back-up-and-restore.md"),
    page!("add-the-cloud-worker.md"),
    page!("add-ons.md"),
    page!("improve-decurion-itself.md"),
];

/// `GET /help`: the help reader.
pub async fn page() -> Response {
    crate::page(include_str!("help.html"))
}

/// `GET /help/book.json`: the contents and every page, for `help.html`.
pub async fn book() -> Response {
    let pages: Map<String, Value> = PAGES
        .iter()
        .map(|(name, text)| ((*name).to_string(), Value::from(*text)))
        .collect();
    (
        [
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        axum::Json(json!({ "summary": SUMMARY, "pages": pages })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_help_file_is_built_in_and_in_the_contents() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/help/src");
        let mut on_disk: Vec<String> = std::fs::read_dir(dir)
            .expect("the help folder is in the repository")
            .map(|e| {
                e.expect("a folder entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|n| n.ends_with(".md") && n != "SUMMARY.md")
            .collect();
        on_disk.sort();
        let mut listed: Vec<String> = PAGES.iter().map(|(n, _)| (*n).to_string()).collect();
        listed.sort();
        assert_eq!(on_disk, listed, "add the new help page to PAGES in help.rs");
        for (name, _) in PAGES {
            assert!(
                SUMMARY.contains(&format!("({name})")),
                "{name} is missing from SUMMARY.md"
            );
        }
    }
}
