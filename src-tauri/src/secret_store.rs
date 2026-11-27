//! OS keychain for session tokens (AC-AUTH-7, 04 §3.3), with the 0600 file
//! fallback from `reqharbor-core`.
//!
//! * Service `dev.reqharbor.desktop`; one entry per session,
//!   `session:<serverOrigin>:<userId>`, holding one JSON value.
//! * macOS uses the file store unless the build was made with
//!   `REQHARBOR_MACOS_KEYCHAIN=1` (only set by CI with Developer ID signing,
//!   DEV-7): ad-hoc signatures change on every release and the keychain ACL
//!   would prompt or deny after each update.
//! * `REQHARBOR_SECRET_STORE=file` forces the file store (dev builds, tests).
//! * Probe: a set/get/delete round trip; any failure (no Secret Service on
//!   a headless Linux box, a locked keychain) falls back to the file store.
//!   It is lazy ([`AppSecrets`]): it runs on the first real use of the store
//!   (loading a saved sign-in, or signing in), never at a plain app start,
//!   so users who don't use accounts never see a keychain prompt (UX §2).

use reqharbor_core::secrets::{self, FileSecretStore, SecretError, SecretStore, SecretStoreKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub const SERVICE: &str = "dev.reqharbor.desktop";
/// A keychain that does not answer within this time is treated as missing.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Shown under Settings › Account when the file store is used on macOS.
pub const MACOS_FILE_NOTE: &str = "Your sign-in is stored in a protected file because this build isn't signed for the macOS keychain.";
/// Shown when no keychain was found (AC-AUTH-7).
pub const FILE_NOTE: &str =
    "Your sign-in is stored in a file because no system keychain was found.";

/// Run a keychain call on its own thread: the Secret Service client blocks
/// on an internal executor and must never run on an async runtime thread.
fn on_thread<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, SecretError> + Send + 'static,
) -> Result<T, SecretError> {
    std::thread::spawn(f)
        .join()
        .unwrap_or_else(|_| Err(SecretError::Unavailable("keychain call panicked".into())))
}

fn map_err(e: keyring::Error) -> SecretError {
    // Never include the error's payload: some platform errors carry the
    // attributes of the entry, and the message stays short.
    match e {
        keyring::Error::NoStorageAccess(_) => {
            SecretError::Denied("no access to the keychain".into())
        }
        keyring::Error::PlatformFailure(_) => SecretError::Unavailable("keychain failure".into()),
        keyring::Error::BadEncoding(_) => SecretError::Io("keychain value is not UTF-8".into()),
        keyring::Error::TooLong(..) | keyring::Error::Invalid(..) => {
            SecretError::Io("keychain entry rejected".into())
        }
        keyring::Error::Ambiguous(_) => SecretError::Io("several keychain entries match".into()),
        keyring::Error::NoEntry => SecretError::Io("no entry".into()),
        _ => SecretError::Unavailable("keychain error".into()),
    }
}

/// The system keychain through the `keyring` crate.
pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        let key = key.to_string();
        on_thread(move || {
            let entry = keyring::Entry::new(SERVICE, &key).map_err(map_err)?;
            match entry.get_password() {
                Ok(v) => Ok(Some(v)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(map_err(e)),
            }
        })
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let (key, value) = (key.to_string(), value.to_string());
        on_thread(move || {
            keyring::Entry::new(SERVICE, &key)
                .and_then(|e| e.set_password(&value))
                .map_err(map_err)
        })
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        let key = key.to_string();
        on_thread(move || {
            let entry = keyring::Entry::new(SERVICE, &key).map_err(map_err)?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(map_err(e)),
            }
        })
    }

    fn kind(&self) -> SecretStoreKind {
        SecretStoreKind::System
    }
}

/// Which store to try first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preference {
    Keychain,
    /// File store, with the reason shown in Settings.
    File(&'static str),
}

/// The build/environment rule (pure, for tests).
pub fn preference(forced_file: bool, os: &str, macos_keychain_build: bool) -> Preference {
    if forced_file {
        Preference::File(FILE_NOTE)
    } else if os == "macos" && !macos_keychain_build {
        Preference::File(MACOS_FILE_NOTE)
    } else {
        Preference::Keychain
    }
}

/// The chosen store, its kind and the note Settings shows for it.
pub struct Chosen {
    pub store: Arc<dyn SecretStore>,
    pub kind: SecretStoreKind,
    pub note: Option<&'static str>,
}

/// Probe `candidate` with a timeout; fall back to the file store.
pub fn choose_with(dir: &Path, pref: Preference, candidate: Arc<dyn SecretStore>) -> Chosen {
    let file = || Arc::new(FileSecretStore::new(dir)) as Arc<dyn SecretStore>;
    match pref {
        Preference::File(note) => Chosen {
            store: file(),
            kind: SecretStoreKind::File,
            note: Some(note),
        },
        Preference::Keychain => {
            let (tx, rx) = std::sync::mpsc::channel();
            let probe_store = candidate.clone();
            std::thread::spawn(move || {
                let _ = tx.send(secrets::probe(probe_store.as_ref()));
            });
            match rx.recv_timeout(PROBE_TIMEOUT) {
                Ok(Ok(())) => Chosen {
                    store: candidate,
                    kind: SecretStoreKind::System,
                    note: None,
                },
                _ => Chosen {
                    store: file(),
                    kind: SecretStoreKind::File,
                    note: Some(FILE_NOTE),
                },
            }
        }
    }
}

/// This build's preference (`REQHARBOR_SECRET_STORE`, the OS, DEV-7).
pub fn build_preference() -> Preference {
    let forced = std::env::var("REQHARBOR_SECRET_STORE").is_ok_and(|v| v == "file");
    preference(
        forced,
        std::env::consts::OS,
        option_env!("REQHARBOR_MACOS_KEYCHAIN") == Some("1"),
    )
}

/// The secret store for this run of the app, chosen (and the keychain
/// probed) on first use only. Until then [`AppSecrets::kind`] reports what
/// the preference expects.
pub struct AppSecrets {
    dir: PathBuf,
    pref: Preference,
    candidate: Arc<dyn SecretStore>,
    chosen: OnceLock<Chosen>,
    probes: AtomicUsize,
}

impl AppSecrets {
    pub fn new(dir: &Path, pref: Preference, candidate: Arc<dyn SecretStore>) -> Self {
        AppSecrets {
            dir: dir.to_path_buf(),
            pref,
            candidate,
            chosen: OnceLock::new(),
            probes: AtomicUsize::new(0),
        }
    }

    /// A store that is already chosen (no probe; for tests).
    #[cfg(test)]
    pub fn fixed(chosen: Chosen) -> Self {
        let s = Self::new(Path::new(""), Preference::Keychain, chosen.store.clone());
        let _ = s.chosen.set(chosen);
        s
    }

    /// The app's store: the OS keychain (or the file store), unprobed.
    pub fn for_app(dir: &Path) -> Self {
        Self::new(dir, build_preference(), Arc::new(KeyringStore))
    }

    fn chosen(&self) -> &Chosen {
        self.chosen.get_or_init(|| {
            self.probes.fetch_add(1, Ordering::SeqCst);
            choose_with(&self.dir, self.pref, self.candidate.clone())
        })
    }

    /// How many times the store was chosen (0 or 1; for tests).
    #[cfg(test)]
    pub fn probe_count(&self) -> usize {
        self.probes.load(Ordering::SeqCst)
    }

    /// Where secrets go; never probes.
    pub fn kind(&self) -> SecretStoreKind {
        match (self.chosen.get(), self.pref) {
            (Some(c), _) => c.kind,
            (None, Preference::File(_)) => SecretStoreKind::File,
            (None, Preference::Keychain) => SecretStoreKind::System,
        }
    }

    /// The note Settings shows for the store; never probes.
    pub fn note(&self) -> Option<&'static str> {
        match (self.chosen.get(), self.pref) {
            (Some(c), _) => c.note,
            (None, Preference::File(note)) => Some(note),
            (None, Preference::Keychain) => None,
        }
    }
}

impl SecretStore for AppSecrets {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        self.chosen().store.get(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.chosen().store.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.chosen().store.delete(key)
    }
    fn kind(&self) -> SecretStoreKind {
        AppSecrets::kind(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqharbor_core::secrets::MemorySecretStore;

    #[test]
    fn preference_rules() {
        assert_eq!(
            preference(true, "linux", false),
            Preference::File(FILE_NOTE)
        );
        assert_eq!(
            preference(false, "macos", false),
            Preference::File(MACOS_FILE_NOTE)
        );
        assert_eq!(preference(false, "macos", true), Preference::Keychain);
        assert_eq!(preference(false, "windows", false), Preference::Keychain);
        assert_eq!(preference(false, "linux", false), Preference::Keychain);
    }

    #[test]
    fn a_working_keychain_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let mem = Arc::new(MemorySecretStore::new());
        let c = choose_with(dir.path(), Preference::Keychain, mem.clone());
        assert_eq!(c.kind, SecretStoreKind::System);
        assert!(c.note.is_none());
        // The probe cleaned up after itself.
        assert!(mem.keys().is_empty());
    }

    #[test]
    fn a_failing_keychain_falls_back_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mem = Arc::new(MemorySecretStore::new());
        mem.set_fail_writes(true);
        let c = choose_with(dir.path(), Preference::Keychain, mem);
        assert_eq!(c.kind, SecretStoreKind::File);
        assert_eq!(c.note, Some(FILE_NOTE));
        c.store.set("k", "v").unwrap();
        assert!(dir.path().join(secrets::SECRETS_FILE).exists());
    }

    #[test]
    fn the_app_store_probes_on_first_use_only() {
        let dir = tempfile::tempdir().unwrap();
        let mem = Arc::new(MemorySecretStore::new());
        let s = AppSecrets::new(dir.path(), Preference::Keychain, mem.clone());
        // Creating it and asking what it is never touches the keychain.
        assert_eq!(s.kind(), SecretStoreKind::System);
        assert!(s.note().is_none());
        assert_eq!(s.probe_count(), 0);
        assert!(mem.log().is_empty());
        // The first real call probes once, then uses the keychain.
        assert_eq!(s.get("k").unwrap(), None);
        s.set("k", "v").unwrap();
        assert_eq!(s.probe_count(), 1);
        assert_eq!(
            mem.log(),
            vec!["set:probe:reqharbor", "delete:probe:reqharbor", "set:k"]
        );

        // A failing keychain switches the reported kind after the probe.
        let bad = Arc::new(MemorySecretStore::new());
        bad.set_fail_writes(true);
        let s = AppSecrets::new(dir.path(), Preference::Keychain, bad);
        assert_eq!(s.kind(), SecretStoreKind::System);
        s.set("k", "v").unwrap();
        assert_eq!(s.kind(), SecretStoreKind::File);
        assert_eq!(s.note(), Some(FILE_NOTE));

        // The file preference needs no probe at all.
        let s = AppSecrets::new(
            dir.path(),
            Preference::File(MACOS_FILE_NOTE),
            Arc::new(MemorySecretStore::new()),
        );
        assert_eq!(s.kind(), SecretStoreKind::File);
        assert_eq!(s.note(), Some(MACOS_FILE_NOTE));
    }

    #[test]
    fn the_real_keychain_probe_never_hangs() {
        // In the dev container there is no Secret Service: this must fall
        // back (or, on a desktop, succeed) within the probe timeout.
        let dir = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let c = choose_with(dir.path(), Preference::Keychain, Arc::new(KeyringStore));
        assert!(start.elapsed() < PROBE_TIMEOUT + Duration::from_secs(2));
        assert!(matches!(
            c.kind,
            SecretStoreKind::System | SecretStoreKind::File
        ));
    }
}
