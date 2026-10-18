//! Files the owner attaches for agents to read: a spec as a PDF, a Word
//! brief, a price list in a spreadsheet, a photo of a sketch.
//!
//! A file belongs to an app, and optionally to one goal of that app. Every
//! piece of work on the app, and every helper (coordinator, design, checker),
//! gets the app's files; a goal's files go only to that goal's helpers and
//! pieces. The hub keeps the files; before each run the runner copies them
//! into the agent's home volume at [`BOX_DIR`], fresh every time, so nothing
//! an agent does to its copy reaches the owner's file. They sit outside
//! `/work`, so they reach GitHub only if an agent copies one into the code
//! on purpose.
//!
//! Rules that must stay true:
//! - Names are cleaned by [`clean_name`] before they are stored, so a name
//!   is always a plain file name in [`BOX_DIR`]: ASCII, no folders, no
//!   leading dot or dash, at most [`MAX_NAME`] bytes.
//! - A text version, when the hub could make one, is `<name>.txt` beside the
//!   file ([`text_name`]); no two files of one app may clash with each other
//!   or with each other's text version ([`clashes`]).
//! - Sizes are capped per file ([`MAX_FILE_BYTES`]) and per app
//!   ([`MAX_APP_BYTES`], [`MAX_FILES`]), so a run's copy stays small.

use serde::{Deserialize, Serialize};

/// Where an agent finds the files, inside its box.
pub const BOX_DIR: &str = "/home/agent/context";

/// The largest single file.
pub const MAX_FILE_BYTES: u64 = 25 << 20;

/// All of one app's files together, goal files included.
pub const MAX_APP_BYTES: u64 = 250 << 20;

/// How many files one app may hold, goal files included.
pub const MAX_FILES: usize = 100;

/// The longest text version the hub keeps; longer text is cut there.
pub const MAX_TEXT_BYTES: usize = 4 << 20;

/// The longest file name, in bytes, extension included.
pub const MAX_NAME: usize = 80;

/// What kind of file it is, from its name. It decides whether the hub makes
/// a text version and how the board shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Pdf,
    /// `.docx`. Old `.doc` files are [`FileKind::Other`].
    Word,
    /// `.xlsx`, or `.csv` / `.tsv`.
    Sheet,
    /// `.pptx`.
    Slides,
    Image,
    /// Already plain text (`.txt`, `.md`, `.json`, ...): agents read it as it is.
    Text,
    Other,
}

impl FileKind {
    pub fn of(name: &str) -> FileKind {
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "pdf" => FileKind::Pdf,
            "docx" => FileKind::Word,
            "xlsx" | "csv" | "tsv" => FileKind::Sheet,
            "pptx" => FileKind::Slides,
            "png" | "jpg" | "jpeg" | "gif" | "webp" => FileKind::Image,
            "txt" | "md" | "markdown" | "json" | "yaml" | "yml" | "xml" | "html" | "htm"
            | "rtf" | "sql" | "log" => FileKind::Text,
            _ => FileKind::Other,
        }
    }
}

/// One attached file, as the board lists it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextFile {
    pub id: i64,
    pub app_id: i64,
    /// Set when the file belongs to one goal only.
    #[serde(default)]
    pub goal_id: Option<i64>,
    /// The cleaned name; also its name in [`BOX_DIR`].
    pub name: String,
    pub size: u64,
    pub kind: FileKind,
    /// Characters in the text version the hub made, if it made one.
    #[serde(default)]
    pub text_chars: Option<u64>,
    /// Plain words for the owner about reading the file, for example why no
    /// text could be taken out of it. Empty when there is nothing to say.
    #[serde(default)]
    pub note: String,
    pub added_at: String,
}

/// A file named in a claim, so the runner can tell the agent what it has.
/// The file itself comes from the hub separately.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextRef {
    pub name: String,
    pub size: u64,
    pub kind: FileKind,
    /// Whether `<name>.txt` sits beside it.
    #[serde(default)]
    pub text: bool,
    /// Set when the file belongs to the goal rather than the whole app.
    #[serde(default)]
    pub for_goal: bool,
}

/// The name of a file's text version.
pub fn text_name(name: &str) -> String {
    format!("{name}.txt")
}

/// Whether two cleaned names would land on the same file in the box, either
/// as files or as one's text version. Compared without case, since Windows
/// and macOS folders ignore it.
pub fn clashes(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
    a == b || text_name(&a) == b || text_name(&b) == a
}

/// Turns the name a browser sent into a plain, safe file name: only the last
/// part of any path, letters, digits, spaces and `. _ - ( )`, everything
/// else as `-`, no leading dot, dash or space, and at most [`MAX_NAME`]
/// bytes with the extension kept. The error is for the owner.
pub fn clean_name(raw: &str) -> Result<String, &'static str> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut s: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-' | '(' | ')') {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Runs of filler read badly and hide nothing.
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    while s.contains("  ") {
        s = s.replace("  ", " ");
    }
    let s = s
        .trim_start_matches(['.', '-', ' '])
        .trim_end_matches(['.', ' '])
        .to_owned();
    if !s.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Err("Give the file a name with some letters or numbers in it.");
    }
    if s.len() <= MAX_NAME {
        return Ok(s);
    }
    // Keep a short extension; cut the rest.
    let (stem, ext) = match s.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 10 => (stem, format!(".{ext}")),
        _ => (s.as_str(), String::new()),
    };
    let keep = MAX_NAME - ext.len();
    let stem = stem[..keep].trim_end_matches(['.', ' ', '-']);
    Ok(format!("{stem}{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned_to_plain_file_names() {
        assert_eq!(clean_name("Spec v2.pdf").as_deref(), Ok("Spec v2.pdf"));
        assert_eq!(
            clean_name("C:\\Users\\me\\Desktop\\Brief.docx").as_deref(),
            Ok("Brief.docx")
        );
        assert_eq!(clean_name("../../etc/passwd").as_deref(), Ok("passwd"));
        assert_eq!(clean_name(".bashrc").as_deref(), Ok("bashrc"));
        assert_eq!(clean_name("-rf.txt").as_deref(), Ok("rf.txt"));
        assert_eq!(
            clean_name("Präsentation:final?.pptx").as_deref(),
            Ok("Pr-sentation-final-.pptx")
        );
        assert!(clean_name("...").is_err());
        assert!(clean_name("").is_err());
        let long = format!("{}.xlsx", "a".repeat(200));
        let c = clean_name(&long).expect("long names are cut");
        assert_eq!(c.len(), MAX_NAME);
        assert!(c.ends_with(".xlsx"));
    }

    #[test]
    fn kinds_come_from_the_extension() {
        assert_eq!(FileKind::of("a.PDF"), FileKind::Pdf);
        assert_eq!(FileKind::of("a.docx"), FileKind::Word);
        assert_eq!(FileKind::of("a.doc"), FileKind::Other);
        assert_eq!(FileKind::of("a.csv"), FileKind::Sheet);
        assert_eq!(FileKind::of("a.jpeg"), FileKind::Image);
        assert_eq!(FileKind::of("notes.md"), FileKind::Text);
        assert_eq!(FileKind::of("noext"), FileKind::Other);
    }

    #[test]
    fn a_file_clashes_with_another_files_text_version() {
        assert!(clashes("Brief.docx", "brief.DOCX"));
        assert!(clashes("Brief.docx", "Brief.docx.txt"));
        assert!(clashes("Brief.docx.txt", "Brief.docx"));
        assert!(!clashes("Brief.docx", "Brief.pdf"));
    }
}
