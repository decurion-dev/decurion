//! Where apps' test keys are kept: Windows Credential Manager on Windows, a
//! private folder elsewhere (for development). Never the hub's database, never
//! an app's folder, never git.
//!
//! The hub saves keys here when the owner adds them on the board and keeps
//! only their names and last four characters. The runner reads them here at
//! the start of each run and hands them to that run's container.
//!
//! Saved website sign-ins are kept here too (see [`sealed`]): a random key
//! per site in the same key store, and the sign-in itself sealed with it in
//! a private file.

use std::path::{Path, PathBuf};

pub mod sealed;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// The owner's input was refused; the text says why, in plain words.
    #[error("{0}")]
    Invalid(String),
    #[error("could not use the key store: {0}")]
    Store(String),
}

pub type Result<T> = std::result::Result<T, VaultError>;

/// Longest value kept. Credential Manager holds at most 2,560 bytes and the
/// keyring crate stores UTF-16, so this leaves room.
pub const MAX_VALUE: usize = 1000;
/// Shortest value kept. Values are hidden from the board by exact match, and
/// a very short one would hide ordinary words.
pub const MIN_VALUE: usize = 8;

/// Names the container, the docker CLI or the agent's tools already use.
/// Windows environment names ignore case, so these are compared uppercased.
const RESERVED: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "TERM",
    "LANG",
    "PWD",
    "HOSTNAME",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "USERPROFILE",
    "LOCALAPPDATA",
    "APPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CURL_CA_BUNDLE",
    "REQUESTS_CA_BUNDLE",
    "BASH_ENV",
    "ENV",
];
const RESERVED_PREFIXES: &[&str] = &[
    "ANTHROPIC_",
    "CLAUDE_",
    "DECURION_",
    "DOCKER_",
    "GIT_",
    "LD_",
    "NODE_",
    "NPM_",
];

/// A test key's name is the environment variable the app reads it from, for
/// example `STRIPE_SECRET_KEY`.
pub fn check_name(name: &str) -> Result<()> {
    let b = name.as_bytes();
    let shaped = (1..=64).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_');
    if !shaped {
        return Err(VaultError::Invalid(
            "a key name is 1-64 capital letters, digits or _, starting with a letter, like STRIPE_SECRET_KEY".into(),
        ));
    }
    if RESERVED.contains(&name) || RESERVED_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Err(VaultError::Invalid(format!(
            "{name} is used by Decurion or the agent's box; pick another name"
        )));
    }
    Ok(())
}

/// What a value looks like when it is a live key, never a test key.
fn live_kind(v: &str) -> Option<&'static str> {
    const SHAPES: &[(&str, &str)] = &[
        ("sk_live_", "a live Stripe key"),
        ("rk_live_", "a live Stripe key"),
        ("pk_live_", "a live Stripe key"),
        ("sk-ant-", "an Anthropic key"),
        ("github_pat_", "a GitHub token"),
        ("ghp_", "a GitHub token"),
        ("gho_", "a GitHub token"),
        ("ghu_", "a GitHub token"),
        ("ghs_", "a GitHub token"),
        ("ghr_", "a GitHub token"),
        ("glpat-", "a GitLab token"),
        ("xoxb-", "a Slack token"),
        ("xoxp-", "a Slack token"),
        ("xapp-", "a Slack token"),
        ("-----BEGIN", "a private key"),
    ];
    if let Some((_, kind)) = SHAPES.iter().find(|(p, _)| v.contains(p)) {
        return Some(kind);
    }
    // AWS access key ids: AKIA or ASIA and 16 more capitals or digits.
    let b = v.as_bytes();
    let aws = (0..b.len().saturating_sub(19)).any(|i| {
        (b[i..].starts_with(b"AKIA") || b[i..].starts_with(b"ASIA"))
            && b[i + 4..i + 20]
                .iter()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    });
    aws.then_some("an AWS access key")
}

/// Test keys only. Live keys, and keys Decurion keeps out of boxes on
/// purpose (Anthropic, GitHub, AWS), are refused here, with no way around it.
pub fn check_value(value: &str) -> Result<()> {
    let n = value.chars().count();
    if !(MIN_VALUE..=MAX_VALUE).contains(&n) {
        return Err(VaultError::Invalid(format!(
            "a key must be {MIN_VALUE} to {MAX_VALUE} characters"
        )));
    }
    // Credential Manager's own limit, in the UTF-16 it stores.
    if value.encode_utf16().count() > 1280 {
        return Err(VaultError::Invalid(format!(
            "a key must be {MIN_VALUE} to {MAX_VALUE} characters"
        )));
    }
    // Hiding and the commit check match the exact value, so a stray space
    // pasted with it would let the real key through.
    if value.trim() != value {
        return Err(VaultError::Invalid(
            "a key cannot start or end with a space".into(),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(VaultError::Invalid(
            "a key must be on one line, without tabs or hidden characters".into(),
        ));
    }
    if let Some(kind) = live_kind(value) {
        return Err(VaultError::Invalid(format!(
            "this looks like {kind}. Only test keys can go to agents; live keys stay off the PC's agent boxes"
        )));
    }
    Ok(())
}

/// What the board shows of a saved key.
pub fn last_four(value: &str) -> String {
    let n = value.chars().count();
    value.chars().skip(n.saturating_sub(4)).collect()
}

/// The Credential Manager entry for one app's test key.
pub fn target(app_id: i64, name: &str) -> String {
    format!("decurion/app-{app_id}/test-key/{name}")
}

#[derive(Debug, Clone)]
enum Backend {
    #[cfg(windows)]
    CredentialManager,
    Folder(PathBuf),
}

/// Where test keys live on this machine.
#[derive(Debug, Clone)]
pub struct Vault {
    backend: Backend,
    /// Where sealed website sign-ins are kept. Their keys are in `backend`.
    sign_ins: PathBuf,
}

impl Vault {
    /// The machine's own key store: Credential Manager on Windows, otherwise
    /// the `vault` folder inside Decurion's data folder, readable only by
    /// this user. Sealed sign-ins go in the `sign-ins` folder beside it.
    pub fn system(data_dir: &Path) -> Self {
        #[cfg(windows)]
        {
            Self {
                backend: Backend::CredentialManager,
                sign_ins: data_dir.join("sign-ins"),
            }
        }
        #[cfg(not(windows))]
        {
            Self {
                sign_ins: data_dir.join("sign-ins"),
                ..Self::in_folder(data_dir.join("vault"))
            }
        }
    }

    /// A folder-backed vault, used off Windows and by tests. Sealed sign-ins
    /// go in its `sign-ins` folder.
    pub fn in_folder(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            sign_ins: dir.join("sign-ins"),
            backend: Backend::Folder(dir),
        }
    }

    pub fn put(&self, app_id: i64, name: &str, value: &str) -> Result<()> {
        check_name(name)?;
        check_value(value)?;
        match &self.backend {
            #[cfg(windows)]
            Backend::CredentialManager => entry(app_id, name)?
                .set_password(value)
                .map_err(|e| VaultError::Store(e.to_string())),
            Backend::Folder(dir) => {
                let path = file(dir, app_id, name)?;
                let parent = path
                    .parent()
                    .ok_or_else(|| VaultError::Store("no folder".into()))?;
                private_dir(dir)?;
                private_dir(parent)?;
                // Its own temporary name, so two saves at once cannot mix.
                static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let tmp = path.with_extension(format!("{}-{n}.tmp", std::process::id()));
                write_private(&tmp, value.as_bytes())?;
                std::fs::rename(&tmp, &path).map_err(store)
            }
        }
    }

    /// The saved value, or `None` if nothing is saved under that name.
    pub fn get(&self, app_id: i64, name: &str) -> Result<Option<String>> {
        check_name(name)?;
        match &self.backend {
            #[cfg(windows)]
            Backend::CredentialManager => match entry(app_id, name)?.get_password() {
                Ok(v) => Ok(Some(v)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(VaultError::Store(e.to_string())),
            },
            Backend::Folder(dir) => match std::fs::read_to_string(file(dir, app_id, name)?) {
                Ok(v) => Ok(Some(v)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(store(e)),
            },
        }
    }

    /// Removes a saved key. Removing one that is not there is not an error.
    pub fn delete(&self, app_id: i64, name: &str) -> Result<()> {
        check_name(name)?;
        match &self.backend {
            #[cfg(windows)]
            Backend::CredentialManager => match entry(app_id, name)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(VaultError::Store(e.to_string())),
            },
            Backend::Folder(dir) => match std::fs::remove_file(file(dir, app_id, name)?) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(store(e)),
            },
        }
    }
}

#[cfg(windows)]
fn entry(app_id: i64, name: &str) -> Result<keyring::Entry> {
    keyring::Entry::new_with_target(&target(app_id, name), "decurion", name)
        .map_err(|e| VaultError::Store(e.to_string()))
}

fn store(e: std::io::Error) -> VaultError {
    VaultError::Store(e.to_string())
}

fn file(dir: &Path, app_id: i64, name: &str) -> Result<PathBuf> {
    // The name was checked, so it is a plain file name.
    check_name(name)?;
    Ok(dir.join(format!("app-{app_id}")).join(name))
}

fn private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(store)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(store)?;
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).map_err(store)?;
    f.write_all(bytes).map_err(store)?;
    f.sync_all().map_err(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("decurion-vault-{}", uuid::Uuid::new_v4().simple()))
    }

    #[test]
    fn names_are_env_names_and_not_reserved() {
        assert!(check_name("STRIPE_SECRET_KEY").is_ok());
        assert!(check_name("SUPABASE_ANON_KEY2").is_ok());
        for bad in [
            "",
            "stripe",
            "1KEY",
            "A-B",
            "../X",
            "PATH",
            "TEMP",
            "SYSTEMROOT",
            "HTTPS_PROXY",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "GIT_CONFIG_COUNT",
            "DOCKER_HOST",
            "LD_PRELOAD",
            "NODE_OPTIONS",
            "DECURION_DATA",
        ] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
        assert!(check_name(&"A".repeat(65)).is_err());
    }

    #[test]
    fn live_and_kept_out_keys_are_refused() {
        assert!(check_value("sk_test_51Habc123def456").is_ok());
        assert!(check_value("eyJhbGciOiJIUzI1NiJ9.eyJyb2xlIjoiYW5vbiJ9.abc").is_ok());
        for bad in [
            "sk_live_51Habc123def456",
            "rk_live_51Habc123def456",
            "sk-ant-api03-abcdefgh",
            "ghp_abcdefghijklmnop",
            "github_pat_11AAAA",
            "AKIAIOSFODNN7EXAMPLE",
            "id=ASIAIOSFODNN7EXAMPLE",
            "-----BEGIN PRIVATE KEY-----",
            "xoxb-1234-5678",
            "short",
            "two\nlines here",
            "tab\there123",
            "sk_test_abc12345 ",
            " sk_test_abc12345",
        ] {
            assert!(check_value(bad).is_err(), "{bad}");
        }
        assert!(check_value(&"x".repeat(MAX_VALUE)).is_ok());
        assert!(check_value(&"x".repeat(MAX_VALUE + 1)).is_err());
        // Within MAX_VALUE characters, but over Credential Manager's limit.
        assert!(check_value(&"😀".repeat(700)).is_err());
    }

    #[test]
    fn shows_only_the_last_four() {
        assert_eq!(last_four("sk_test_abcd1234"), "1234");
        assert_eq!(last_four("ab"), "ab");
        assert_eq!(last_four("ünïcødé"), "cødé");
    }

    #[test]
    fn folder_vault_round_trips_and_is_private() {
        let dir = tmp();
        let v = Vault::in_folder(&dir);
        assert!(matches!(v.get(3, "STRIPE_SECRET_KEY"), Ok(None)));
        v.put(3, "STRIPE_SECRET_KEY", "sk_test_one11111")
            .expect("put");
        v.put(3, "STRIPE_SECRET_KEY", "sk_test_two22222")
            .expect("replace");
        assert_eq!(
            v.get(3, "STRIPE_SECRET_KEY").ok().flatten().as_deref(),
            Some("sk_test_two22222")
        );
        // Another app's key of the same name is separate.
        assert!(matches!(v.get(4, "STRIPE_SECRET_KEY"), Ok(None)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| {
                std::fs::metadata(p)
                    .map(|m| m.permissions().mode() & 0o777)
                    .unwrap_or(0)
            };
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&dir.join("app-3").join("STRIPE_SECRET_KEY")), 0o600);
        }
        v.delete(3, "STRIPE_SECRET_KEY").expect("delete");
        v.delete(3, "STRIPE_SECRET_KEY").expect("delete twice");
        assert!(matches!(v.get(3, "STRIPE_SECRET_KEY"), Ok(None)));
        assert!(v.put(3, "STRIPE_SECRET_KEY", "sk_live_nope1234").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn targets_are_per_app() {
        assert_eq!(target(7, "API_KEY"), "decurion/app-7/test-key/API_KEY");
    }
}
