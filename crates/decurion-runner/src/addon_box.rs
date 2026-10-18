//! Add-on tools beside a work run's agent (ADR 0001, v1: tools only):
//! fetching each add-on's files from the hub and checking their hash,
//! starting one sealed box per tool on the run's network, waiting until it
//! answers, and removing the boxes when the run ends. The arguments and
//! checks themselves are in `decurion_runner::addons`.
//!
//! A tool that cannot be fetched, checked or started is left out of the
//! run and the owner is told on the work's activity; the work goes on
//! without it. Nothing here ever gives a tool the run's credential.

use super::{docker, docker_out, download_retrying, remove_all, spawn_stderr_tail, Hub};
use decurion_proto::addon::{AddonUse, Bundle, ToolUse, MAX_RUN_TOOLS, MAX_TOTAL_BYTES};
use decurion_proto::RunEvent;
use decurion_runner::addons as a;
use decurion_runner::hide;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Child;
use tokio::time::Instant;

/// How long a tool may take to start listening.
const READY_WAIT: Duration = Duration::from_secs(45);
/// The most one add-on's download may weigh: its files in base64, in JSON.
const BUNDLE_CAP: u64 = MAX_TOTAL_BYTES * 2 + (1 << 20);

/// The tools a run got, and what the agent's start needs from them.
pub(crate) struct Tools {
    names: Vec<String>,
    // Held so each `docker run` stays attached; killed when dropped.
    _children: Vec<Child>,
    dir: PathBuf,
    /// The MCP config to add to the agent's arguments, if any tool started.
    pub(crate) config: Option<String>,
    /// What the agent is told about them.
    pub(crate) prompt: Option<String>,
}

fn run_dir(data: &Path, id: i64) -> PathBuf {
    data.join("addons").join(id.to_string())
}

/// Fetches, checks and starts the tools of `addons` for thread `id`, beside
/// its browser box. `keys` are the app's test keys (name, value); a tool gets
/// only the one it named. `secrets` are hidden from the tools' logs.
pub(crate) async fn start(
    hub: &Hub,
    data: &Path,
    id: i64,
    browser: &str,
    addons: &[AddonUse],
    keys: &[(String, String)],
    secrets: &[String],
) -> Tools {
    let dir = run_dir(data, id);
    let mut t = Tools {
        names: Vec::new(),
        _children: Vec::new(),
        dir: dir.clone(),
        config: None,
        prompt: None,
    };
    if addons.is_empty() {
        return t;
    }
    clear(id).await;
    let _ = remove_all(&dir);
    let mut started: Vec<(String, u16)> = Vec::new();
    let mut told: Vec<ToolUse> = Vec::new();
    let mut n = 0;
    for addon in addons {
        let files = match fetch(hub, data, id, &dir, addon).await {
            Ok(f) => f,
            Err(err) => {
                tracing::warn!(err = %format!("{err:#}"), id, addon = %addon.id, "left an add-on out of the run");
                tell(
                    hub,
                    id,
                    format!(
                        "The add-on \"{}\" was left out of this run: its files could not be fetched, or they changed after you approved them. The work goes on without it.",
                        addon.name
                    ),
                )
                .await;
                continue;
            }
        };
        for tool in &addon.tools {
            if n >= MAX_RUN_TOOLS {
                tell(hub, id, format!("The add-on \"{}\" has more tools than one run can take, so {} was left out.", addon.name, tool.name)).await;
                continue;
            }
            let value = match &tool.key {
                None => None,
                Some(k) => match keys.iter().find(|(name, _)| *name == k.test_key) {
                    Some((_, v)) => Some((k.var.clone(), v.clone())),
                    None => {
                        tell(
                            hub,
                            id,
                            format!(
                                "The add-on \"{}\" needs the test key {}, which this app does not have, so its tool {} was left out. Add the key in Settings, Test keys.",
                                addon.name, k.test_key, tool.name
                            ),
                        )
                        .await;
                        continue;
                    }
                },
            };
            match start_tool(id, n, browser, &files, tool, value, secrets).await {
                Ok(child) => {
                    t.names.push(a::tool_container_name(id, n));
                    t._children.push(child);
                    started.push((tool.name.clone(), a::port(n)));
                    told.push(tool.clone());
                }
                Err(err) => {
                    tracing::warn!(err = %hide(&format!("{err:#}"), secrets), id, tool = %tool.name, "an add-on tool did not start");
                    let _ = docker_out(&["rm", "-f", &a::tool_container_name(id, n)]).await;
                    tell(
                        hub,
                        id,
                        format!(
                            "The tool {} from the add-on \"{}\" did not start, so this run goes without it.",
                            tool.name, addon.name
                        ),
                    )
                    .await;
                }
            }
            n += 1;
        }
    }
    if !started.is_empty() {
        t.config = Some(a::mcp_config(&started));
        t.prompt = a::tools_prompt(&told.iter().collect::<Vec<_>>());
        tracing::info!(id, tools = started.len(), "add-on tools up");
    }
    t
}

async fn tell(hub: &Hub, id: i64, text: String) {
    hub.event(id, &RunEvent::Message { text }).await;
}

/// Downloads one add-on's files, checks them against the claim, and writes
/// them to their own folder for the tool boxes.
async fn fetch(
    hub: &Hub,
    data: &Path,
    id: i64,
    dir: &Path,
    addon: &AddonUse,
) -> anyhow::Result<PathBuf> {
    let xfer = super::transfer_dir(data, id)?;
    let json = xfer.join("addon.json");
    let path = format!("/runner/threads/{id}/addons/{}", addon.id);
    let r = download_retrying(hub, &path, &json, BUNDLE_CAP).await;
    let bytes = r.and_then(|_| Ok(std::fs::read(&json)?));
    let _ = remove_all(&xfer);
    let bundle: Bundle = serde_json::from_slice(&bytes?)?;
    let files = a::check_bundle(&bundle, addon)?;
    // The id passed check_id at the hub; the folder is named for the hash.
    let dest = dir.join(&addon.hash);
    remove_all(&dest)?;
    for (rel, data) in &files {
        let p = dest.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, data)?;
    }
    std::fs::create_dir_all(&dest)?;
    Ok(dest)
}

/// Starts one tool's box and waits until it listens on its port.
async fn start_tool(
    id: i64,
    n: usize,
    browser: &str,
    files: &Path,
    tool: &ToolUse,
    key: Option<(String, String)>,
    secrets: &[String],
) -> anyhow::Result<Child> {
    let name = a::tool_container_name(id, n);
    let mut cmd = docker();
    cmd.args(a::tool_box_args(id, n, browser, files, tool)?);
    if let Some((var, value)) = key {
        // Passed by name in the arguments; the value only ever here.
        cmd.env(var, value);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let tail = child
        .stderr
        .take()
        .map(|e| spawn_stderr_tail(e, secrets.to_vec()));
    let probe = a::ready_probe(a::port(n));
    let deadline = Instant::now() + READY_WAIT;
    loop {
        if let Some(status) = child.try_wait()? {
            let said = match tail {
                Some(t) => t.await.unwrap_or_default(),
                None => String::new(),
            };
            anyhow::bail!("tool box exited with {status}: {}", said.trim());
        }
        if let Ok(out) = docker_out(&["exec", &name, "node", "-e", &probe]).await {
            if out.status.success() {
                return Ok(child);
            }
        }
        if Instant::now() > deadline {
            anyhow::bail!("tool did not start listening in time");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Stops a run's tool boxes and removes their copy of the files.
pub(crate) async fn stop(t: &Tools) {
    if !t.names.is_empty() {
        let mut rm = vec!["rm", "-f"];
        rm.extend(t.names.iter().map(String::as_str));
        let _ = docker_out(&rm).await;
    }
    let _ = remove_all(&t.dir);
}

/// Removes any tool box of thread `id`'s, such as one a dead runner left.
pub(crate) async fn clear(id: i64) {
    let filter = format!("name=^{}", a::tool_container_prefix(id));
    let Ok(out) = docker_out(&["ps", "-aq", "--filter", &filter]).await else {
        return;
    };
    let ids = String::from_utf8_lossy(&out.stdout).into_owned();
    let ids: Vec<&str> = ids.split_whitespace().collect();
    if !ids.is_empty() {
        let mut rm = vec!["rm", "-f"];
        rm.extend(ids);
        let _ = docker_out(&rm).await;
    }
}
