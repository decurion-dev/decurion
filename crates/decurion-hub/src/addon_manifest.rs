//! Reading an add-on's `decurion-addon.toml` (ADR 0001, v1: tools only),
//! and the plain sentences the owner's one approval shows.
//!
//! What must stay true:
//! - **The owner sees every permission.** The permission vocabulary is a
//!   closed list. An unknown key inside `permissions` or `secrets` refuses
//!   the add-on, because Decurion cannot explain a permission it does not
//!   know. Unknown keys elsewhere only add a warning.
//! - **Fixed words.** Each permission reads as one fixed sentence
//!   ([`can_lines`]); the author's summary goes inside it, never instead.
//! - **Refuse what v1 cannot wall in.** Agent types, checks, tools that run
//!   beside the hub, tools in images of their own, stdio tools, access to
//!   the app's code and every hard-stop effect are refused with a plain
//!   reason, rather than half-supported.
//! - Errors are owner-facing: short, plain, no jargon beyond the field the
//!   author must fix.

use decurion_proto::addon::{self, KeyUse, ToolUse};
use decurion_proto::sites;
use serde::{Deserialize, Serialize};

/// Most tools one add-on may declare.
pub const MAX_TOOLS: usize = 4;
/// Most websites one tool may name.
pub const MAX_TOOL_SITES: usize = 10;

/// A manifest that passed every check.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub authors: Vec<String>,
    pub homepage: Option<String>,
    pub tools: Vec<Tool>,
    /// Things the hub ignored, in plain words, shown on the approval card.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub run: ToolUse,
    pub effect: Effect,
}

/// What a tool does to the world. The other effects in ADR 0001
/// (`outbound_message`, `deploy`, `production_data`, `spend`) are hard
/// stops and are refused in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Read,
    Write,
}

/// What the owner agreed to, kept with the add-on. An update is checked
/// against it ([`new_lines`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Grants {
    pub tools: Vec<GrantedTool>,
    pub sites: Vec<String>,
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrantedTool {
    pub name: String,
    pub effect: Effect,
}

impl Manifest {
    pub fn grants(&self) -> Grants {
        grants_of(&self.tools)
    }

    /// The app test keys its tools read.
    pub fn keys(&self) -> Vec<String> {
        self.grants().keys
    }
}

fn grants_of(tools: &[Tool]) -> Grants {
    let mut g = Grants::default();
    for t in tools {
        g.tools.push(GrantedTool {
            name: t.run.name.clone(),
            effect: t.effect,
        });
        for s in &t.run.sites {
            if !g.sites.contains(s) {
                g.sites.push(s.clone());
            }
        }
        if let Some(k) = &t.run.key {
            if !g.keys.contains(&k.test_key) {
                g.keys.push(k.test_key.clone());
            }
        }
    }
    g
}

type Table = toml::Table;
type R<T> = std::result::Result<T, String>;

/// Reads and checks a manifest's text.
pub fn parse(text: &str) -> R<Manifest> {
    let top: Table = toml::from_str(text).map_err(|e| {
        format!(
            "{} is not valid TOML: {}",
            addon::MANIFEST,
            e.message().trim()
        )
    })?;
    let mut warnings = Vec::new();
    match top.get("decurion_api") {
        Some(toml::Value::String(v)) if v == addon::API => {}
        Some(toml::Value::String(v)) => {
            return Err(format!(
                "This add-on is written for Decurion add-on version {v}. This Decurion runs version {}. Look for a version of the add-on made for it.",
                addon::API
            ))
        }
        _ => {
            return Err(format!(
                "{} must start with decurion_api = \"{}\".",
                addon::MANIFEST,
                addon::API
            ))
        }
    }
    if top.contains_key("agent") {
        return Err("This add-on adds a new kind of agent. This version of Decurion can only add tools for agents, so it cannot add this one yet.".into());
    }
    if top.contains_key("check") {
        return Err("This add-on adds an automatic check. This version of Decurion can only add tools for agents, so it cannot add this one yet.".into());
    }
    for k in top.keys() {
        if !matches!(k.as_str(), "decurion_api" | "addon" | "tool") {
            warnings.push(format!("Decurion does not use \"{k}\" and ignored it."));
        }
    }
    let info = table(&top, "addon", "")?.ok_or_else(|| {
        format!(
            "{} needs an [addon] part with its id, name and version.",
            addon::MANIFEST
        )
    })?;
    for k in info.keys() {
        if !matches!(
            k.as_str(),
            "id" | "name" | "version" | "description" | "authors" | "license" | "homepage"
        ) {
            warnings.push(format!("Decurion does not use addon.{k} and ignored it."));
        }
    }
    let id = string(info, "id", "addon.")?.unwrap_or_default();
    if !addon::check_id(&id) {
        return Err("addon.id must be lowercase letters and digits joined by dots or dashes, starting with something that is yours, like example.tickets.".into());
    }
    let name = plain(string(info, "name", "addon.")?, "addon.name", 40)?;
    let version = string(info, "version", "addon.")?.unwrap_or_default();
    if !semver(&version) {
        return Err("addon.version must be a version like 1.2.0.".into());
    }
    let description = string(info, "description", "addon.")?.unwrap_or_default();
    if description.chars().count() > 300 || description.chars().any(char::is_control) {
        return Err("addon.description must be one line of at most 300 characters.".into());
    }
    let authors = match info.get("authors") {
        None => Vec::new(),
        Some(v) => strings(v, "addon.authors", 10, 100)?,
    };
    let homepage = string(info, "homepage", "addon.")?.filter(|h| {
        let ok = h.starts_with("https://") && h.len() <= 300 && !h.contains(char::is_whitespace);
        if !ok {
            warnings.push("addon.homepage is not an https link, so it is not shown.".into());
        }
        ok
    });

    let list = match top.get("tool") {
        Some(toml::Value::Array(a)) if !a.is_empty() => a,
        Some(_) => return Err("Each [[tool]] must be a table, written as [[tool]].".into()),
        None => {
            return Err("This add-on has no [[tool]]. Add at least one tool for agents.".into())
        }
    };
    if list.len() > MAX_TOOLS {
        return Err(format!("An add-on can have at most {MAX_TOOLS} tools."));
    }
    let mut tools: Vec<Tool> = Vec::new();
    for v in list {
        let toml::Value::Table(t) = v else {
            return Err("Each [[tool]] must be a table, written as [[tool]].".into());
        };
        let tool = parse_tool(t, &mut warnings)?;
        if tools.iter().any(|x| x.run.name == tool.run.name) {
            return Err(format!(
                "Two tools are named {}. Give each its own name.",
                tool.run.name
            ));
        }
        tools.push(tool);
    }
    Ok(Manifest {
        id,
        name,
        version,
        description,
        authors,
        homepage,
        tools,
        warnings,
    })
}

fn parse_tool(t: &Table, warnings: &mut Vec<String>) -> R<Tool> {
    let name = string(t, "name", "tool.")?.unwrap_or_default();
    if !addon::check_tool_name(&name) {
        return Err(format!(
            "tool.name {name:?} must be 1 to 30 lowercase letters, digits or dashes, starting with a letter, and not one of Decurion's own (browser, decurion)."
        ));
    }
    let at = |field: &str| format!("tool {name}: {field}");
    for k in t.keys() {
        if !matches!(
            k.as_str(),
            "name"
                | "summary"
                | "placement"
                | "transport"
                | "image"
                | "command"
                | "port"
                | "effect"
                | "expose"
                | "permissions"
                | "secrets"
                | "resources"
        ) {
            warnings.push(format!("Decurion does not use {} and ignored it.", at(k)));
        }
    }
    let summary = plain(string(t, "summary", "tool.")?, &at("summary"), 120)?;
    match string(t, "placement", "tool.")?.as_deref() {
        None | Some("box") => {}
        Some("hub") => {
            return Err(format!(
                "The tool {name} needs to run beside Decurion so it can keep a key away from agents. This version of Decurion cannot run tools that way yet."
            ))
        }
        Some(p) => return Err(format!("{} must be \"box\", not {p:?}.", at("placement"))),
    }
    match string(t, "transport", "tool.")?.as_deref() {
        None | Some("http") => {}
        Some("stdio") => {
            return Err(format!(
                "The tool {name} talks over stdio. This version of Decurion runs tools that serve MCP over HTTP only."
            ))
        }
        Some(p) => return Err(format!("{} must be \"http\", not {p:?}.", at("transport"))),
    }
    if t.contains_key("image") {
        return Err(format!(
            "The tool {name} runs from an image of its own. This version of Decurion runs add-on tools from the add-on's own files only."
        ));
    }
    if t.contains_key("port") {
        warnings.push(format!(
            "Decurion picks the port for the tool {name} itself and ignored tool.port. The tool reads it from {} or {{port}}.",
            addon::PORT_VAR
        ));
    }
    if t.contains_key("resources") {
        warnings.push(format!(
            "The tool {name} gets Decurion's usual limits for tool boxes; tool.resources was ignored."
        ));
    }
    let command = match t.get("command") {
        Some(v) => strings(v, &at("command"), 20, 500)?,
        None => Vec::new(),
    };
    check_command(&command).map_err(|e| format!("{}: {e}", at("command")))?;
    let mut effect = match string(t, "effect", "tool.")?.as_deref() {
        Some(e) => effect_of(&name, e)?,
        None => {
            return Err(format!(
                "Say what the tool {name} does with effect = \"read\" (it only looks things up) or effect = \"write\" (it changes things)."
            ))
        }
    };
    // Functions listed one by one: their effects count too, and a hard stop
    // among them refuses the add-on. v1 cannot hide functions left out.
    if let Some(v) = t.get("expose") {
        let toml::Value::Array(list) = v else {
            return Err(format!(
                "{} must be a list of [[tool.expose]] tables.",
                at("expose")
            ));
        };
        for f in list {
            let toml::Value::Table(f) = f else {
                return Err(format!(
                    "{} must be a list of [[tool.expose]] tables.",
                    at("expose")
                ));
            };
            for k in f.keys() {
                if !matches!(k.as_str(), "name" | "effect" | "summary") {
                    return Err(format!(
                        "{} has {k:?}, which Decurion does not know, so it cannot tell you what it means.",
                        at("expose")
                    ));
                }
            }
            if let Some(e) = string(f, "effect", "tool.expose.")? {
                effect = effect.max(effect_of(&name, &e)?);
            }
        }
        warnings.push(format!(
            "This version of Decurion gives agents every function the tool {name} has, not only the ones it lists."
        ));
    }
    let mut sites_out: Vec<String> = Vec::new();
    if let Some(p) = table(t, "permissions", "tool.")? {
        for (k, v) in p {
            match k.as_str() {
                "network" => {
                    for s in strings(v, &at("permissions.network"), MAX_TOOL_SITES, 300)? {
                        let Some(n) = sites::normalize(&s) else {
                            return Err(format!(
                                "{} has {s:?}, which is not a website name like api.example.com.",
                                at("permissions.network")
                            ));
                        };
                        if let Some(stop) = addon::hard_stop_site(&n) {
                            return Err(format!(
                                "The tool {name} talks to {n}. Messages and deploys through {stop} must wait for your OK every time, which Decurion cannot do for add-on tools yet, so it cannot be added."
                            ));
                        }
                        if !sites_out.contains(&n) && !sites::BUILT_IN.contains(&n.as_str()) {
                            sites_out.push(n);
                        }
                    }
                }
                "worktree" => match v.as_str() {
                    Some("none") => {}
                    _ => {
                        return Err(format!(
                            "The tool {name} asks to see your app's code. This version of Decurion never gives add-on tools your code."
                        ))
                    }
                },
                other => {
                    return Err(format!(
                        "{} has {other:?}, which Decurion does not know, so it cannot tell you what it allows.",
                        at("permissions")
                    ))
                }
            }
        }
    }
    let mut key = None;
    if let Some(sec) = table(t, "secrets", "tool.")? {
        if sec.len() > 1 {
            return Err(format!(
                "The tool {name} asks for {} saved keys. A tool can use at most one.",
                sec.len()
            ));
        }
        for (var, v) in sec {
            let Some(test_key) = v.as_str() else {
                return Err(format!(
                    "{} must name a saved test key, like {var} = \"TRACKER_TOKEN\".",
                    at("secrets")
                ));
            };
            decurion_vault::check_name(var)
                .map_err(|e| format!("{}: the variable {var}: {e}.", at("secrets")))?;
            decurion_vault::check_name(test_key).map_err(|e| {
                format!(
                    "{}: {test_key:?} must be the name of one of your test keys: {e}.",
                    at("secrets")
                )
            })?;
            key = Some(KeyUse {
                var: var.clone(),
                test_key: test_key.to_owned(),
            });
        }
    }
    Ok(Tool {
        run: ToolUse {
            name,
            summary,
            command,
            sites: sites_out,
            key,
        },
        effect,
    })
}

fn effect_of(tool: &str, e: &str) -> R<Effect> {
    match e {
        "read" => Ok(Effect::Read),
        "write" => Ok(Effect::Write),
        "outbound_message" | "deploy" | "production_data" | "spend" => {
            let what = match e {
                "outbound_message" => "sends messages to people",
                "deploy" => "deploys",
                "production_data" => "touches production data",
                _ => "spends money",
            };
            Err(format!(
                "The tool {tool} {what}. That must wait for your OK every time, which Decurion cannot do for add-on tools yet, so it cannot be added."
            ))
        }
        other => Err(format!(
            "The tool {tool} has effect {other:?}. Use \"read\" or \"write\"."
        )),
    }
}

/// The command a tool box runs: a program the box image has (`node`), by
/// its plain name, then its arguments, which usually name the add-on's own
/// file (`server.mjs`, read from the add-on's folder). It runs without a
/// shell, so arguments are passed as they are. Files of the add-on are
/// never run directly: they arrive without their executable bit, which the
/// tree hash does not cover.
fn check_command(c: &[String]) -> R<()> {
    let Some(program) = c.first() else {
        return Err("give the program and its arguments, like [\"node\", \"server.mjs\"].".into());
    };
    let shaped = (1..=40).contains(&program.len())
        && !program.starts_with(['-', '.'])
        && program
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !shaped {
        return Err(format!(
            "{program:?} must be a program the tool's box has, by its plain name, like node. Put the add-on's own file after it: [\"node\", \"server.mjs\"]."
        ));
    }
    if c.iter().any(|a| a.contains('\0')) {
        return Err("an argument holds a NUL character.".into());
    }
    Ok(())
}

/// A table under `key`, if there is one.
fn table<'a>(t: &'a Table, key: &str, prefix: &str) -> R<Option<&'a Table>> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::Table(x)) => Ok(Some(x)),
        Some(_) => Err(format!("{prefix}{key} must be a table.")),
    }
}

fn string(t: &Table, key: &str, prefix: &str) -> R<Option<String>> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s.trim().to_owned())),
        Some(_) => Err(format!("{prefix}{key} must be text in quotes.")),
    }
}

fn strings(v: &toml::Value, what: &str, max: usize, each: usize) -> R<Vec<String>> {
    let toml::Value::Array(a) = v else {
        return Err(format!("{what} must be a list, like [\"a\", \"b\"]."));
    };
    if a.len() > max {
        return Err(format!("{what} can list at most {max}."));
    }
    a.iter()
        .map(|x| match x.as_str() {
            Some(s) if s.len() <= each => Ok(s.to_owned()),
            Some(_) => Err(format!(
                "{what} has an entry longer than {each} characters."
            )),
            None => Err(format!("{what} must hold text in quotes only.")),
        })
        .collect()
}

/// One line of plain text, 1 to `max` characters.
fn plain(v: Option<String>, what: &str, max: usize) -> R<String> {
    match v {
        Some(s)
            if !s.is_empty() && s.chars().count() <= max && !s.chars().any(char::is_control) =>
        {
            Ok(s)
        }
        _ => Err(format!("{what} must be one line of 1 to {max} characters.")),
    }
}

/// `MAJOR.MINOR.PATCH`, with an optional `-pre` or `+build` part.
fn semver(v: &str) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or("");
    let rest = &v[core.len()..];
    let parts: Vec<&str> = core.split('.').collect();
    v.len() <= 40
        && parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
}

/// "a", "a and b", "a, b and c".
pub fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What every add-on can never do, on every approval card.
pub const NEVER: &str = "It can never merge to main, deploy, touch production data, spend past your cap, send messages to people, or see your Claude sign-in.";

/// What the add-on may do, one fixed sentence per permission.
pub fn can_lines(m: &Manifest) -> Vec<String> {
    lines(&m.tools)
}

/// The same sentences for an installed add-on, from what was approved.
pub fn installed_lines(tools: &[ToolUse], g: &Grants) -> Vec<String> {
    let tools: Vec<Tool> = tools
        .iter()
        .map(|t| Tool {
            run: t.clone(),
            effect: g
                .tools
                .iter()
                .find(|x| x.name == t.name)
                .map_or(Effect::Write, |x| x.effect),
        })
        .collect();
    lines(&tools)
}

fn lines(tools: &[Tool]) -> Vec<String> {
    let mut out = Vec::new();
    for t in tools {
        out.push(tool_line(&t.run));
    }
    out.extend(grant_lines(&grants_of(tools), tools));
    out.push("Runs in a sealed box of its own beside your agents. It gets none of your app's code and never sees your Claude sign-in.".into());
    out
}

fn tool_line(t: &ToolUse) -> String {
    format!(
        "Gives your agents a tool: {}.",
        t.summary.trim_end_matches('.')
    )
}

fn key_line(k: &str) -> String {
    format!("Uses your saved test key {k} in each app it is on. Like every test key, your agents can use it too, and Decurion hides it from what they report.")
}

fn sites_line(sites: &[String]) -> String {
    format!(
        "Talks to {}. Your agents can reach {} too while they work.",
        and_list(sites),
        if sites.len() == 1 {
            "that site"
        } else {
            "these sites"
        }
    )
}

fn write_line(sites: &[String]) -> String {
    if sites.is_empty() {
        "Can change things, not only look them up.".into()
    } else {
        format!("Can change things on {}.", and_list(sites))
    }
}

fn grant_lines(g: &Grants, tools: &[Tool]) -> Vec<String> {
    let mut out = Vec::new();
    if !g.sites.is_empty() {
        out.push(sites_line(&g.sites));
    }
    let mut write_sites: Vec<String> = Vec::new();
    let mut writes = false;
    for t in tools.iter().filter(|t| t.effect == Effect::Write) {
        writes = true;
        for s in &t.run.sites {
            if !write_sites.contains(s) {
                write_sites.push(s.clone());
            }
        }
    }
    if writes {
        out.push(write_line(&write_sites));
    }
    for k in &g.keys {
        out.push(key_line(k));
    }
    out
}

/// What an update asks for beyond what the owner approved before: new
/// tools, a tool that now changes things, websites not covered before, and
/// keys. Empty means the update needs no new approval.
pub fn new_lines(old: &Grants, m: &Manifest) -> Vec<String> {
    let new = m.grants();
    let mut out = Vec::new();
    for t in &m.tools {
        match old.tools.iter().find(|o| o.name == t.run.name) {
            None => out.push(tool_line(&t.run)),
            Some(o) if t.effect > o.effect => {
                out.push(format!(
                    "Its tool {} can now change things, not only look them up.",
                    t.run.name
                ));
            }
            Some(_) => {}
        }
    }
    let sites: Vec<String> = new
        .sites
        .iter()
        .filter(|s| !old.sites.iter().any(|o| sites::covers(o, s)))
        .cloned()
        .collect();
    if !sites.is_empty() {
        out.push(sites_line(&sites));
    }
    for k in new.keys.iter().filter(|k| !old.keys.contains(k)) {
        out.push(key_line(k));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
decurion_api = "1"

[addon]
id = "example.words"
name = "Word count"
version = "0.1.0"
description = "Counts words."

[[tool]]
name = "words"
summary = "Count the words in a piece of text"
command = ["node", "server.mjs", "--port", "{port}"]
effect = "read"

[tool.permissions]
network = ["https://API.Example.com/v1"]

[tool.secrets]
WORDS_TOKEN = "WORDS_TOKEN"
"#;

    #[test]
    fn a_good_manifest_reads_as_plain_sentences() {
        let m = parse(GOOD).expect("parses");
        assert_eq!(m.id, "example.words");
        assert_eq!(m.tools.len(), 1);
        let t = &m.tools[0].run;
        assert_eq!(t.sites, ["api.example.com"]);
        assert_eq!(
            t.key,
            Some(KeyUse {
                var: "WORDS_TOKEN".into(),
                test_key: "WORDS_TOKEN".into()
            })
        );
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        let lines = can_lines(&m);
        assert_eq!(
            lines[0],
            "Gives your agents a tool: Count the words in a piece of text."
        );
        assert!(lines
            .iter()
            .any(|l| l.starts_with("Talks to api.example.com.")));
        assert!(lines.iter().any(|l| l.contains("test key WORDS_TOKEN")));
        assert!(!lines.iter().any(|l| l.contains("change things")));
        assert!(lines
            .iter()
            .chain([&NEVER.to_owned()])
            .all(|l| !l.contains('\u{2014}')));
    }

    fn refused(text: &str) -> String {
        parse(text).expect_err("refused")
    }

    #[test]
    fn what_v1_cannot_wall_in_is_refused_in_plain_words() {
        let with = |from: &str, to: &str| GOOD.replace(from, to);
        assert!(
            refused(&with("decurion_api = \"1\"", "decurion_api = \"2\"")).contains("version 2")
        );
        assert!(
            refused(&with("effect = \"read\"", "effect = \"outbound_message\""))
                .contains("sends messages")
        );
        assert!(refused(&with("effect = \"read\"", "effect = \"deploy\"")).contains("deploys"));
        assert!(refused(&with("effect = \"read\"\n", "")).contains("effect = \"read\""));
        assert!(
            refused(&with("https://API.Example.com/v1", "hooks.slack.com")).contains("slack.com")
        );
        assert!(
            refused(&with("[[tool]]", "[[agent]]\nname = \"x\"\n[[tool]]"))
                .contains("new kind of agent")
        );
        assert!(
            refused(&with("[[tool]]", "[[check]]\nname = \"x\"\n[[tool]]"))
                .contains("automatic check")
        );
        assert!(refused(&with(
            "effect = \"read\"",
            "effect = \"read\"\nplacement = \"hub\""
        ))
        .contains("beside Decurion"));
        assert!(refused(&with(
            "effect = \"read\"",
            "effect = \"read\"\ntransport = \"stdio\""
        ))
        .contains("stdio"));
        assert!(refused(&with(
            "effect = \"read\"",
            "effect = \"read\"\nimage = \"x@sha256:00\""
        ))
        .contains("image"));
        assert!(refused(&with("network = [", "worktree = \"read\"\nnetwork = [")).contains("code"));
        assert!(refused(&with("network = [", "camera = true\nnetwork = [")).contains("camera"));
        assert!(refused(&with(
            "WORDS_TOKEN = \"WORDS_TOKEN\"",
            "WORDS_TOKEN = \"claude-profile-A\""
        ))
        .contains("test keys"));
        assert!(refused(&with(
            "WORDS_TOKEN = \"WORDS_TOKEN\"",
            "CLAUDE_CODE_OAUTH_TOKEN = \"X_KEY\""
        ))
        .contains("CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(refused(&with(
            "WORDS_TOKEN = \"WORDS_TOKEN\"",
            "A_KEY = \"A_KEY\"\nB_KEY = \"B_KEY\""
        ))
        .contains("at most one"));
        assert!(refused(&with("name = \"words\"", "name = \"browser\"")).contains("browser"));
        assert!(refused(&with("\"node\", ", "\"/usr/bin/node\", ")).contains("program"));
        assert!(refused(&with("id = \"example.words\"", "id = \"words\"")).contains("addon.id"));
        assert!(refused(&with("version = \"0.1.0\"", "version = \"one\"")).contains("1.2.0"));
        assert!(refused("not toml [").contains("not valid TOML"));
    }

    #[test]
    fn unknown_keys_outside_permissions_only_warn() {
        let m = parse(&GOOD.replace(
            "effect = \"read\"",
            "effect = \"read\"\nport = 7000\ncolor = \"blue\"",
        ))
        .expect("parses");
        assert_eq!(m.warnings.len(), 2, "{:?}", m.warnings);
    }

    #[test]
    fn an_update_asks_again_only_for_what_is_new() {
        let old = parse(GOOD).expect("parses").grants();
        assert!(new_lines(&old, &parse(GOOD).expect("parses")).is_empty());
        // Fewer: a site and the key gone.
        let fewer = GOOD
            .replace("network = [\"https://API.Example.com/v1\"]", "")
            .replace("WORDS_TOKEN = \"WORDS_TOKEN\"", "");
        assert!(new_lines(&old, &parse(&fewer).expect("parses")).is_empty());
        // A site under one already approved is not new.
        let under = GOOD.replace("https://API.Example.com/v1", "v2.api.example.com");
        assert!(new_lines(&old, &parse(&under).expect("parses")).is_empty());
        let wider = GOOD
            .replace("effect = \"read\"", "effect = \"write\"")
            .replace(
                "\"https://API.Example.com/v1\"",
                "\"api.example.com\", \"files.example.org\"",
            );
        let lines = new_lines(&old, &parse(&wider).expect("parses"));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("can now change things"));
        assert!(lines[1].starts_with("Talks to files.example.org."));
    }

    #[test]
    fn the_examples_in_the_docs_read_as_they_say() {
        let m = parse(include_str!(
            "../../../docs/addons/word-count/decurion-addon.toml"
        ))
        .expect("the working example parses");
        assert_eq!(m.id, "decurion.word-count");
        assert_eq!(m.tools[0].run.command, ["node", "server.mjs"]);
        assert!(m.tools[0].run.sites.is_empty() && m.tools[0].run.key.is_none());
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        // The full design's example has kinds v1 does not run yet.
        let full = parse(include_str!(
            "../../../docs/addons/example/decurion-addon.toml"
        ));
        assert!(full.expect_err("refused").contains("new kind of agent"));
    }

    #[test]
    fn lists_read_like_a_sentence() {
        let s = |v: &[&str]| and_list(&v.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        assert_eq!(s(&["a"]), "a");
        assert_eq!(s(&["a", "b"]), "a and b");
        assert_eq!(s(&["a", "b", "c"]), "a, b and c");
    }
}
