//! Add-ons, as far as the hub and the runner share them (ADR 0001, v1:
//! tools only).
//!
//! An add-on tool is an MCP server the owner approved once. It runs in a
//! sealed box of its own beside the agent's box, never inside it: the
//! agent's box holds the run's Claude credential, and no add-on code may
//! ever run where it could read it. The tool box shares the run's network
//! (so it goes out through the same gate) and serves MCP over HTTP on a
//! port on that network's localhost, which is how the agent reaches it.
//!
//! What must stay true:
//! - **Exactly the approved bytes.** An add-on is pinned to a commit, and
//!   Decurion hashes its files itself ([`tree_hash`]). The hub checks the
//!   hash before it hands the files out and the runner checks it again
//!   before it starts a tool. A mismatch means the tool does not run.
//! - **Nothing it was not granted.** A tool gets no code, no home volume,
//!   no Claude sign-in and none of the app's test keys except the one it
//!   names. Its websites join the run's gate list for that run only.
//! - **Hard stops stay hard.** Tools that would send messages, deploy,
//!   touch production data or spend money are refused at install, and so
//!   are websites known for those ([`HARD_STOP_SITES`]), because v1 has no
//!   way to stop each such call for the owner's OK.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The manifest's file name, at the add-on's root.
pub const MANIFEST: &str = "decurion-addon.toml";

/// The add-on API version this Decurion runs.
pub const API: &str = "1";

/// Where a tool box sees the add-on's files, read-only.
pub const BOX_DIR: &str = "/addon";

/// The variable a tool box reads its port from. `{port}` in the tool's
/// command is replaced with the same number.
pub const PORT_VAR: &str = "DECURION_MCP_PORT";

/// The first add-on tool's port on the run's localhost; the next tool gets
/// the next one. Decurion's own browser tools are on 8931.
pub const FIRST_PORT: u16 = 8940;

/// Most tools one run gets from all its add-ons together.
pub const MAX_RUN_TOOLS: usize = 8;

/// Limits on what an add-on may hold, so fetching and checking one stays
/// quick and a run's download stays small.
pub const MAX_FILES: usize = 1000;
pub const MAX_FILE_BYTES: u64 = 5 << 20;
pub const MAX_TOTAL_BYTES: u64 = 20 << 20;

/// Tool names Decurion's own tools use.
pub const RESERVED_TOOLS: &[&str] = &["browser", "decurion"];

/// Websites whose main use is sending messages to people or deploying.
/// Each of those is a hard stop that needs the owner's OK every time, which
/// Decurion cannot ask for add-on tools yet, so an add-on that names one of
/// them (or a site under one) is refused at install.
pub const HARD_STOP_SITES: &[&str] = &[
    // Chat and messages
    "slack.com",
    "discord.com",
    "discordapp.com",
    "telegram.org",
    "whatsapp.com",
    "whatsapp.net",
    "twilio.com",
    "vonage.com",
    "nexmo.com",
    "messagebird.com",
    // Email
    "sendgrid.com",
    "sendgrid.net",
    "mailgun.net",
    "mailgun.org",
    "postmarkapp.com",
    "mailchimp.com",
    "mandrillapp.com",
    "resend.com",
    "sparkpost.com",
    "mailjet.com",
    "gmail.googleapis.com",
    "graph.microsoft.com",
    "smtp.gmail.com",
    // Deploys
    "vercel.com",
    "netlify.com",
    "heroku.com",
    "fly.io",
    "render.com",
    "railway.app",
];

/// The hard-stop website `site` is, or is part of, if any.
pub fn hard_stop_site(site: &str) -> Option<&'static str> {
    HARD_STOP_SITES
        .iter()
        .find(|s| crate::sites::covers(s, site) || crate::sites::covers(site, s))
        .copied()
}

/// An add-on enabled for the app a claimed piece of work belongs to: what
/// the runner needs to fetch, check and start its tools. Never a key's value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AddonUse {
    /// The add-on's id, such as `example.words`.
    pub id: String,
    pub name: String,
    pub version: String,
    /// [`tree_hash`] of the files the owner approved.
    pub hash: String,
    pub tools: Vec<ToolUse>,
}

/// One approved tool of an add-on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolUse {
    /// The agent sees its functions as `mcp__<name>__<function>`.
    pub name: String,
    /// Plain words for the agent and the owner.
    pub summary: String,
    /// The program and its arguments, run in the tool box with the add-on's
    /// files at [`BOX_DIR`] as the working folder. `{port}` is replaced.
    pub command: Vec<String>,
    /// Websites the tool needs, added to the run's gate list.
    #[serde(default)]
    pub sites: Vec<String>,
    /// The one test key the tool gets, if any.
    #[serde(default)]
    pub key: Option<KeyUse>,
}

/// A test key of the app, by name, and the variable the tool reads it from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyUse {
    /// The variable inside the tool box.
    pub var: String,
    /// The app's test key, by its name in the vault.
    pub test_key: String,
}

/// An add-on's files, as the hub hands them to a runner for one run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bundle {
    pub id: String,
    pub version: String,
    pub hash: String,
    pub files: Vec<BundleFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BundleFile {
    /// A [`check_path`] path inside the add-on.
    pub path: String,
    /// The file's bytes, base64 (standard, padded).
    pub data: String,
}

/// The add-on's tree hash, hex: SHA-256 over one line per file, sorted by
/// path, of `path NUL sha256(file) LF`. Computed by Decurion itself rather
/// than trusting git's own hashes, the same way on the hub and the runner.
pub fn tree_hash<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> String {
    let mut lines: Vec<(&str, String)> = files
        .into_iter()
        .map(|(p, data)| (p, hex(&Sha256::digest(data))))
        .collect();
    lines.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut h = Sha256::new();
    for (p, sum) in lines {
        h.update(p.as_bytes());
        h.update([0]);
        h.update(sum.as_bytes());
        h.update(b"\n");
    }
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A tree hash as [`tree_hash`] writes it.
pub fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A path inside an add-on: plain segments of letters, digits and `._-+@`
/// joined by `/`, never `.`, `..`, absolute, or with a backslash, so it can
/// only ever name a file inside the add-on's own folder.
///
/// It must also be a name Windows keeps exactly as written, because the hub
/// and the runner may be Windows PCs and the tree hash is over the names:
/// no device name such as `CON` or `com1.txt` (Windows opens the device, not
/// a file), no segment ending in `.` (Windows drops it, so `a.` and `a`
/// would be one file), and none of `\ : < > " | ? *` or spaces, which the
/// letter list above already leaves out.
pub fn check_path(p: &str) -> bool {
    (1..=200).contains(&p.len())
        && p.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && !seg.ends_with('.')
                && !windows_device(seg)
                && seg.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+' | b'@')
                })
        })
}

/// A name Windows reads as a device whatever its extension: `CON`, `PRN`,
/// `AUX`, `NUL`, `COM1`-`COM9` and `LPT1`-`LPT9`, in any case, alone or
/// before the first dot (`nul.txt`, `Com1.tar.gz`).
fn windows_device(seg: &str) -> bool {
    let stem = seg.split('.').next().unwrap_or(seg).to_ascii_uppercase();
    match stem.as_bytes() {
        b"CON" | b"PRN" | b"AUX" | b"NUL" => true,
        [b'C', b'O', b'M', d] | [b'L', b'P', b'T', d] => (b'1'..=b'9').contains(d),
        _ => false,
    }
}

/// An add-on id: lowercase letters and digits in parts joined by `.` or
/// `-`, with at least one dot (`example.tickets`), so it starts with
/// something that is the author's own.
pub fn check_id(id: &str) -> bool {
    let b = id.as_bytes();
    (3..=64).contains(&b.len())
        && id.contains('.')
        && b[0].is_ascii_alphanumeric()
        && b[b.len() - 1].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'-'))
        && !b
            .windows(2)
            .any(|w| !w[0].is_ascii_alphanumeric() && !w[1].is_ascii_alphanumeric())
}

/// A tool name: what the agent sees between `mcp__` and the next `__`. A
/// lowercase letter, then lowercase letters, digits or `-` (never `_`, which
/// would blur where the name ends), and none of Decurion's own.
pub fn check_tool_name(n: &str) -> bool {
    let b = n.as_bytes();
    (1..=30).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && !n.ends_with('-')
        && !RESERVED_TOOLS.contains(&n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_hash_ignores_order_and_sees_every_byte_and_name() {
        let a = tree_hash([("b.txt", &b"two"[..]), ("a.txt", &b"one"[..])]);
        let b = tree_hash([("a.txt", &b"one"[..]), ("b.txt", &b"two"[..])]);
        assert_eq!(a, b);
        assert!(is_hash(&a));
        assert_ne!(
            a,
            tree_hash([("a.txt", &b"one"[..]), ("b.txt", &b"twO"[..])])
        );
        assert_ne!(
            a,
            tree_hash([("a.txt", &b"one"[..]), ("c.txt", &b"two"[..])])
        );
        assert_ne!(a, tree_hash([("a.txt", &b"one"[..])]));
        // A known value, so the hub and runner can never drift apart.
        assert_eq!(tree_hash([("x", &b""[..])]), {
            let empty = hex(&Sha256::digest(b""));
            hex(&Sha256::digest(format!("x\0{empty}\n").as_bytes()))
        });
    }

    #[test]
    fn paths_stay_inside_the_add_on() {
        for ok in [
            "server.mjs",
            "lib/a.js",
            "node_modules/@scope/pkg/index.js",
            "a+b.txt",
        ] {
            assert!(check_path(ok), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "./a",
            "a\\b",
            "a b",
            "é.txt",
            "a/",
        ] {
            assert!(!check_path(bad), "{bad}");
        }
    }

    #[test]
    fn paths_are_names_windows_keeps_as_written() {
        for ok in [
            "console.js",
            "lib/conf.txt",
            "com.txt",
            "com10.txt",
            "lpt0",
            "auxiliary/x",
            "nul-check.js",
            "a.b.c",
            ".hidden",
        ] {
            assert!(check_path(ok), "{ok}");
        }
        for bad in [
            "CON",
            "con",
            "Con.txt",
            "lib/prn.js",
            "aux.tar.gz",
            "NUL",
            "nul.txt",
            "COM1",
            "com9.log",
            "lpt1",
            "Lpt5.txt/x",
            "a/LPT3",
            "a.",
            "lib./x",
            "a/b.",
            "a ",
            "a\\b",
            "c:x",
            "C:/x",
            "a<b",
            "a>b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
        ] {
            assert!(!check_path(bad), "{bad}");
        }
    }

    #[test]
    fn ids_and_tool_names_have_one_plain_shape() {
        for ok in ["example.tickets", "acme-co.word-count", "a.b"] {
            assert!(check_id(ok), "{ok}");
        }
        for bad in [
            "tickets",
            "Example.tickets",
            ".a.b",
            "a..b",
            "a.-b",
            "a.b.",
            "a_b.c",
            "x",
        ] {
            assert!(!check_id(bad), "{bad}");
        }
        for ok in ["words", "issue-tracker", "t2"] {
            assert!(check_tool_name(ok), "{ok}");
        }
        for bad in [
            "",
            "Words",
            "2t",
            "a_b",
            "a__b",
            "browser",
            "decurion",
            "end-",
            "x".repeat(31).as_str(),
        ] {
            assert!(!check_tool_name(bad), "{bad}");
        }
    }

    #[test]
    fn messaging_and_deploy_sites_are_hard_stops() {
        assert_eq!(hard_stop_site("hooks.slack.com"), Some("slack.com"));
        assert_eq!(hard_stop_site("slack.com"), Some("slack.com"));
        assert_eq!(hard_stop_site("api.vercel.com"), Some("vercel.com"));
        // A whole ending that covers one is refused too.
        assert_eq!(
            hard_stop_site("googleapis.com"),
            Some("gmail.googleapis.com")
        );
        assert_eq!(hard_stop_site("api.example-tracker.com"), None);
        assert_eq!(hard_stop_site("notslack.com"), None);
        assert!(HARD_STOP_SITES
            .iter()
            .all(|s| crate::sites::normalize(s).as_deref() == Some(*s)));
    }
}
