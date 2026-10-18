//! Saved website sign-ins, sealed.
//!
//! Each saved sign-in (a browser's cookies and local storage for one site)
//! gets its own random 256-bit key, kept in the same key store as test keys:
//! Windows Credential Manager as `decurion/app-{a}/sign-in/{s}`, or the
//! private vault folder elsewhere. The sign-in itself is sealed with that key
//! (AES-256-GCM) into a private file,
//! `<data>/sign-ins/app-{a}/site-{s}.v{n}.bin`, never the hub's database.
//!
//! The app, site and version are sealed in with it, so a file copied to
//! another app's, site's or version's name does not open. Removing a sign-in
//! removes its key first: from then on any copy of the file left behind is
//! unreadable.

use crate::{private_dir, store, write_private, Backend, Result, Vault, VaultError};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use std::path::PathBuf;

pub use decurion_proto::browser::MAX_STATE_BYTES;

const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;

/// The Credential Manager entry holding one saved sign-in's key.
pub fn target(app_id: i64, site_id: i64) -> String {
    format!("decurion/app-{app_id}/sign-in/{site_id}")
}

/// What the seal is bound to besides the key.
fn bound_to(app_id: i64, site_id: i64, version: i64) -> Vec<u8> {
    format!("decurion sign-in|app-{app_id}|site-{site_id}|v{version}").into_bytes()
}

/// Ids name files and key store entries, so only positive numbers are used.
fn check_ids(ids: &[i64]) -> Result<()> {
    if ids.iter().all(|&n| n > 0) {
        Ok(())
    } else {
        Err(VaultError::Invalid("not a saved sign-in".into()))
    }
}

fn random(buf: &mut [u8]) -> Result<()> {
    getrandom::getrandom(buf)
        .map_err(|e| VaultError::Store(format!("no randomness from the system: {e}")))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; KEY_BYTES]> {
    let s = s.trim();
    if s.len() != KEY_BYTES * 2 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; KEY_BYTES];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Where the folder backend keeps a site's key. Test key names are capitals,
/// so `sign-in` never collides with one.
fn key_file(dir: &std::path::Path, app_id: i64, site_id: i64) -> PathBuf {
    dir.join(format!("app-{app_id}"))
        .join("sign-in")
        .join(site_id.to_string())
}

/// Two saves of the same site at once must not each make a key, or the
/// first file would be sealed with a key that no longer exists.
static KEY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Vault {
    fn sign_in_dir(&self, app_id: i64) -> PathBuf {
        self.sign_ins.join(format!("app-{app_id}"))
    }

    /// Where one version of a saved sign-in is kept.
    pub fn sign_in_file(&self, app_id: i64, site_id: i64, version: i64) -> PathBuf {
        self.sign_in_dir(app_id)
            .join(format!("site-{site_id}.v{version}.bin"))
    }

    /// The site's key, made the first time it is needed if `make` is set.
    fn sign_in_key(
        &self,
        app_id: i64,
        site_id: i64,
        make: bool,
    ) -> Result<Option<[u8; KEY_BYTES]>> {
        let _held = KEY_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(text) = self.read_key(app_id, site_id)? {
            return unhex(&text)
                .map(Some)
                .ok_or_else(|| VaultError::Store("a saved sign-in's key is damaged".into()));
        }
        if !make {
            return Ok(None);
        }
        let mut key = [0u8; KEY_BYTES];
        random(&mut key)?;
        self.write_key(app_id, site_id, &hex(&key))?;
        Ok(Some(key))
    }

    fn read_key(&self, app_id: i64, site_id: i64) -> Result<Option<String>> {
        match &self.backend {
            #[cfg(windows)]
            Backend::CredentialManager => match key_entry(app_id, site_id)?.get_password() {
                Ok(v) => Ok(Some(v)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(VaultError::Store(e.to_string())),
            },
            Backend::Folder(dir) => match std::fs::read_to_string(key_file(dir, app_id, site_id)) {
                Ok(v) => Ok(Some(v)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(store(e)),
            },
        }
    }

    fn write_key(&self, app_id: i64, site_id: i64, text: &str) -> Result<()> {
        match &self.backend {
            #[cfg(windows)]
            Backend::CredentialManager => key_entry(app_id, site_id)?
                .set_password(text)
                .map_err(|e| VaultError::Store(e.to_string())),
            Backend::Folder(dir) => {
                let app_dir = dir.join(format!("app-{app_id}"));
                let path = key_file(dir, app_id, site_id);
                private_dir(dir)?;
                private_dir(&app_dir)?;
                private_dir(&app_dir.join("sign-in"))?;
                write_private(&path, text.as_bytes())
            }
        }
    }

    fn delete_key(&self, app_id: i64, site_id: i64) -> Result<()> {
        match &self.backend {
            #[cfg(windows)]
            Backend::CredentialManager => match key_entry(app_id, site_id)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(VaultError::Store(e.to_string())),
            },
            Backend::Folder(dir) => match std::fs::remove_file(key_file(dir, app_id, site_id)) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(store(e)),
            },
        }
    }

    /// Seals one version of a site's saved sign-in (Playwright's
    /// `storageState`, as JSON bytes) into its private file, replacing that
    /// version if it was saved before. At most [`MAX_STATE_BYTES`].
    pub fn seal_sign_in(
        &self,
        app_id: i64,
        site_id: i64,
        version: i64,
        state: &[u8],
    ) -> Result<()> {
        check_ids(&[app_id, site_id, version])?;
        if state.len() > MAX_STATE_BYTES {
            return Err(VaultError::Invalid(format!(
                "a saved sign-in can be at most {} MB",
                MAX_STATE_BYTES >> 20
            )));
        }
        let key = self
            .sign_in_key(app_id, site_id, true)?
            .ok_or_else(|| VaultError::Store("no key for the saved sign-in".into()))?;
        let cipher =
            Aes256Gcm::new_from_slice(&key).map_err(|e| VaultError::Store(e.to_string()))?;
        let mut nonce = [0u8; NONCE_BYTES];
        random(&mut nonce)?;
        let aad = bound_to(app_id, site_id, version);
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: state,
                    aad: &aad,
                },
            )
            .map_err(|_| VaultError::Store("could not seal the saved sign-in".into()))?;
        let mut bytes = Vec::with_capacity(NONCE_BYTES + sealed.len());
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&sealed);

        let dir = self.sign_in_dir(app_id);
        private_dir(&self.sign_ins)?;
        private_dir(&dir)?;
        let path = self.sign_in_file(app_id, site_id, version);
        // Its own temporary name, so two saves at once cannot mix.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("{}-{n}.tmp", std::process::id()));
        write_private(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            store(e)
        })
    }

    /// Opens one version of a site's saved sign-in. `None` if it was never
    /// saved or has been removed. A file that was changed, or moved from
    /// another app, site or version, is an error, never opened.
    pub fn open_sign_in(&self, app_id: i64, site_id: i64, version: i64) -> Result<Option<Vec<u8>>> {
        check_ids(&[app_id, site_id, version])?;
        let path = self.sign_in_file(app_id, site_id, version);
        let len = match std::fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(store(e)),
        };
        if len > (MAX_STATE_BYTES + NONCE_BYTES + TAG_BYTES) as u64
            || len < (NONCE_BYTES + TAG_BYTES) as u64
        {
            return Err(VaultError::Store("a saved sign-in file is damaged".into()));
        }
        let Some(key) = self.sign_in_key(app_id, site_id, false)? else {
            return Ok(None);
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(store(e)),
        };
        if bytes.len() < NONCE_BYTES + TAG_BYTES {
            return Err(VaultError::Store("a saved sign-in file is damaged".into()));
        }
        let (nonce, sealed) = bytes.split_at(NONCE_BYTES);
        let cipher =
            Aes256Gcm::new_from_slice(&key).map_err(|e| VaultError::Store(e.to_string()))?;
        let aad = bound_to(app_id, site_id, version);
        cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: sealed,
                    aad: &aad,
                },
            )
            .map(Some)
            .map_err(|_| {
                VaultError::Store(
                    "a saved sign-in did not open: it was changed, or belongs to another app, site or version"
                        .into(),
                )
            })
    }

    /// Removes every version of a site's saved sign-in except `version`,
    /// once the hub has recorded `version` as the one in use.
    pub fn keep_only_sign_in(&self, app_id: i64, site_id: i64, version: i64) -> Result<()> {
        check_ids(&[app_id, site_id, version])?;
        self.remove_sign_in_files(app_id, site_id, Some(version))
    }

    /// Removes one version's file, such as one the hub refused to record.
    /// Removing one that is not there is not an error.
    pub fn drop_sign_in_version(&self, app_id: i64, site_id: i64, version: i64) -> Result<()> {
        check_ids(&[app_id, site_id, version])?;
        match std::fs::remove_file(self.sign_in_file(app_id, site_id, version)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(store(e)),
        }
    }

    /// Removes a site's saved sign-in: its key first, so any copy of a file
    /// left behind can no longer be opened, then every version's file.
    /// Removing one that is not there is not an error.
    pub fn delete_sign_in(&self, app_id: i64, site_id: i64) -> Result<()> {
        check_ids(&[app_id, site_id])?;
        {
            let _held = KEY_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            self.delete_key(app_id, site_id)?;
        }
        self.remove_sign_in_files(app_id, site_id, None)
    }

    fn remove_sign_in_files(&self, app_id: i64, site_id: i64, keep: Option<i64>) -> Result<()> {
        let dir = self.sign_in_dir(app_id);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(store(e)),
        };
        let prefix = format!("site-{site_id}.v");
        let kept = keep.map(|v| format!("site-{site_id}.v{v}.bin"));
        for entry in entries {
            let entry = entry.map_err(store)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // `site-5.v` never starts `site-50.v...`, so other sites are safe.
            if !name.starts_with(&prefix) || kept.as_deref() == Some(name) {
                continue;
            }
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(store(e)),
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn key_entry(app_id: i64, site_id: i64) -> Result<keyring::Entry> {
    keyring::Entry::new_with_target(&target(app_id, site_id), "decurion", "sign-in")
        .map_err(|e| VaultError::Store(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tmp() -> Tmp {
        Tmp(std::env::temp_dir().join(format!("decurion-sealed-{}", uuid::Uuid::new_v4().simple())))
    }

    const STATE: &[u8] = br#"{"cookies":[{"name":"sid","value":"s3ss10n-abcdef"}],"origins":[]}"#;

    #[test]
    fn seals_and_opens_and_is_not_plain_text() {
        let t = tmp();
        let v = Vault::in_folder(&t.0);
        assert!(matches!(v.open_sign_in(3, 5, 1), Ok(None)));
        v.seal_sign_in(3, 5, 1, STATE).expect("seal");
        assert_eq!(
            v.open_sign_in(3, 5, 1).ok().flatten().as_deref(),
            Some(STATE)
        );
        let raw = std::fs::read(v.sign_in_file(3, 5, 1)).expect("file");
        assert!(!raw.windows(8).any(|w| w == b"s3ss10n-"));
        // Sealing the same version again replaces it.
        v.seal_sign_in(3, 5, 1, b"{}").expect("again");
        assert_eq!(
            v.open_sign_in(3, 5, 1).ok().flatten().as_deref(),
            Some(&b"{}"[..])
        );
        // Another version or site that was never saved is simply not there.
        assert!(matches!(v.open_sign_in(3, 5, 2), Ok(None)));
        assert!(matches!(v.open_sign_in(3, 6, 1), Ok(None)));
        assert!(v.seal_sign_in(0, 5, 1, STATE).is_err());
        assert!(v.seal_sign_in(3, -5, 1, STATE).is_err());
    }

    #[test]
    fn a_file_moved_to_another_app_site_or_version_does_not_open() {
        let t = tmp();
        let v = Vault::in_folder(&t.0);
        v.seal_sign_in(3, 5, 1, STATE).expect("seal");
        // Give the other names keys of their own, as real sign-ins would have.
        v.seal_sign_in(4, 5, 1, b"{}").expect("other app");
        v.seal_sign_in(3, 6, 1, b"{}").expect("other site");
        let from = v.sign_in_file(3, 5, 1);
        for (a, s, n) in [(4, 5, 1), (3, 6, 1), (3, 5, 2)] {
            std::fs::copy(&from, v.sign_in_file(a, s, n)).expect("copy");
            assert!(v.open_sign_in(a, s, n).is_err(), "app {a} site {s} v{n}");
        }
        // Same key, same name, one byte changed.
        let mut raw = std::fs::read(&from).expect("read");
        let last = raw.len() - 1;
        raw[last] ^= 1;
        std::fs::write(&from, &raw).expect("write");
        assert!(v.open_sign_in(3, 5, 1).is_err());
    }

    #[test]
    fn size_is_capped() {
        let t = tmp();
        let v = Vault::in_folder(&t.0);
        let big = vec![b'x'; MAX_STATE_BYTES];
        v.seal_sign_in(3, 5, 1, &big).expect("at the cap");
        assert_eq!(
            v.open_sign_in(3, 5, 1).ok().flatten().map(|b| b.len()),
            Some(MAX_STATE_BYTES)
        );
        let over = vec![b'x'; MAX_STATE_BYTES + 1];
        assert!(matches!(
            v.seal_sign_in(3, 5, 2, &over),
            Err(VaultError::Invalid(_))
        ));
        assert!(!v.sign_in_file(3, 5, 2).exists());
        // A file grown past the cap on disk is refused before it is read.
        std::fs::write(v.sign_in_file(3, 5, 1), vec![0u8; MAX_STATE_BYTES + 64]).expect("write");
        assert!(v.open_sign_in(3, 5, 1).is_err());
    }

    #[test]
    fn older_versions_and_removed_sign_ins_are_gone() {
        let t = tmp();
        let v = Vault::in_folder(&t.0);
        v.seal_sign_in(3, 5, 1, STATE).expect("v1");
        v.seal_sign_in(3, 5, 2, STATE).expect("v2");
        v.seal_sign_in(3, 50, 1, STATE).expect("site 50");
        v.keep_only_sign_in(3, 5, 2).expect("keep v2");
        assert!(!v.sign_in_file(3, 5, 1).exists());
        assert!(v.open_sign_in(3, 5, 2).ok().flatten().is_some());
        // A site whose number starts the same is untouched.
        assert!(v.open_sign_in(3, 50, 1).ok().flatten().is_some());
        v.seal_sign_in(3, 5, 3, STATE).expect("v3");
        v.drop_sign_in_version(3, 5, 3).expect("drop v3");
        v.drop_sign_in_version(3, 5, 3).expect("drop v3 twice");
        assert!(matches!(v.open_sign_in(3, 5, 3), Ok(None)));
        assert!(v.open_sign_in(3, 5, 2).ok().flatten().is_some());

        // A copy kept from before removal no longer opens: its key is gone.
        let copy = t.0.join("copy.bin");
        std::fs::copy(v.sign_in_file(3, 5, 2), &copy).expect("copy");
        v.delete_sign_in(3, 5).expect("delete");
        v.delete_sign_in(3, 5).expect("delete twice");
        assert!(!v.sign_in_file(3, 5, 2).exists());
        std::fs::copy(&copy, v.sign_in_file(3, 5, 2)).expect("put back");
        assert!(matches!(v.open_sign_in(3, 5, 2), Ok(None)));
        assert!(v.open_sign_in(3, 50, 1).ok().flatten().is_some());
    }

    #[test]
    fn system_vault_keeps_sign_ins_beside_the_data() {
        let data = Path::new("/data");
        let v = Vault::system(data);
        assert_eq!(
            v.sign_in_file(3, 5, 7),
            data.join("sign-ins").join("app-3").join("site-5.v7.bin")
        );
        assert_eq!(target(3, 5), "decurion/app-3/sign-in/5");
    }

    #[cfg(unix)]
    #[test]
    fn folders_and_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let t = tmp();
        let v = Vault::in_folder(&t.0);
        v.seal_sign_in(3, 5, 1, STATE).expect("seal");
        let mode = |p: &Path| {
            std::fs::metadata(p)
                .map(|m| m.permissions().mode() & 0o777)
                .unwrap_or(0)
        };
        assert_eq!(mode(&t.0), 0o700);
        assert_eq!(mode(&t.0.join("sign-ins")), 0o700);
        assert_eq!(mode(&t.0.join("sign-ins").join("app-3")), 0o700);
        assert_eq!(mode(&v.sign_in_file(3, 5, 1)), 0o600);
        let key_dir = t.0.join("app-3").join("sign-in");
        assert_eq!(mode(&t.0.join("app-3")), 0o700);
        assert_eq!(mode(&key_dir), 0o700);
        assert_eq!(mode(&key_dir.join("5")), 0o600);
        let key = std::fs::read_to_string(key_dir.join("5")).expect("key");
        assert_eq!(key.len(), 64);
        assert!(unhex(&key).is_some());
    }
}
