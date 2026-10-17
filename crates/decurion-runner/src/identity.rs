//! Who this runner is: the PC runner, or a cloud worker with its own key.

use anyhow::Context;
use decurion_proto::Place;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The PC runner's name. The hub ties it to the `runner` key in keys.json.
pub const PC_NAME: &str = "local";

/// The hub a PC runner talks to unless `DECURION_HUB` says otherwise.
pub const PC_HUB: &str = "http://127.0.0.1:7420";

/// Which computer this runner is, from `DECURION_PLACE` (unset means the PC).
pub fn place_from_env(v: Option<&str>) -> anyhow::Result<Place> {
    match v.map(str::trim) {
        None | Some("") => Ok(Place::Pc),
        Some(s) => s
            .parse()
            .with_context(|| format!("DECURION_PLACE must be pc or cloud, not {s:?}")),
    }
}

/// A cloud worker's `cloud-runner.json`: where its hub is and the key it got
/// when it joined. Written once, on first start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudRunner {
    pub hub: String,
    pub name: String,
    pub token: String,
}

impl CloudRunner {
    pub fn parse(bytes: &[u8]) -> anyhow::Result<CloudRunner> {
        let mut c: CloudRunner =
            serde_json::from_slice(bytes).context("cloud-runner.json is not valid")?;
        c.hub = hub_url(&c.hub)?;
        if c.name.trim().is_empty() || c.token.trim().is_empty() {
            anyhow::bail!("cloud-runner.json has no name or token");
        }
        Ok(c)
    }
}

/// A hub address without its trailing slash, refusing anything that is not
/// http(s).
pub fn hub_url(s: &str) -> anyhow::Result<String> {
    let s = s.trim().trim_end_matches('/');
    if !(s.starts_with("https://") || s.starts_with("http://")) {
        anyhow::bail!("hub address {s:?} must start with https:// or http://");
    }
    Ok(s.to_owned())
}

/// Writes a file so that a crash mid-write never leaves a half-written one.
/// `private` files (keys) are readable by this user only on unix.
pub fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    use std::io::Write;
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    // A leftover temp file keeps its old permissions; start from nothing.
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_defaults_to_the_pc() {
        assert_eq!(place_from_env(None).expect("pc"), Place::Pc);
        assert_eq!(place_from_env(Some("")).expect("pc"), Place::Pc);
        assert_eq!(place_from_env(Some("cloud")).expect("cloud"), Place::Cloud);
        assert!(place_from_env(Some("moon")).is_err());
    }

    #[test]
    fn cloud_runner_file_is_checked() {
        let c = CloudRunner::parse(
            br#"{"hub":"https://pc.tail1.ts.net/","name":"cloud-1","token":"abc"}"#,
        )
        .expect("parse");
        assert_eq!(c.hub, "https://pc.tail1.ts.net");
        assert_eq!(c.name, "cloud-1");
        assert!(CloudRunner::parse(br#"{"hub":"pc","name":"c","token":"t"}"#).is_err());
        assert!(CloudRunner::parse(br#"{"hub":"http://pc","name":"c","token":""}"#).is_err());
        assert!(CloudRunner::parse(b"{").is_err());
    }

    #[test]
    fn private_files_are_written_whole_and_owner_only() {
        let dir = std::env::temp_dir().join(format!("decurion-id-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let p = dir.join("cloud-runner.json");
        write_atomic(&p, b"one", true).expect("write");
        write_atomic(&p, b"two", true).expect("rewrite");
        assert_eq!(std::fs::read(&p).expect("read"), b"two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
