//! Records the git commit the hub is built from, so a cloud worker can build
//! the same one.

use std::process::Command;

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for p in [".git/HEAD", ".git/refs/heads", ".git/packed-refs"] {
        let p = root.join(p);
        if p.exists() {
            println!("cargo:rerun-if-changed={}", p.display());
        }
    }
    let commit = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|c| c.len() == 40);
    if let Some(c) = commit {
        println!("cargo:rustc-env=DECURION_COMMIT={c}");
    }
}
