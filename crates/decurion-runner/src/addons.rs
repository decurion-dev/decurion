//! Add-on tools in a work run (ADR 0001, v1: tools only): the pure pieces,
//! kept here so they can be tested without Docker. The runner's
//! `addon_box` starts and stops the boxes.
//!
//! What must stay true:
//! - **Never in the agent's box.** Each tool runs in a box of its own, from
//!   the agent image but with none of what the agent holds: no code, no
//!   home volume, no Claude credential, no test keys but the one it named.
//!   Add-on code never runs where the run's Claude sign-in is.
//! - **Sealed like every box**: all capabilities dropped, no new
//!   privileges, capped memory and processes, a read-only root, and only
//!   the add-on's own checked files, read-only, at `/addon`.
//! - **Its own network namespace.** The tool box is a container of its own
//!   on the run's network, behind the run's gate, reached by the agent at
//!   [`tool_host`]. It never joins the agent's or the browser box's
//!   namespace (`--network container:`), whose loopback holds the browser
//!   tools with the owner's saved sign-ins, the agent's database and dev
//!   servers. Its websites are added to that gate for this run only.
//! - **Exactly the approved bytes.** [`check_bundle`] refuses files whose
//!   tree hash is not the one in the claim.
//! - **Only Decurion's MCP config.** The tools reach the agent as one more
//!   `--mcp-config` value next to Decurion's own, before
//!   `--strict-mcp-config`, so a repo's MCP servers still never load.

use crate::{container_name, mount_src, GATE_URL, IMAGE, MCP_BOX_CONFIG};
use base64::Engine;
use decurion_proto::addon::{self, AddonUse, Bundle, ToolUse};
use serde_json::{json, Value};
use std::path::Path;

/// One tool box of a thread's run. Its name starts like the agent's, so
/// the clean-up of boxes a dead runner left behind finds it too.
pub fn tool_container_name(thread_id: i64, n: usize) -> String {
    format!("{}-addon-{n}", container_name(thread_id))
}

/// What every tool box names its boxes with, for the clean-up.
pub fn tool_container_prefix(thread_id: i64) -> String {
    format!("{}-addon-", container_name(thread_id))
}

/// The name the agent reaches the `n`th tool of its run at, on the run's
/// own network. Every box lists these in `NO_PROXY` (`no_proxy`).
pub fn tool_host(n: usize) -> String {
    format!("addon-{n}")
}

/// The variable a tool box finds its [`tool_host`] name in, to check the
/// `Host` of each call against.
pub const HOST_VAR: &str = "DECURION_MCP_HOST";

/// The port the `n`th tool of a run listens on.
pub fn port(n: usize) -> u16 {
    addon::FIRST_PORT + n as u16
}

/// The websites a run's add-ons add to its gate, without repeats.
pub fn sites(addons: &[AddonUse]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in addons.iter().flat_map(|a| &a.tools) {
        for s in &t.sites {
            if !out.contains(s) {
                out.push(s.clone());
            }
        }
    }
    out
}

/// Arguments for `docker run` of one add-on tool. `files` is the runner's
/// checked copy of the add-on, mounted read-only; `network` is the run's
/// own network (behind the run's gate), which the tool joins as a box of
/// its own under [`tool_host`]. The
/// key, if the tool has one, is passed by name only: the caller sets its
/// value on the docker command.
pub fn tool_box_args(
    thread_id: i64,
    n: usize,
    network: &str,
    files: &Path,
    tool: &ToolUse,
) -> anyhow::Result<Vec<String>> {
    if !addon::check_tool_name(&tool.name) {
        anyhow::bail!("refusing odd tool name {:?}", tool.name);
    }
    let Some((program, rest)) = tool.command.split_first() else {
        anyhow::bail!("tool {} has no command", tool.name);
    };
    let plain = !program.is_empty()
        && !program.starts_with(['-', '.'])
        && program
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !plain {
        anyhow::bail!("refusing odd program {program:?} for tool {}", tool.name);
    }
    if let Some(k) = &tool.key {
        decurion_vault::check_name(&k.var).map_err(|e| anyhow::anyhow!("key {:?}: {e}", k.var))?;
    }
    let port = port(n).to_string();
    let src = mount_src(files)?;
    let mut a: Vec<String> = [
        "run",
        "--rm",
        // The agent image is built on this computer. Never fetch an image
        // by that name from Docker Hub, where anyone could publish one.
        "--pull",
        "never",
        "--name",
        &tool_container_name(thread_id, n),
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--memory",
        "1g",
        "--pids-limit",
        "256",
        "--read-only",
        "--tmpfs",
        "/tmp",
        // Never `container:`: sharing the agent's loopback would hand the
        // tool the browser tools and anything else listening there.
        "--network",
        network,
        "--network-alias",
        &tool_host(n),
        "--mount",
        &format!("type=bind,src={src},dst={},readonly", addon::BOX_DIR),
        "--workdir",
        addon::BOX_DIR,
        "-e",
        "HOME=/tmp",
        // Web traffic goes through the run's gate, like the agent's.
        "-e",
        &format!("HTTPS_PROXY={GATE_URL}"),
        "-e",
        &format!("https_proxy={GATE_URL}"),
        "-e",
        &format!("HTTP_PROXY={GATE_URL}"),
        "-e",
        &format!("http_proxy={GATE_URL}"),
        "-e",
        "NO_PROXY=gate,localhost,127.0.0.1,::1",
        "-e",
        "no_proxy=gate,localhost,127.0.0.1,::1",
        "-e",
        "NODE_USE_ENV_PROXY=1",
        "-e",
        &format!("{}={port}", addon::PORT_VAR),
        "-e",
        &format!("{HOST_VAR}={}", tool_host(n)),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(k) = &tool.key {
        a.extend(["-e".to_owned(), k.var.clone()]);
    }
    a.extend(["--entrypoint".to_owned(), program.clone(), IMAGE.to_owned()]);
    a.extend(rest.iter().map(|x| x.replace("{port}", &port)));
    Ok(a)
}

/// The MCP config the agent's Claude Code gets for the tools that started,
/// by name and number: each one over HTTP at its own box on the run's
/// network.
pub fn mcp_config(started: &[(String, usize)]) -> String {
    let servers: serde_json::Map<String, Value> = started
        .iter()
        .map(|(name, n)| {
            let url = format!("http://{}:{}/mcp", tool_host(*n), port(*n));
            (name.clone(), json!({ "type": "http", "url": url }))
        })
        .collect();
    json!({ "mcpServers": servers }).to_string()
}

/// Puts the tools' config next to Decurion's own in a Claude Code run's
/// arguments: right after the box config, inside the one `--mcp-config`,
/// so `--strict-mcp-config` still covers everything that loads.
pub fn add_to_run(args: &mut Vec<String>, config: &str) -> anyhow::Result<()> {
    let at = args
        .windows(2)
        .position(|w| w[0] == "--mcp-config" && w[1] == MCP_BOX_CONFIG)
        .ok_or_else(|| anyhow::anyhow!("run has no box MCP config to add tools to"))?;
    args.insert(at + 2, config.to_owned());
    Ok(())
}

/// What the agent is told about the tools it got.
pub fn tools_prompt(started: &[&ToolUse]) -> Option<String> {
    if started.is_empty() {
        return None;
    }
    let list: Vec<String> = started
        .iter()
        .map(|t| format!("mcp__{}__* ({})", t.name, t.summary.trim_end_matches('.')))
        .collect();
    Some(format!(
        "The owner added tools for this app: {}. Each runs in a box of its own and cannot see your files, \
so pass it what it needs. Use them like your other tools.",
        list.join("; ")
    ))
}

/// The files of a bundle the hub sent, checked against what the claim says
/// the owner approved: same add-on, plain paths, and the same tree hash,
/// worked out here again. Anything else is refused.
pub fn check_bundle(b: &Bundle, want: &AddonUse) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    if b.id != want.id || b.hash != want.hash {
        anyhow::bail!("the hub sent another add-on or version");
    }
    if b.files.len() > addon::MAX_FILES {
        anyhow::bail!("too many files");
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut files: Vec<(String, Vec<u8>)> = Vec::with_capacity(b.files.len());
    let mut total: u64 = 0;
    for f in &b.files {
        if !addon::check_path(&f.path) || files.iter().any(|(p, _)| *p == f.path) {
            anyhow::bail!("odd file name {:?}", f.path);
        }
        let data = b64
            .decode(&f.data)
            .map_err(|e| anyhow::anyhow!("file {:?}: {e}", f.path))?;
        total += data.len() as u64;
        if data.len() as u64 > addon::MAX_FILE_BYTES || total > addon::MAX_TOTAL_BYTES {
            anyhow::bail!("too big");
        }
        files.push((f.path.clone(), data));
    }
    let got = addon::tree_hash(files.iter().map(|(p, d)| (p.as_str(), d.as_slice())));
    if got != want.hash {
        anyhow::bail!("its files do not match what the owner approved");
    }
    Ok(files)
}

/// A one-line check, run inside the tool box, that something listens on
/// `port`: node is in the image the box runs from.
pub fn ready_probe(port: u16) -> String {
    format!(
        "require('net').connect({port},'127.0.0.1').on('connect',()=>process.exit(0)).on('error',()=>process.exit(1))"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use decurion_proto::addon::{BundleFile, KeyUse};

    fn tool() -> ToolUse {
        ToolUse {
            name: "words".into(),
            summary: "Count words.".into(),
            command: vec![
                "node".into(),
                "server.mjs".into(),
                "--port".into(),
                "{port}".into(),
            ],
            sites: vec!["api.example.com".into()],
            key: Some(KeyUse {
                var: "WORDS_TOKEN".into(),
                test_key: "WORDS_KEY".into(),
            }),
        }
    }

    #[test]
    fn a_tool_box_is_sealed_and_holds_only_its_files_and_its_one_key() {
        let a = tool_box_args(
            7,
            1,
            "decurion-net-7",
            Path::new("/data/addons/7/x"),
            &tool(),
        )
        .expect("args");
        let s = a.join(" ");
        assert_eq!(a[0], "run");
        assert!(s.contains("--name decurion-thread-7-addon-1"));
        for want in [
            "--pull never",
            "--cap-drop ALL",
            "--security-opt no-new-privileges",
            "--read-only",
            "--network decurion-net-7 --network-alias addon-1",
            "--mount type=bind,src=/data/addons/7/x,dst=/addon,readonly",
            "-e HTTPS_PROXY=http://gate:3128",
            "-e DECURION_MCP_PORT=8941",
            "-e DECURION_MCP_HOST=addon-1",
            "-e WORDS_TOKEN",
            "--entrypoint node decurion/claude-worker:dev server.mjs --port 8941",
        ] {
            assert!(s.contains(want), "missing {want:?} in {s}");
        }
        // One mount, no home volume, no credential, no Docker socket, no
        // value after a variable's name.
        assert_eq!(a.iter().filter(|x| *x == "--mount").count(), 1);
        for never in [
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "/home/agent",
            "/work",
            "docker.sock",
            "--privileged",
            "WORDS_TOKEN=",
            // Never the agent's or the browser's loopback: that holds the
            // browser tools with the owner's saved sign-ins.
            "container:",
            "--network=",
        ] {
            assert!(!s.contains(never), "{never} in {s}");
        }
        assert_eq!(a.iter().filter(|x| *x == "--network").count(), 1);
        let net = a.iter().position(|x| x == "--network").expect("network");
        assert_eq!(a[net + 1], "decurion-net-7");
        let image = a.iter().position(|x| x == IMAGE).expect("image");
        assert_eq!(a[image - 2], "--entrypoint");
    }

    #[test]
    fn every_box_reaches_each_tool_without_the_gate_and_nothing_else() {
        let n = crate::no_proxy();
        for i in 0..addon::MAX_RUN_TOOLS {
            assert!(
                n.split(',').any(|x| x == tool_host(i)),
                "{i} missing in {n}"
            );
        }
        assert_eq!(
            n.split(',').count(),
            addon::MAX_RUN_TOOLS + 4,
            "only the gate, the tools and loopback: {n}"
        );
    }

    #[test]
    fn odd_programs_names_and_keys_are_refused() {
        let mut t = tool();
        t.command[0] = "/bin/sh".into();
        assert!(tool_box_args(1, 0, "b", Path::new("/x"), &t).is_err());
        let mut t = tool();
        t.name = "browser".into();
        assert!(tool_box_args(1, 0, "b", Path::new("/x"), &t).is_err());
        let mut t = tool();
        t.key = Some(KeyUse {
            var: "CLAUDE_CODE_OAUTH_TOKEN".into(),
            test_key: "X_KEY".into(),
        });
        assert!(tool_box_args(1, 0, "b", Path::new("/x"), &t).is_err());
        let mut t = tool();
        t.command.clear();
        assert!(tool_box_args(1, 0, "b", Path::new("/x"), &t).is_err());
        assert!(tool_box_args(1, 0, "b", Path::new("/a,b"), &tool()).is_err());
    }

    #[test]
    fn tools_join_decurions_own_config_under_the_strict_flag() {
        let mut a: Vec<String> = [
            "run",
            "x",
            "--mcp-config",
            MCP_BOX_CONFIG,
            "--strict-mcp-config",
            "--session-id",
            "s",
        ]
        .map(String::from)
        .to_vec();
        let config = mcp_config(&[("words".into(), 0)]);
        add_to_run(&mut a, &config).expect("added");
        let at = a.iter().position(|x| x == "--mcp-config").expect("flag");
        assert_eq!(a[at + 1], MCP_BOX_CONFIG);
        assert_eq!(a[at + 2], config);
        assert_eq!(a[at + 3], "--strict-mcp-config");
        assert_eq!(a.iter().filter(|x| *x == "--mcp-config").count(), 1);
        let v: Value = serde_json::from_str(&config).expect("json");
        assert_eq!(
            v["mcpServers"]["words"]["url"],
            format!("http://addon-0:{}/mcp", port(0))
        );
        assert_eq!(v["mcpServers"]["words"]["type"], "http");
        let mut helper: Vec<String> = vec!["--mcp-config".into(), "/etc/decurion/mcp.json".into()];
        assert!(add_to_run(&mut helper, &config).is_err());
    }

    #[test]
    fn a_bundle_is_used_only_with_the_approved_hash() {
        let b64 = base64::engine::general_purpose::STANDARD;
        let files = [
            ("server.mjs", b"console.log(1)".to_vec()),
            ("decurion-addon.toml", b"x".to_vec()),
        ];
        let hash = addon::tree_hash(files.iter().map(|(p, d)| (*p, d.as_slice())));
        let want = AddonUse {
            id: "example.words".into(),
            name: "Words".into(),
            version: "0.1.0".into(),
            hash: hash.clone(),
            tools: vec![tool()],
        };
        let mut b = Bundle {
            id: want.id.clone(),
            version: want.version.clone(),
            hash: hash.clone(),
            files: files
                .iter()
                .map(|(p, d)| BundleFile {
                    path: (*p).into(),
                    data: b64.encode(d),
                })
                .collect(),
        };
        assert_eq!(check_bundle(&b, &want).expect("ok").len(), 2);
        b.files[0].data = b64.encode(b"console.log(2)");
        assert!(check_bundle(&b, &want).is_err());
        b.files[0] = BundleFile {
            path: "../server.mjs".into(),
            data: b64.encode(b"console.log(1)"),
        };
        assert!(check_bundle(&b, &want).is_err());
        b.hash = "0".repeat(64);
        assert!(check_bundle(&b, &want).is_err());
    }

    #[test]
    fn the_agent_hears_about_its_tools_and_their_sites_join_the_gate() {
        let t = tool();
        let p = tools_prompt(&[&t]).expect("prompt");
        assert!(p.contains("mcp__words__* (Count words)"));
        assert!(tools_prompt(&[]).is_none());
        let a = AddonUse {
            id: "a.b".into(),
            name: "B".into(),
            version: "1.0.0".into(),
            hash: "0".repeat(64),
            tools: vec![t.clone(), t],
        };
        assert_eq!(sites(&[a]), ["api.example.com"]);
        assert!(ready_probe(8940).contains("8940"));
        assert_eq!(port(0), 8940);
    }
}
