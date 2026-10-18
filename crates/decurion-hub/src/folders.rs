//! Lets the owner pick an app's folder on the PC from a list, on the board or
//! the phone, instead of typing its path (`GET /api/folders`).
//!
//! Rules that must stay true:
//! - Owner only. A runner key gets 403, and cloud workers never reach it
//!   (`cloud_reach` refuses every route it does not list).
//! - It shows only folder names, never files, sizes or contents.
//! - It shows only folders inside the safe starting places: the usual code
//!   folders in the owner's home folder (Projects, Documents, source\repos
//!   and a few more), the folders that hold apps already added, and any
//!   folders listed in `DECURION_FOLDER_ROOTS`. Never a drive root, never the
//!   whole home folder or a folder above it, and never Decurion's own data
//!   folder (its keys, database and vault) or anything inside it.
//! - Every asked-for path is resolved first (`..`, links and junctions), and
//!   is refused unless the resolved folder is inside a starting place. Hidden
//!   folders, dot folders and build clutter (node_modules, target, AppData)
//!   are neither listed nor opened, and links are not followed in a listing.
//! - On Windows, the `\\?\` prefix that resolving adds is taken off every
//!   path, starting places and asked-for paths alike, so they compare the
//!   same way and read the way the owner types them.

use crate::{forbidden, AppState, Caller};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// Folders in the owner's home folder that often hold code. Only the ones
/// that exist are offered.
const COMMON: &[&str] = &[
    "Projects",
    "Documents",
    "Desktop",
    "source/repos",
    "repos",
    "code",
    "dev",
    "src",
    "GitHub",
    "git",
    "Developer",
    "OneDrive/Documents",
    "OneDrive/Desktop",
];

/// Folders that are never code to pick, compared without regard to case.
const NOISE: &[&str] = &["node_modules", "target", "appdata", "__pycache__"];

/// At most this many folders in one answer; the rest are left out and the
/// answer says so.
pub const MAX_FOLDERS: usize = 500;

/// A folder with more entries than this is not read to the end.
const MAX_SCANNED: usize = 20_000;

/// Where the picker may look, fixed when the hub starts.
#[derive(Clone, Debug, Default)]
pub struct Places {
    /// The owner's home folder, resolved.
    home: Option<PathBuf>,
    /// Starting places other than apps' folders, resolved and checked.
    roots: Vec<PathBuf>,
    /// Folders never shown, with everything inside them: Decurion's data.
    hidden: Vec<PathBuf>,
}

impl Places {
    /// The places on this PC: the usual code folders under the home folder
    /// (`USERPROFILE` on Windows, `HOME` elsewhere), plus any folders in
    /// `DECURION_FOLDER_ROOTS` (separated by `;` on Windows and `:`
    /// elsewhere). `data` is the hub's own folder, which is never shown.
    pub fn for_this_pc(data: &Path) -> Self {
        let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let home = std::env::var_os(home_var).map(PathBuf::from);
        let extra = std::env::var_os("DECURION_FOLDER_ROOTS")
            .map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_default();
        Self::new(home, extra, vec![data.to_owned()])
    }

    /// Places from a given home folder, extra starting places and folders
    /// never to show. Paths that do not exist, or break the rules in the
    /// module header, are dropped.
    pub fn new(home: Option<PathBuf>, extra: Vec<PathBuf>, hidden: Vec<PathBuf>) -> Self {
        let home = home.filter(|h| h.is_absolute()).and_then(|h| resolve(&h));
        let hidden = hidden.iter().filter_map(|h| resolve(h)).collect();
        let mut p = Places {
            home,
            roots: Vec::new(),
            hidden,
        };
        let mut wanted: Vec<PathBuf> = Vec::new();
        if let Some(h) = &p.home {
            wanted.extend(COMMON.iter().map(|c| h.join(c)));
        }
        wanted.extend(extra.into_iter().filter(|e| e.is_absolute()));
        for w in wanted {
            if let Some(r) = resolve(&w).filter(|r| p.safe_root(r)) {
                if !p.roots.contains(&r) {
                    p.roots.push(r);
                }
            }
        }
        p
    }

    /// Whether `r` (resolved) may be a starting place.
    fn safe_root(&self, r: &Path) -> bool {
        r.is_dir()
            && r.parent().is_some()
            && self.home.as_ref().is_none_or(|h| !h.starts_with(r))
            && !self.is_hidden(r)
    }

    fn is_hidden(&self, p: &Path) -> bool {
        self.hidden.iter().any(|h| p.starts_with(h))
    }

    /// The starting places now: the fixed ones, then the folders that hold
    /// the apps in `apps` (their saved paths).
    fn roots_with(&self, apps: &[String]) -> Vec<PathBuf> {
        let mut roots = self.roots.clone();
        let mut parents: Vec<PathBuf> = apps
            .iter()
            .filter_map(|a| resolve(Path::new(a)))
            .filter_map(|a| a.parent().map(Path::to_owned))
            .filter(|p| self.safe_root(p))
            .collect();
        parents.sort();
        for p in parents {
            if !roots.contains(&p) {
                roots.push(p);
            }
        }
        roots
    }

    /// The answer for `GET /api/folders`: the starting places when `path` is
    /// `None`, else the folders inside `path`.
    pub fn answer(&self, path: Option<&str>, apps: &[String]) -> Result<Listing, Refusal> {
        let roots = self.roots_with(apps);
        let Some(asked) = path.map(str::trim).filter(|p| !p.is_empty()) else {
            let (folders, cut) = entries(roots.iter().cloned());
            return Ok(Listing {
                path: None,
                name: None,
                git: false,
                up: None,
                crumbs: Vec::new(),
                folders,
                cut,
            });
        };
        if asked.len() > 4096 || asked.contains('\0') || !Path::new(asked).is_absolute() {
            return Err(Refusal::Bad);
        }
        let dir = resolve(Path::new(asked)).ok_or(Refusal::Missing)?;
        if !dir.is_dir() {
            return Err(Refusal::Missing);
        }
        if self.is_hidden(&dir) {
            return Err(Refusal::Hidden);
        }
        // The outermost starting place holding it, so Up goes as far as it can.
        let root = roots
            .iter()
            .filter(|r| dir.starts_with(r))
            .min_by_key(|r| r.components().count())
            .ok_or(Refusal::Outside)?;
        let rel = dir.strip_prefix(root).map_err(|_| Refusal::Outside)?;
        let mut crumbs = vec![crumb(root).ok_or(Refusal::Missing)?];
        let mut at = root.clone();
        for c in rel.components() {
            let Component::Normal(name) = c else {
                return Err(Refusal::Outside);
            };
            // Only folders the list would show can be opened.
            if name.to_str().is_none_or(skipped) {
                return Err(Refusal::Outside);
            }
            at.push(name);
            crumbs.push(crumb(&at).ok_or(Refusal::Missing)?);
        }
        let listed = std::fs::read_dir(&dir).map_err(|_| Refusal::Missing)?;
        let (folders, cut) = entries(
            listed
                .take(MAX_SCANNED)
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter(|e| !hidden_by_system(e))
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| !skipped(n))
                })
                .filter(|p| !self.is_hidden(p)),
        );
        let up = crumbs.len().checked_sub(2).map(|i| crumbs[i].path.clone());
        let here = crumbs.last().expect("crumbs start with the starting place");
        Ok(Listing {
            path: Some(here.path.clone()),
            name: Some(here.name.clone()),
            git: is_git(&dir),
            up,
            crumbs,
            folders,
            cut,
        })
    }
}

/// What `GET /api/folders` returns.
#[derive(Debug, Serialize)]
pub struct Listing {
    /// The folder shown, or `None` for the starting places.
    pub path: Option<String>,
    pub name: Option<String>,
    /// Whether the folder shown is a git folder (holds a `.git`).
    pub git: bool,
    /// The folder above, while it is still inside a starting place. `None`
    /// means Up goes back to the starting places.
    pub up: Option<String>,
    /// From the starting place down to the folder shown.
    pub crumbs: Vec<Crumb>,
    /// The folders inside, sorted by name.
    pub folders: Vec<Folder>,
    /// Some folders were left out because there were too many.
    pub cut: bool,
}

#[derive(Debug, Serialize)]
pub struct Crumb {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct Folder {
    pub name: String,
    pub path: String,
    pub git: bool,
}

/// Why a folder is not shown, in words for the owner.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Bad,
    Missing,
    Outside,
    Hidden,
}

impl Refusal {
    pub fn text(&self) -> &'static str {
        match self {
            Refusal::Bad => "Pick a folder from the list.",
            Refusal::Missing => {
                "That folder can't be opened. It may have been moved or removed. Pick another one."
            }
            Refusal::Outside => {
                "Decurion only shows folders inside your Projects, Documents and other code folders, and the folders your apps are in."
            }
            Refusal::Hidden => "That folder holds Decurion's own data, so Decurion does not show it.",
        }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let code = match self {
            Refusal::Bad | Refusal::Missing => StatusCode::BAD_REQUEST,
            Refusal::Outside | Refusal::Hidden => StatusCode::FORBIDDEN,
        };
        (code, self.text()).into_response()
    }
}

/// Sorted entries for `dirs`, at most [`MAX_FOLDERS`]; true if some were cut.
fn entries(dirs: impl Iterator<Item = PathBuf>) -> (Vec<Folder>, bool) {
    let mut all: Vec<Folder> = dirs
        .filter_map(|p| {
            let c = crumb(&p)?;
            Some(Folder {
                git: is_git(&p),
                name: c.name,
                path: c.path,
            })
        })
        .collect();
    all.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
    let cut = all.len() > MAX_FOLDERS;
    all.truncate(MAX_FOLDERS);
    (all, cut)
}

/// A folder's name and path as text, or `None` if either is not valid text
/// (it could not be sent back and opened again).
fn crumb(p: &Path) -> Option<Crumb> {
    Some(Crumb {
        name: p.file_name()?.to_str()?.to_owned(),
        path: p.to_str()?.to_owned(),
    })
}

/// Dot folders, `$` folders (the recycle bin) and build clutter.
fn skipped(name: &str) -> bool {
    name.starts_with('.')
        || name.starts_with('$')
        || NOISE.iter().any(|n| n.eq_ignore_ascii_case(name))
}

fn is_git(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).is_ok()
}

/// Folders Windows marks hidden or system.
#[cfg(windows)]
fn hidden_by_system(e: &std::fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    e.metadata()
        .is_ok_and(|m| m.file_attributes() & (HIDDEN | SYSTEM) != 0)
}

#[cfg(not(windows))]
fn hidden_by_system(_: &std::fs::DirEntry) -> bool {
    false
}

/// The real, absolute path of `p`, with links and `..` resolved, or `None`
/// if it does not exist.
fn resolve(p: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(p).ok().map(plain)
}

/// Takes the `\\?\` prefix off a Windows path when the plain form means the
/// same folder, so every path compares and reads the same way.
fn plain(p: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return p;
    }
    match p.to_str().and_then(strip_verbatim) {
        Some(s) => PathBuf::from(s),
        None => p,
    }
}

/// `\\?\C:\x` is `C:\x` and `\\?\UNC\srv\share` is `\\srv\share`; anything
/// else (a device path, or no prefix) is left as it is.
fn strip_verbatim(s: &str) -> Option<String> {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    let rest = s.strip_prefix(r"\\?\")?;
    let b = rest.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\')
        .then(|| rest.to_owned())
}

#[derive(Deserialize)]
pub(crate) struct FolderQuery {
    path: Option<String>,
}

/// `GET /api/folders[?path=...]`.
pub(crate) async fn list(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Query(q): Query<FolderQuery>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("browsing the PC's folders");
    }
    let Some(places) = st.folders.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Choosing a folder is not set up on this hub. Type the folder's path instead.",
        )
            .into_response();
    };
    let apps: Vec<String> = match st.store.apps().await {
        Ok(a) => a.into_iter().map(|a| a.repo_path).collect(),
        Err(e) => return crate::ApiError::from(e).into_response(),
    };
    let answer = tokio::task::spawn_blocking(move || places.answer(q.path.as_deref(), &apps)).await;
    match answer {
        Ok(Ok(l)) => Json(l).into_response(),
        Ok(Err(r)) => {
            tracing::debug!(refusal = ?r, "folder not shown");
            r.into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "folder listing task failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_clutter_and_dot_folders() {
        for n in [
            ".git",
            ".ssh",
            "node_modules",
            "Target",
            "AppData",
            "$RECYCLE.BIN",
        ] {
            assert!(skipped(n), "{n}");
        }
        for n in ["portal", "my app", "targets"] {
            assert!(!skipped(n), "{n}");
        }
    }

    #[test]
    fn windows_prefixes_come_off() {
        assert_eq!(
            strip_verbatim(r"\\?\C:\Users\me").as_deref(),
            Some(r"C:\Users\me")
        );
        assert_eq!(
            strip_verbatim(r"\\?\UNC\srv\share\x").as_deref(),
            Some(r"\\srv\share\x")
        );
        assert_eq!(strip_verbatim(r"C:\x"), None);
        assert_eq!(strip_verbatim(r"\\?\GLOBALROOT\Device\x"), None);
    }
}
