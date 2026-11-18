//! Where the RomM API token is kept.
//!
//! The token is a long-lived credential: with the scopes this app asks for it can read the whole
//! library and overwrite saves. So on a normal install it goes into the operating system's
//! keychain, where it is protected by the user's login rather than by a file's permissions:
//!
//! * Windows: the Credential Manager (a generic credential, local to this machine).
//! * Linux: the Secret Service (GNOME Keyring, KWallet, KeePassXC, ...), over D-Bus.
//!
//! Two situations keep a plain file in the config folder (`token`, mode 0600) instead:
//!
//! * Portable mode (`ROMM_DESKTOP_HOME`) and `ROMM_DESKTOP_KEYCHAIN=off`: everything the user asked
//!   to keep in one folder stays there. This is also what the test and smoke harnesses use.
//! * No keychain is reachable (a headless box, no Secret Service running, a locked keyring nobody can
//!   unlock). [`store_token`] then falls back to the file and says so in [`TokenStorage`], so the
//!   caller can tell the user rather than quietly keeping a secret somewhere weaker than they expect.
//!
//! A token file written by an earlier version is moved into the keychain the first time it is read,
//! and only deleted once the keychain has been read back with the same value.
//!
//! The keychain is reached through the small [`Vault`] trait so that the logic above (fallbacks,
//! migration, what gets deleted when) is tested with an in-memory vault and never touches the
//! keychain of whoever runs the tests.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use crate::config::AppDirs;
use crate::error::{Error, Result};

/// The keychain item's service and user: together they name the one item this app keeps.
const KEYCHAIN_SERVICE: &str = "app.romm.romm-desktop";
const KEYCHAIN_USER: &str = "api-token";

/// Where [`store_token`] put the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenStorage {
    /// In the operating system's keychain.
    Keychain,
    /// In the `token` file of the config folder. `why_not_keychain` is `None` when that was the
    /// intent (portable mode, or the keychain switched off), and the keychain's own error when it
    /// was a fallback.
    File { why_not_keychain: Option<String> },
}

impl TokenStorage {
    /// What to tell the user, when the token ended up somewhere other than they would expect: only a
    /// fallback from the keychain is worth a warning.
    pub fn warning(&self, dirs: &AppDirs) -> Option<String> {
        match self {
            TokenStorage::File { why_not_keychain: Some(why) } => Some(format!(
                "The OS keychain could not be used ({why}), so the sign-in token was saved to {} instead. \
                 It is readable only by your user account, but is not encrypted.",
                dirs.token_file().display()
            )),
            _ => None,
        }
    }
}

/// A place that can hold exactly one secret: the keychain item, or a stand-in for it in tests.
/// Errors are the place's own words, for the user.
pub(crate) trait Vault {
    fn get(&self) -> std::result::Result<Option<String>, String>;
    fn set(&self, secret: &str) -> std::result::Result<(), String>;
    /// Deleting what is not there is not an error.
    fn delete(&self) -> std::result::Result<(), String>;
}

pub(crate) enum Backend {
    /// The file only (portable mode, keychain switched off).
    File,
    Keychain(Box<dyn Vault>),
    /// The keychain was wanted but could not be reached; the file stands in and the reason is kept.
    KeychainUnavailable(String),
}

fn backend(dirs: &AppDirs) -> Backend {
    if !dirs.keychain {
        return Backend::File;
    }
    match OsKeychain::connect() {
        Ok(k) => Backend::Keychain(Box::new(k)),
        Err(why) => Backend::KeychainUnavailable(why),
    }
}

// ------------------------------------------------------------------------------ public functions

/// Keeps `token` (surrounding whitespace is dropped) and says where it went. The keychain is
/// tried first and read back; if that fails the token is written to the file instead, so that
/// signing in never fails for want of a keychain, and the result says so (see
/// [`TokenStorage::warning`]). Whatever the old place held is removed once the new one is good.
pub fn store_token(dirs: &AppDirs, token: &str) -> Result<TokenStorage> {
    store_in(dirs, token, &backend(dirs))
}

/// The saved token, if there is one and it can be read.
pub fn load_token(dirs: &AppDirs) -> Option<String> {
    read_token(dirs).ok().flatten()
}

/// The saved token. `Ok(None)` is "nothing saved"; an error means a token may well be saved but
/// cannot be read right now (the keychain is locked or unreachable and no file has it either),
/// which is different from having none and is worth telling the user.
pub fn read_token(dirs: &AppDirs) -> Result<Option<String>> {
    read_in(dirs, &backend(dirs))
}

/// Forgets the token everywhere it might be: the keychain and the file. Both are always tried; if
/// either fails the error says which, because a token that is still stored is not a sign-out.
pub fn delete_token(dirs: &AppDirs) -> Result<()> {
    delete_in(dirs, &backend(dirs))
}

// ------------------------------------------------------------------------------------- the logic

pub(crate) fn store_in(dirs: &AppDirs, token: &str, backend: &Backend) -> Result<TokenStorage> {
    let token = token.trim();
    if token.is_empty() {
        return Err(Error::msg("there is no token to save"));
    }
    let why_not_keychain = match backend {
        Backend::File => None,
        Backend::KeychainUnavailable(why) => Some(why.clone()),
        Backend::Keychain(vault) => match put_and_verify(vault.as_ref(), token) {
            Ok(()) => {
                // The keychain has it. A file left by an earlier version (or an earlier fallback)
                // holds an older copy in the clear: remove it.
                remove_token_file(dirs)?;
                return Ok(TokenStorage::Keychain);
            }
            Err(why) => Some(why),
        },
    };
    write_private_file(&dirs.token_file(), token.as_bytes())?;
    Ok(TokenStorage::File { why_not_keychain })
}

/// Writes to the vault and reads it back: some stores accept a write they then do not keep (a keyring
/// that locked meanwhile), and the file must not be deleted on their word alone.
fn put_and_verify(vault: &dyn Vault, token: &str) -> std::result::Result<(), String> {
    vault.set(token)?;
    match vault.get()? {
        Some(back) if back == token => Ok(()),
        Some(_) => Err("the keychain returned a different value than was saved".into()),
        None => Err("the keychain did not keep the token it was given".into()),
    }
}

pub(crate) fn read_in(dirs: &AppDirs, backend: &Backend) -> Result<Option<String>> {
    let file = read_token_file(dirs);
    let vault = match backend {
        Backend::File => return Ok(file),
        Backend::KeychainUnavailable(why) => {
            return match file {
                Some(t) => Ok(Some(t)),
                None => Err(Error::msg(format!("the OS keychain could not be reached: {why}"))),
            }
        }
        Backend::Keychain(v) => v.as_ref(),
    };
    match vault.get() {
        Ok(Some(token)) => {
            if file.is_some() {
                // Stale by construction: `store_in` removes the file when the keychain takes the token.
                if let Err(e) = remove_token_file(dirs) {
                    tracing::warn!("could not remove the old token file: {e}");
                }
            }
            Ok(Some(token))
        }
        Ok(None) => match file {
            None => Ok(None),
            Some(token) => {
                // An earlier version kept it in the file. Move it, once the keychain has proved it
                // holds it; if the keychain will not take it, keep using the file as before.
                match put_and_verify(vault, &token) {
                    Ok(()) => {
                        if let Err(e) = remove_token_file(dirs) {
                            tracing::warn!("the token was moved to the keychain but the old file could not be removed: {e}");
                        }
                    }
                    Err(why) => tracing::warn!("could not move the token into the OS keychain: {why}"),
                }
                Ok(Some(token))
            }
        },
        Err(why) => match file {
            Some(t) => {
                tracing::warn!("could not read the OS keychain ({why}); using the token file");
                Ok(Some(t))
            }
            None => Err(Error::msg(format!("the OS keychain could not be read: {why}"))),
        },
    }
}

pub(crate) fn delete_in(dirs: &AppDirs, backend: &Backend) -> Result<()> {
    let mut failures = Vec::new();
    let mut unreachable = None;
    match backend {
        Backend::Keychain(vault) => {
            if let Err(why) = vault.delete() {
                failures.push(format!("from the OS keychain: {why}"));
            }
        }
        Backend::KeychainUnavailable(why) => unreachable = Some(why),
        Backend::File => {}
    }
    match remove_token_file(dirs) {
        // A keychain that cannot be reached cannot be cleared, but if the token was in the file
        // (which is where `store_in` put it when it could not be reached) there is nothing in it to clear.
        // Only when there was no file is the token possibly still out of reach in there.
        Ok(removed) => {
            if let (Some(why), false) = (unreachable, removed) {
                failures.push(format!("from the OS keychain (which cannot be reached: {why})"));
            }
        }
        Err(e) => failures.push(format!("from {}: {e}", dirs.token_file().display())),
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Error::msg(format!("the saved token could not be removed {}", failures.join("; and "))))
    }
}

// -------------------------------------------------------------------------------------- the file

fn read_token_file(dirs: &AppDirs) -> Option<String> {
    fs::read_to_string(dirs.token_file()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Whether there was a file to remove.
fn remove_token_file(dirs: &AppDirs) -> std::io::Result<bool> {
    match fs::remove_file(dirs.token_file()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Writes `bytes` to `path` through a temp file that is created private (0600 on Unix) from the
/// start. The earlier code wrote the file with the default mode and tightened it afterwards, which
/// left the token readable to other users for a moment. On Windows the file keeps the ACL it inherits
/// from the user's profile folder, which only the user, administrators and the system can read.
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut name = path.file_name().map(|n| n.to_owned()).unwrap_or_default();
    name.push(format!(".{}-{}.romm-tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let tmp = path.with_file_name(name);

    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let written = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

// ---------------------------------------------------------------------------- the OS keychain

use keyring_core::api::CredentialStore;

/// The one keychain item, in whatever store this platform has.
struct OsKeychain {
    store: Arc<CredentialStore>,
}

impl OsKeychain {
    /// Connects to the platform's credential store (on Linux this opens a D-Bus session, so it is
    /// done once per operation rather than kept: the app reads the token at start-up and at login,
    /// not per request, and a connection kept for days would not survive the keyring restarting).
    fn connect() -> std::result::Result<OsKeychain, String> {
        platform_store().map(|store| OsKeychain { store })
    }

    fn entry(&self) -> std::result::Result<keyring_core::Entry, String> {
        self.store
            .build(KEYCHAIN_SERVICE, KEYCHAIN_USER, entry_modifiers().as_ref())
            .map_err(|e| e.to_string())
    }
}

impl Vault for OsKeychain {
    fn get(&self) -> std::result::Result<Option<String>, String> {
        match self.entry()?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, secret: &str) -> std::result::Result<(), String> {
        self.entry()?.set_password(secret).map_err(|e| e.to_string())
    }

    fn delete(&self) -> std::result::Result<(), String> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(target_os = "linux")]
fn platform_store() -> std::result::Result<Arc<CredentialStore>, String> {
    let store = dbus_secret_service_keyring_store::Store::new()
        .map_err(|e| format!("no Secret Service (GNOME Keyring, KWallet, ...) is reachable: {e}"))?;
    Ok(store)
}

#[cfg(windows)]
fn platform_store() -> std::result::Result<Arc<CredentialStore>, String> {
    let store = windows_native_keyring_store::Store::new().map_err(|e| e.to_string())?;
    Ok(store)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_store() -> std::result::Result<Arc<CredentialStore>, String> {
    Err("this platform has no supported keychain".into())
}

/// Windows credentials default to roaming with the user's domain profile; a token for one LAN server
/// and one registered device should stay on this machine. (Other stores reject modifiers they do not
/// know, so this is Windows-only.)
#[cfg(windows)]
fn entry_modifiers() -> Option<std::collections::HashMap<&'static str, &'static str>> {
    Some(std::collections::HashMap::from([("persistence", "Local")]))
}

#[cfg(not(windows))]
fn entry_modifiers() -> Option<std::collections::HashMap<&'static str, &'static str>> {
    None
}

// ------------------------------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// An in-memory keychain item that can be told to misbehave.
    #[derive(Default)]
    struct FakeVault {
        value: Mutex<Option<String>>,
        fail_get: bool,
        fail_set: bool,
        fail_delete: bool,
        /// Accepts a write and does not keep it.
        forgetful: bool,
    }

    impl FakeVault {
        fn holding(&self) -> Option<String> {
            self.value.lock().unwrap().clone()
        }
    }

    impl Vault for Arc<FakeVault> {
        fn get(&self) -> std::result::Result<Option<String>, String> {
            if self.fail_get {
                return Err("keyring is locked".into());
            }
            Ok(self.holding())
        }
        fn set(&self, secret: &str) -> std::result::Result<(), String> {
            if self.fail_set {
                return Err("keyring is locked".into());
            }
            if !self.forgetful {
                *self.value.lock().unwrap() = Some(secret.to_string());
            }
            Ok(())
        }
        fn delete(&self) -> std::result::Result<(), String> {
            if self.fail_delete {
                return Err("keyring is locked".into());
            }
            *self.value.lock().unwrap() = None;
            Ok(())
        }
    }

    fn dirs(t: &tempfile::TempDir, keychain: bool) -> AppDirs {
        AppDirs { config_dir: t.path().join("c"), data_dir: t.path().join("d"), keychain }
    }

    fn kc(v: &Arc<FakeVault>) -> Backend {
        Backend::Keychain(Box::new(v.clone()))
    }

    fn write_legacy_file(d: &AppDirs, token: &str) {
        fs::create_dir_all(&d.config_dir).unwrap();
        fs::write(d.token_file(), token).unwrap();
    }

    #[cfg(unix)]
    fn mode_of(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    // ---- storing

    #[test]
    fn a_token_goes_into_the_keychain_and_no_file_is_made() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        let v = Arc::new(FakeVault::default());
        let stored = store_in(&d, " rmm_abc\n", &kc(&v)).unwrap();
        assert_eq!(stored, TokenStorage::Keychain);
        assert_eq!(v.holding().as_deref(), Some("rmm_abc"), "trimmed");
        assert!(!d.token_file().exists(), "nothing in the clear");
        assert!(stored.warning(&d).is_none());
    }

    #[test]
    fn storing_in_the_keychain_removes_the_old_plaintext_file() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        write_legacy_file(&d, "rmm_old");
        let v = Arc::new(FakeVault::default());
        store_in(&d, "rmm_new", &kc(&v)).unwrap();
        assert_eq!(v.holding().as_deref(), Some("rmm_new"));
        assert!(!d.token_file().exists());
    }

    #[test]
    fn a_keychain_that_refuses_the_write_falls_back_to_a_private_file_and_says_so() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        let v = Arc::new(FakeVault { fail_set: true, ..Default::default() });
        let stored = store_in(&d, "rmm_abc", &kc(&v)).unwrap();
        assert_eq!(stored, TokenStorage::File { why_not_keychain: Some("keyring is locked".into()) });
        assert_eq!(fs::read_to_string(d.token_file()).unwrap(), "rmm_abc");
        let warning = stored.warning(&d).expect("a fallback is worth a warning");
        assert!(warning.contains("keyring is locked") && warning.contains("not encrypted"), "{warning}");
        #[cfg(unix)]
        assert_eq!(mode_of(&d.token_file()), 0o600);
    }

    #[test]
    fn a_keychain_that_does_not_keep_what_it_was_given_is_not_trusted() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        write_legacy_file(&d, "rmm_old");
        let v = Arc::new(FakeVault { forgetful: true, ..Default::default() });
        let stored = store_in(&d, "rmm_new", &kc(&v)).unwrap();
        assert!(matches!(&stored, TokenStorage::File { why_not_keychain: Some(w) } if w.contains("did not keep")), "{stored:?}");
        assert_eq!(fs::read_to_string(d.token_file()).unwrap(), "rmm_new", "and the file holds the new token, not the old");
    }

    #[test]
    fn an_unreachable_keychain_falls_back_to_the_file() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        let stored = store_in(&d, "rmm_abc", &Backend::KeychainUnavailable("no D-Bus session".into())).unwrap();
        assert_eq!(stored, TokenStorage::File { why_not_keychain: Some("no D-Bus session".into()) });
        assert_eq!(fs::read_to_string(d.token_file()).unwrap(), "rmm_abc");
    }

    #[test]
    fn portable_mode_keeps_the_file_by_design_and_does_not_warn() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, false);
        let stored = store_in(&d, "rmm_abc", &Backend::File).unwrap();
        assert_eq!(stored, TokenStorage::File { why_not_keychain: None });
        assert!(stored.warning(&d).is_none(), "the user chose this");
        assert_eq!(read_in(&d, &Backend::File).unwrap().as_deref(), Some("rmm_abc"));
        #[cfg(unix)]
        assert_eq!(mode_of(&d.token_file()), 0o600);
    }

    #[test]
    fn an_empty_token_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, false);
        assert!(store_in(&d, " \n", &Backend::File).is_err());
        assert!(!d.token_file().exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_token_file_is_private_from_the_moment_it_exists() {
        // Even under a permissive umask the file is never made readable to others, not even
        // briefly: it is created 0600 rather than chmod-ed afterwards. (The umask is process-wide;
        // no other test in this crate reads it.)
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("token");
        let old = unsafe { libc::umask(0o000) };
        let r = write_private_file(&path, b"rmm_abc");
        unsafe { libc::umask(old) };
        r.unwrap();
        assert_eq!(mode_of(&path), 0o600);
        // And no temp file is left behind.
        let leftovers: Vec<_> = fs::read_dir(t.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("token")]);
    }

    #[test]
    fn rewriting_the_token_file_replaces_it() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, false);
        store_in(&d, "rmm_one", &Backend::File).unwrap();
        store_in(&d, "rmm_two", &Backend::File).unwrap();
        assert_eq!(read_in(&d, &Backend::File).unwrap().as_deref(), Some("rmm_two"));
    }

    // ---- reading

    #[test]
    fn nothing_saved_is_none_not_an_error() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        assert_eq!(read_in(&d, &kc(&Arc::new(FakeVault::default()))).unwrap(), None);
        assert_eq!(read_in(&dirs(&t, false), &Backend::File).unwrap(), None);
    }

    #[test]
    fn the_keychain_is_read_first_and_a_stale_file_is_removed() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        write_legacy_file(&d, "rmm_stale");
        let v = Arc::new(FakeVault::default());
        *v.value.lock().unwrap() = Some("rmm_current".into());
        assert_eq!(read_in(&d, &kc(&v)).unwrap().as_deref(), Some("rmm_current"));
        assert!(!d.token_file().exists());
    }

    #[test]
    fn a_token_file_from_an_earlier_version_is_moved_into_the_keychain() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        write_legacy_file(&d, "rmm_old\n");
        let v = Arc::new(FakeVault::default());
        assert_eq!(read_in(&d, &kc(&v)).unwrap().as_deref(), Some("rmm_old"), "the sign-in survives the upgrade");
        assert_eq!(v.holding().as_deref(), Some("rmm_old"), "now in the keychain");
        assert!(!d.token_file().exists(), "and gone from disk");
        // A second read comes from the keychain alone.
        assert_eq!(read_in(&d, &kc(&v)).unwrap().as_deref(), Some("rmm_old"));
    }

    #[test]
    fn the_file_is_kept_when_the_keychain_will_not_take_the_move() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        write_legacy_file(&d, "rmm_old");
        for v in [
            Arc::new(FakeVault { fail_set: true, ..Default::default() }),
            Arc::new(FakeVault { forgetful: true, ..Default::default() }),
        ] {
            assert_eq!(read_in(&d, &kc(&v)).unwrap().as_deref(), Some("rmm_old"), "still signed in");
            assert_eq!(fs::read_to_string(d.token_file()).unwrap(), "rmm_old", "and the only copy is not deleted");
        }
    }

    #[test]
    fn an_unreadable_keychain_uses_the_file_if_there_is_one() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        write_legacy_file(&d, "rmm_fallback");
        let v = Arc::new(FakeVault { fail_get: true, ..Default::default() });
        assert_eq!(read_in(&d, &kc(&v)).unwrap().as_deref(), Some("rmm_fallback"));
        assert_eq!(read_in(&d, &Backend::KeychainUnavailable("no bus".into())).unwrap().as_deref(), Some("rmm_fallback"));
        assert!(d.token_file().exists());
    }

    #[test]
    fn an_unreadable_keychain_with_no_file_is_an_error_not_a_missing_token() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        let v = Arc::new(FakeVault { fail_get: true, ..Default::default() });
        let err = read_in(&d, &kc(&v)).unwrap_err().to_string();
        assert!(err.contains("keychain") && err.contains("keyring is locked"), "{err}");
        let err = read_in(&d, &Backend::KeychainUnavailable("no bus".into())).unwrap_err().to_string();
        assert!(err.contains("no bus"), "{err}");
    }

    // ---- deleting

    #[test]
    fn signing_out_clears_the_keychain_and_the_file() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        write_legacy_file(&d, "rmm_leftover");
        let v = Arc::new(FakeVault::default());
        *v.value.lock().unwrap() = Some("rmm_current".into());
        delete_in(&d, &kc(&v)).unwrap();
        assert_eq!(v.holding(), None);
        assert!(!d.token_file().exists());
        delete_in(&d, &kc(&v)).expect("deleting what is not there is fine");
    }

    #[test]
    fn a_token_that_could_not_be_removed_is_reported_and_the_file_is_removed_anyway() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        write_legacy_file(&d, "rmm_leftover");
        let v = Arc::new(FakeVault { fail_delete: true, ..Default::default() });
        let err = delete_in(&d, &kc(&v)).unwrap_err().to_string();
        assert!(err.contains("OS keychain") && err.contains("keyring is locked"), "{err}");
        assert!(!d.token_file().exists(), "one failure does not stop the other removal");

        // No file left and the keychain out of reach: the token may still be in there.
        let err = delete_in(&d, &Backend::KeychainUnavailable("no bus".into())).unwrap_err().to_string();
        assert!(err.contains("cannot be reached") && err.contains("no bus"), "{err}");
    }

    #[test]
    fn signing_out_where_the_keychain_never_worked_is_not_an_alarm() {
        // A headless machine: `store_in` fell back to the file, so the file is all there is to remove.
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        let down = Backend::KeychainUnavailable("no bus".into());
        store_in(&d, "rmm_abc", &down).unwrap();
        delete_in(&d, &down).expect("the token was in the file, and the file is gone");
        assert!(!d.token_file().exists());
    }

    #[test]
    fn signing_out_in_portable_mode_removes_the_file() {
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, false);
        store_in(&d, "rmm_abc", &Backend::File).unwrap();
        delete_in(&d, &Backend::File).unwrap();
        assert!(!d.token_file().exists());
        delete_in(&d, &Backend::File).expect("nothing to remove is fine");
    }

    // ---- the real thing

    /// Round-trips a token through this machine's actual keychain, under a throwaway service name's
    /// worth of care: it uses the app's real item, so it refuses to run if one already exists.
    /// `cargo test -p romm-core real_os_keychain -- --ignored` on a desktop session.
    #[test]
    #[ignore = "uses the real OS keychain of whoever runs it"]
    fn real_os_keychain_roundtrip() {
        let k = OsKeychain::connect().expect("an OS keychain should be reachable in a desktop session");
        assert_eq!(k.get().expect("read"), None, "the app's real token is already in this keychain; sign out first");
        let t = tempfile::tempdir().unwrap();
        let d = dirs(&t, true);
        let backend = Backend::Keychain(Box::new(k));

        assert_eq!(store_in(&d, "rmm_real_keychain_test", &backend).unwrap(), TokenStorage::Keychain);
        assert!(!d.token_file().exists());
        assert_eq!(read_in(&d, &backend).unwrap().as_deref(), Some("rmm_real_keychain_test"));
        delete_in(&d, &backend).unwrap();
        assert_eq!(read_in(&d, &backend).unwrap(), None);
    }
}
