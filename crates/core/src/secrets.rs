//! Secret storage for session tokens (AC-AUTH-7, 04 §3.3).
//!
//! [`SecretStore`] is implemented by the OS keychain in the Tauri shell
//! (`src-tauri/src/secret_store.rs`), by [`FileSecretStore`] (`secrets.json`
//! in the app data dir, owner-only 0600 on Unix) when no keychain works, and
//! by [`MemorySecretStore`] in tests. Calls are blocking: async callers run
//! them on a blocking thread. Error messages never contain a secret value.

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Name of the file store in the app data dir.
pub const SECRETS_FILE: &str = "secrets.json";

/// Where secrets end up, as the UI reports it (`AuthStatus.keychain`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretStoreKind {
    /// macOS Keychain, Windows Credential Manager or Linux Secret Service.
    System,
    /// The 0600 file fallback.
    File,
    /// Nothing could be stored (keychain denied and no file fallback).
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("The secret store is unavailable: {0}")]
    Unavailable(String),
    #[error("Access to the secret store was denied: {0}")]
    Denied(String),
    #[error("Secret store I/O error: {0}")]
    Io(String),
}

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, key: &str) -> Result<(), SecretError>;
    fn kind(&self) -> SecretStoreKind;
}

/// A set/get/delete round trip with a throwaway value (04 §3.3 startup probe).
pub fn probe(store: &dyn SecretStore) -> Result<(), SecretError> {
    const KEY: &str = "probe:reqharbor";
    let value = format!("probe-{}", crate::model::new_id());
    store.set(KEY, &value)?;
    let read = store.get(KEY)?;
    let _ = store.delete(KEY);
    if read.as_deref() == Some(value.as_str()) {
        Ok(())
    } else {
        Err(SecretError::Unavailable(
            "probe value did not round-trip".into(),
        ))
    }
}

/// `secrets.json`: a flat JSON object of key -> value, rewritten atomically
/// with owner-only permissions (0600 on Unix; the profile ACL on Windows).
pub struct FileSecretStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileSecretStore {
    pub fn new(dir: &Path) -> Self {
        FileSecretStore {
            path: dir.join(SECRETS_FILE),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<Map<String, Value>, SecretError> {
        match std::fs::read(&self.path) {
            Ok(b) => match serde_json::from_slice::<Value>(&b) {
                Ok(Value::Object(m)) => Ok(m),
                // A damaged file holds nothing usable: set it aside as
                // `secrets.corrupt-<ts>.json` (like `account.json`) and start
                // over, so the next write can't silently drop what was in it.
                _ => {
                    self.quarantine()?;
                    Ok(Map::new())
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
            Err(e) => Err(io_err(&e)),
        }
    }

    /// Rename a damaged `secrets.json` out of the way (kept owner-only).
    fn quarantine(&self) -> Result<(), SecretError> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| SecretError::Io("no parent directory".into()))?;
        let mut target = dir.join(format!("secrets.corrupt-{}.json", crate::model::now_ms()));
        if target.exists() {
            target = dir.join(format!(
                "secrets.corrupt-{}-{}.json",
                crate::model::now_ms(),
                crate::model::new_id()
            ));
        }
        match std::fs::rename(&self.path, &target) {
            Ok(()) => {
                let _ = restrict(&target);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(&e)),
        }
    }

    fn write(&self, map: &Map<String, Value>) -> Result<(), SecretError> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| SecretError::Io("no parent directory".into()))?;
        std::fs::create_dir_all(dir).map_err(|e| io_err(&e))?;
        let bytes = serde_json::to_vec_pretty(map).map_err(|e| SecretError::Io(e.to_string()))?;
        let tmp = dir.join(format!(".{SECRETS_FILE}.tmp-{}", crate::model::new_id()));
        let result = (|| -> std::io::Result<()> {
            let mut f = owner_only_create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, &self.path)?;
            restrict(&self.path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result.map_err(|e| io_err(&e))
    }
}

fn io_err(e: &std::io::Error) -> SecretError {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => SecretError::Denied(e.kind().to_string()),
        _ => SecretError::Io(e.to_string()),
    }
}

#[cfg(unix)]
fn owner_only_create(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn owner_only_create(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
fn restrict(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

impl SecretStore for FileSecretStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        Ok(self
            .read()?
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut map = self.read()?;
        map.insert(key.to_string(), Value::String(value.to_string()));
        self.write(&map)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut map = self.read()?;
        if map.remove(key).is_none() {
            return Ok(());
        }
        if map.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(io_err(&e)),
            };
        }
        self.write(&map)
    }

    fn kind(&self) -> SecretStoreKind {
        SecretStoreKind::File
    }
}

/// In-memory store for tests. Records every operation (`"set:key"`,
/// `"delete:key"`) so ordering can be asserted, and can be told to fail.
#[derive(Default)]
pub struct MemorySecretStore {
    map: Mutex<HashMap<String, String>>,
    log: Mutex<Vec<String>>,
    fail_writes: AtomicBool,
    fail_reads: AtomicBool,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_fail_writes(&self, fail: bool) {
        self.fail_writes.store(fail, Ordering::SeqCst);
    }
    pub fn set_fail_reads(&self, fail: bool) {
        self.fail_reads.store(fail, Ordering::SeqCst);
    }
    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
    pub fn keys(&self) -> Vec<String> {
        let mut k: Vec<String> = self
            .map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect();
        k.sort();
        k
    }
    fn record(&self, op: String) {
        self.log.lock().unwrap_or_else(|p| p.into_inner()).push(op);
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(SecretError::Denied("read denied (test)".into()));
        }
        Ok(self
            .map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            self.record(format!("set-failed:{key}"));
            return Err(SecretError::Denied("write denied (test)".into()));
        }
        self.record(format!("set:{key}"));
        self.map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.record(format!("delete:{key}"));
        self.map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(key);
        Ok(())
    }
    fn kind(&self) -> SecretStoreKind {
        SecretStoreKind::System
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trip_and_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileSecretStore::new(dir.path());
        assert_eq!(s.get("a").unwrap(), None);
        s.set("a", "1").unwrap();
        s.set("b", "2").unwrap();
        assert_eq!(s.get("a").unwrap().as_deref(), Some("1"));
        s.delete("a").unwrap();
        assert_eq!(s.get("a").unwrap(), None);
        assert_eq!(s.get("b").unwrap().as_deref(), Some("2"));
        s.delete("b").unwrap();
        assert!(!s.path().exists(), "an empty store leaves no file behind");
        s.delete("missing").unwrap();
        probe(&s).unwrap();
        // No temp files are left in the directory.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn file_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let s = FileSecretStore::new(dir.path());
        s.set("session:https://rh.acme.dev:u1", "{}").unwrap();
        let mode = std::fs::metadata(s.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // A file that was loosened by hand is tightened on the next write.
        std::fs::set_permissions(s.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        s.set("k", "v").unwrap();
        let mode = std::fs::metadata(s.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    fn corrupt_files(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("secrets.corrupt-") && n.ends_with(".json"))
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn damaged_file_is_quarantined_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let damaged = b"{\"session:https://a.dev:u1\": \"keep me\", oops";
        std::fs::write(dir.path().join(SECRETS_FILE), damaged).unwrap();
        let s = FileSecretStore::new(dir.path());
        assert_eq!(s.get("a").unwrap(), None);
        // The damaged bytes are set aside, byte for byte.
        let moved = corrupt_files(dir.path());
        assert_eq!(moved.len(), 1, "{moved:?}");
        assert_eq!(std::fs::read(&moved[0]).unwrap(), damaged);
        s.set("a", "1").unwrap();
        assert_eq!(s.get("a").unwrap().as_deref(), Some("1"));
        assert_eq!(corrupt_files(dir.path()), moved);
        // A file that is valid JSON but not an object is damaged too; a set
        // on it quarantines before writing.
        std::fs::write(s.path(), b"[1, 2]").unwrap();
        s.set("b", "2").unwrap();
        assert_eq!(corrupt_files(dir.path()).len(), 2);
        assert_eq!(s.get("a").unwrap(), None);
        assert_eq!(s.get("b").unwrap().as_deref(), Some("2"));
    }

    #[test]
    fn memory_store_failures_and_log() {
        let s = MemorySecretStore::new();
        s.set("a", "1").unwrap();
        s.set_fail_writes(true);
        assert!(s.set("a", "2").is_err());
        assert_eq!(s.get("a").unwrap().as_deref(), Some("1"));
        s.set_fail_reads(true);
        assert!(matches!(s.get("a"), Err(SecretError::Denied(_))));
        assert_eq!(s.log(), vec!["set:a", "set-failed:a"]);
        let msg = SecretError::Denied("write denied (test)".into()).to_string();
        assert!(!msg.contains('1'));
    }
}
