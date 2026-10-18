//! How "Run it" starts an app: which commands install it and start its dev
//! server, and where that server listens. Nothing here does any I/O beyond
//! reading a few small files from the preview's own copy of the code, so it
//! can be tested directly; `main.rs` runs the boxes.
//!
//! An app says how it starts in `.decurion/run.json` (agents are told to keep
//! it up to date). Without one, a few common shapes are recognised: a
//! `package.json` with a `dev`, `start` or `serve` script, or a plain
//! `index.html`. The commands only ever run inside the preview's sealed box,
//! exactly as an agent would run them, and go in on its stdin, never on a
//! command line on the PC. The files are written by agents, so they are
//! read capped, never through a link, and anything odd in them is refused.

use serde_json::Value;
use std::path::Path;

/// Where an app keeps its start recipe, relative to the top of its code.
pub const RUN_FILE: &str = ".decurion/run.json";

/// The longest a recipe file, or a `package.json`, may be.
const MAX_FILE: u64 = 1 << 20;
/// The longest one command may be.
const MAX_COMMAND: usize = 1000;
/// The longest start page path.
const MAX_PATH: usize = 500;

/// The port a plain `index.html` is served on.
const STATIC_PORT: u16 = 8080;
/// A pinned static file server for apps that are just pages.
const STATIC_SERVER: &str = "npx --yes http-server@14.1.1 -p 8080 -c-1 -a 127.0.0.1 .";

/// What the preview's box runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    /// Installs what the app needs, if anything.
    pub install: Option<String>,
    /// Starts the dev server and keeps running.
    pub start: String,
    /// Where it listens, if known up front. Otherwise it is read from what
    /// the server prints (see [`find_port`]).
    pub port: Option<u16>,
    /// The page to open, starting with `/`.
    pub path: String,
    /// The app's own recipe file, rather than a guess.
    pub from_file: bool,
}

/// What the owner is told when nothing says how to start the app.
pub const NO_RECIPE: &str = "Decurion couldn't tell how to start this app. Ask an agent: \
\"Add a Run it recipe so Decurion can start this app\", then tap Run it again.";

/// Works out how to start the app in `dir`. The error is for the owner.
pub fn recipe(dir: &Path) -> Result<Recipe, String> {
    if let Some(text) = read_small(&dir.join(RUN_FILE))? {
        return from_run_file(&text);
    }
    if let Some(text) = read_small(&dir.join("package.json"))? {
        let pkg: Value = serde_json::from_str(&text)
            .map_err(|_| "The app's package.json could not be read. Ask an agent to fix it.")?;
        if let Some(r) = from_package(&pkg, dir) {
            return Ok(r);
        }
    }
    if is_plain_file(&dir.join("index.html")) {
        return Ok(Recipe {
            install: None,
            start: STATIC_SERVER.to_owned(),
            port: Some(STATIC_PORT),
            path: "/".to_owned(),
            from_file: false,
        });
    }
    Err(NO_RECIPE.to_owned())
}

/// A regular file, not a link to somewhere else on the PC.
fn is_plain_file(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file())
}

/// A small file's text, or `None` if there is no such file. A link, or a
/// file too big to be a recipe, is refused rather than followed.
fn read_small(p: &Path) -> Result<Option<String>, String> {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
    let name = name.unwrap_or_default();
    let m = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if !m.file_type().is_file() || m.len() > MAX_FILE {
        return Err(format!(
            "The app's {name} is not a plain, small file, so Decurion won't read it. Ask an agent to fix it."
        ));
    }
    std::fs::read_to_string(p)
        .map(Some)
        .map_err(|_| format!("The app's {name} could not be read. Ask an agent to fix it."))
}

fn from_run_file(text: &str) -> Result<Recipe, String> {
    const BAD: &str =
        "The app's Run it recipe (.decurion/run.json) is not right. Ask an agent to fix it.";
    let v: Value = serde_json::from_str(text).map_err(|_| BAD.to_owned())?;
    let obj = v.as_object().ok_or(BAD)?;
    // Unknown keys are refused, so a typo is noticed instead of ignored.
    if obj
        .keys()
        .any(|k| !matches!(k.as_str(), "install" | "start" | "port" | "path"))
    {
        return Err(BAD.to_owned());
    }
    let start = obj
        .get("start")
        .and_then(Value::as_str)
        .and_then(command)
        .ok_or(BAD)?;
    let install = match obj.get("install") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().and_then(command).ok_or(BAD)?),
    };
    let port = match obj.get("port") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|p| u16::try_from(p).ok())
                .filter(|p| *p > 0)
                .ok_or(BAD)?,
        ),
    };
    let path = match obj.get("path") {
        None | Some(Value::Null) => "/".to_owned(),
        Some(v) => v.as_str().filter(|p| page_path(p)).ok_or(BAD)?.to_owned(),
    };
    Ok(Recipe {
        install,
        start,
        port,
        path,
        from_file: true,
    })
}

/// One command on one line: it goes into a script line by line.
fn command(c: &str) -> Option<String> {
    let c = c.trim();
    let ok = !c.is_empty() && c.len() <= MAX_COMMAND && !c.chars().any(char::is_control);
    ok.then(|| c.to_owned())
}

/// A path on the app's own server: `/`, `/admin`, `/shop?x=1`.
fn page_path(p: &str) -> bool {
    p.starts_with('/')
        && !p.starts_with("//")
        && p.len() <= MAX_PATH
        && !p
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
}

/// A Node app with a script that starts a server.
fn from_package(pkg: &Value, dir: &Path) -> Option<Recipe> {
    let scripts = pkg.get("scripts")?.as_object()?;
    let script = ["dev", "start", "serve"]
        .into_iter()
        .find(|s| scripts.get(*s).and_then(Value::as_str).is_some())?;
    // The lock file says which package manager the app uses. Corepack runs
    // pnpm and yarn without installing them (it fetches them from the npm
    // registry, which every box may reach).
    let (install, run) = if is_plain_file(&dir.join("pnpm-lock.yaml")) {
        ("corepack pnpm install", "corepack pnpm run")
    } else if is_plain_file(&dir.join("yarn.lock")) {
        ("corepack yarn install", "corepack yarn run")
    } else {
        ("npm install --no-audit --no-fund", "npm run")
    };
    Some(Recipe {
        install: Some(install.to_owned()),
        start: format!("{run} {script}"),
        port: None,
        path: "/".to_owned(),
        from_file: false,
    })
}

/// Lines the start script prints so the runner can tell the owner where it
/// has got to. The app could print them too; that only changes the progress
/// line shown.
pub const INSTALLING_MARK: &str = "@@decurion-preview: installing";
pub const STARTING_MARK: &str = "@@decurion-preview: starting";

/// Where the start script has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Installing,
    Starting,
}

pub fn stage(line: &str) -> Option<Stage> {
    match line.trim() {
        INSTALLING_MARK => Some(Stage::Installing),
        STARTING_MARK => Some(Stage::Starting),
        _ => None,
    }
}

/// The script the preview's box reads on stdin. Every command reads nothing
/// more from stdin (`</dev/null`), so none can swallow the rest of the
/// script. Colours are turned off so the server's address can be read, and
/// no server tries to open a browser of its own.
pub fn script(r: &Recipe) -> String {
    let mut s = String::from(
        "set -e\ncd /work\n\
export NO_COLOR=1 FORCE_COLOR=0 BROWSER=none COREPACK_ENABLE_DOWNLOAD_PROMPT=0\n",
    );
    if let Some(p) = r.port {
        s.push_str(&format!("export PORT={p}\n"));
    }
    if let Some(i) = &r.install {
        s.push_str(&format!("echo '{INSTALLING_MARK}'\n{i} </dev/null\n"));
    }
    s.push_str(&format!("echo '{STARTING_MARK}'\n{} </dev/null\n", r.start));
    s
}

/// The address the preview's browser opens: always the box's own
/// localhost, whatever the app printed.
pub fn url(port: u16, path: &str) -> String {
    format!("http://localhost:{port}{path}")
}

/// Takes out terminal colour codes, which servers put in the middle of
/// their address (`http://localhost:\x1b[1m5173\x1b[22m/`).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            // Parameters and middle bytes, then one final byte.
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// The port a dev server says it listens on, from one line of its output:
/// an address on this machine (`http://localhost:5173/`, `127.0.0.1:3000`,
/// `0.0.0.0:8000`, `[::1]:4321`), or "listening on port 3000".
pub fn find_port(line: &str) -> Option<u16> {
    let line = strip_ansi(line);
    for host in ["localhost:", "127.0.0.1:", "0.0.0.0:", "[::1]:", "[::]:"] {
        let mut rest = line.as_str();
        while let Some(i) = rest.find(host) {
            rest = &rest[i + host.len()..];
            if let Some(p) = leading_port(rest) {
                return Some(p);
            }
        }
    }
    let lower = line.to_ascii_lowercase();
    if lower.contains("listen") {
        if let Some(i) = lower.find("port ") {
            return leading_port(&lower[i + 5..]);
        }
    }
    None
}

fn leading_port(s: &str) -> Option<u16> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 5 {
        return None;
    }
    digits.parse().ok().filter(|p| *p > 0)
}

/// What the owner is told while the app gets ready, on the preview's waiting
/// page.
pub const GETTING_CODE: &str = "Getting the code.";
pub const INSTALLING: &str =
    "Installing what the app needs. The first time can take a few minutes.";
pub const STARTING: &str = "Starting the app.";
pub const OPENING: &str = "Opening it.";

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(files: &[(&str, &str)]) -> tempdir::Dir {
        let d = tempdir::Dir::new();
        for (name, text) in files {
            let p = d.path().join(name);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, text).expect("write");
        }
        d
    }

    /// A folder deleted when dropped; no extra crate for one test module.
    mod tempdir {
        pub struct Dir(std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!(
                    "decurion-preview-test-{}",
                    uuid::Uuid::new_v4().simple()
                ));
                std::fs::create_dir_all(&p).expect("temp dir");
                Dir(p)
            }
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn the_apps_own_recipe_comes_first() {
        let d = dir(&[
            (
                ".decurion/run.json",
                r#"{"install": "npm ci", "start": "npm run dev -- --port 5180", "port": 5180, "path": "/admin"}"#,
            ),
            ("package.json", r#"{"scripts": {"dev": "vite"}}"#),
        ]);
        let r = recipe(d.path()).expect("recipe");
        assert_eq!(r.install.as_deref(), Some("npm ci"));
        assert_eq!(r.start, "npm run dev -- --port 5180");
        assert_eq!(r.port, Some(5180));
        assert_eq!(r.path, "/admin");
        assert!(r.from_file);
    }

    #[test]
    fn a_bad_recipe_is_refused_not_guessed_around() {
        for bad in [
            "not json",
            r#"{"install": "npm ci"}"#,
            r#"{"start": ""}"#,
            r#"{"start": "npm run dev\nrm -rf /"}"#,
            r#"{"start": "npm run dev", "port": 70000}"#,
            r#"{"start": "npm run dev", "port": "3000"}"#,
            r#"{"start": "npm run dev", "path": "//evil.example"}"#,
            r#"{"start": "npm run dev", "path": "admin"}"#,
            r#"{"start": "npm run dev", "srart": "typo"}"#,
        ] {
            let d = dir(&[(".decurion/run.json", bad)]);
            let e = recipe(d.path()).expect_err(bad);
            assert!(e.contains("run.json"), "{bad}: {e}");
        }
    }

    #[test]
    fn common_node_apps_are_recognised() {
        let d = dir(&[(
            "package.json",
            r#"{"scripts": {"build": "x", "start": "node server.js", "dev": "vite"}}"#,
        )]);
        let r = recipe(d.path()).expect("npm");
        assert_eq!(
            r.install.as_deref(),
            Some("npm install --no-audit --no-fund")
        );
        assert_eq!(r.start, "npm run dev");
        assert_eq!(r.port, None);
        assert!(!r.from_file);

        let d = dir(&[
            ("package.json", r#"{"scripts": {"start": "next start"}}"#),
            ("pnpm-lock.yaml", ""),
        ]);
        let r = recipe(d.path()).expect("pnpm");
        assert_eq!(r.install.as_deref(), Some("corepack pnpm install"));
        assert_eq!(r.start, "corepack pnpm run start");

        let d = dir(&[
            ("package.json", r#"{"scripts": {"serve": "x"}}"#),
            ("yarn.lock", ""),
        ]);
        assert_eq!(
            recipe(d.path()).expect("yarn").start,
            "corepack yarn run serve"
        );
    }

    #[test]
    fn a_plain_page_is_served_and_anything_else_asks_for_a_recipe() {
        let d = dir(&[("index.html", "<h1>hi</h1>")]);
        let r = recipe(d.path()).expect("static");
        assert_eq!(r.port, Some(8080));
        assert!(r.start.contains("http-server@14.1.1"));

        let d = dir(&[("package.json", r#"{"scripts": {"test": "jest"}}"#)]);
        assert_eq!(recipe(d.path()), Err(NO_RECIPE.to_owned()));
        let d = dir(&[("main.py", "print(1)")]);
        assert_eq!(recipe(d.path()), Err(NO_RECIPE.to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn a_recipe_that_is_a_link_is_never_followed() {
        let outside = dir(&[("keys.json", r#"{"start": "cat /etc/passwd"}"#)]);
        let d = dir(&[]);
        std::fs::create_dir_all(d.path().join(".decurion")).expect("mkdir");
        std::os::unix::fs::symlink(outside.path().join("keys.json"), d.path().join(RUN_FILE))
            .expect("link");
        assert!(recipe(d.path()).is_err());
    }

    #[test]
    fn the_script_keeps_stdin_for_itself() {
        let r = Recipe {
            install: Some("npm ci".into()),
            start: "npm run dev".into(),
            port: Some(3000),
            path: "/".into(),
            from_file: true,
        };
        let s = script(&r);
        assert!(s.starts_with("set -e\ncd /work\n"));
        assert!(s.contains("export PORT=3000\n"));
        assert!(s.contains("npm ci </dev/null\n"));
        assert!(s.ends_with("npm run dev </dev/null\n"));
        let marks: Vec<Stage> = s
            .lines()
            .filter_map(|l| {
                l.strip_prefix("echo '")
                    .and_then(|l| l.strip_suffix('\''))
                    .and_then(stage)
            })
            .collect();
        assert_eq!(marks, [Stage::Installing, Stage::Starting]);
    }

    #[test]
    fn ports_are_read_from_what_servers_print() {
        for (line, port) in [
            (
                "  ➜  Local:   http://localhost:\x1b[1m5173\x1b[22m/",
                Some(5173),
            ),
            ("   - Local:        http://localhost:3000", Some(3000)),
            ("Available on:\n  http://127.0.0.1:8080", Some(8080)),
            ("Server running at http://0.0.0.0:8000/", Some(8000)),
            ("listening on [::1]:4321", Some(4321)),
            ("Example app listening on port 3001", Some(3001)),
            ("Connecting to the database on port 5432", None),
            ("localhost:abc and later localhost:4000", Some(4000)),
            ("http://localhost:999999", None),
            ("nothing here", None),
        ] {
            assert_eq!(find_port(line), port, "{line:?}");
        }
        assert_eq!(url(5173, "/admin"), "http://localhost:5173/admin");
    }
}
