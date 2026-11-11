//! Small JSON database of what's installed and what each save looked like at its
//! last successful sync. Written atomically.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::saves::{self, write_atomic};
use crate::unit::Identity;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocalRom {
    pub rom_id: i64,
    pub name: String,
    pub platform_slug: String,
    /// What was installed (a file, or a folder for multi-file ROMs).
    pub path: PathBuf,
    /// What the emulator is pointed at (e.g. the .m3u or .cue).
    pub launch_path: PathBuf,
    /// File stem of `launch_path`; RetroArch names its save after it.
    pub stem: String,
    /// Server-relative cover art path, kept so the Installed list can show covers.
    #[serde(default)]
    pub cover_path: Option<String>,
    /// What Sigil read from the ROM (title id, save id, cart features). Read once, like Argosy does
    /// when a game is installed; redone if the launch path changes.
    #[serde(default)]
    pub identity: Option<Identity>,
}

/// What was last exchanged with the server for one game's save under one emulator.
///
/// (Earlier versions also tracked the list of files a play session had touched and a fingerprint of
/// them. Saves are now found by game id, so neither exists; an old state file's copies are ignored.)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SaveRecord {
    /// RomM's `content_hash` of the save at the last successful upload/download. Reported back to
    /// the server while the save is unchanged, so it can never look newer than it is.
    #[serde(default)]
    pub last_hash: Option<String>,
    #[serde(default)]
    pub server_save_id: Option<i64>,
    /// The server's `updated_at` for that version.
    #[serde(default)]
    pub server_updated_at: Option<String>,
    /// Sigil's state moved on without the save reaching the server (a claimed save's owner is kept
    /// only there), so Sigil alone would call it unchanged. Cleared once it has been transferred.
    #[serde(default)]
    pub dirty: bool,
}

impl SaveRecord {
    /// The server and this machine now hold the same save: remember what was exchanged, so later
    /// changes are measured from here. `save_id` is kept when the server didn't say which save it
    /// was; a missing `updated_at` is taken to be now, so the save can never look newer than the
    /// server's copy.
    pub fn mark_synced(&mut self, hash: &str, save_id: Option<i64>, server_updated_at: Option<String>) {
        self.last_hash = Some(hash.to_string());
        if save_id.is_some() {
            self.server_save_id = save_id;
        }
        self.server_updated_at = server_updated_at.or_else(|| Some(saves::iso(SystemTime::now())));
        self.dirty = false;
    }
}

/// What a state file looked like at its last successful sync (the server exposes no hash).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateRecord {
    pub server_id: Option<i64>,
    pub local_hash: String,
    pub server_updated_at: String,
    pub server_size: u64,
    /// stat of the local file when `local_hash` was computed, so unchanged big files aren't re-hashed
    #[serde(default)]
    pub local_size: u64,
    #[serde(default)]
    pub local_mtime_ns: u128,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub roms: BTreeMap<i64, LocalRom>,
    /// key = "<rom_id>:<emulator_id>"
    #[serde(default)]
    pub saves: BTreeMap<String, SaveRecord>,
    /// key = "<rom_id>:<emulator_id>:<state file name>"
    #[serde(default)]
    pub states: BTreeMap<String, StateRecord>,
    /// Shared-folder emulators: state files attributed to a game, key = "<rom_id>:<emulator_id>"
    #[serde(default)]
    pub state_names: BTreeMap<String, Vec<String>>,
    /// Sigil's opaque sync state (base64), key = `unit::Route::state_key`. It is what lets a later
    /// collect tell "the save changed" from "the file was only touched or moved".
    #[serde(default)]
    pub sigil: BTreeMap<String, String>,
    /// Names of the unowned saves on a shared volume (Sega CD, Dreamcast, Saturn) as of each route's
    /// last collect, same key as `sigil`. A save that appears after this is the one just played.
    #[serde(default)]
    pub sigil_unowned: BTreeMap<String, Vec<String>>,
}

pub fn save_key(rom_id: i64, emulator: &str) -> String {
    format!("{rom_id}:{emulator}")
}

pub fn state_key(rom_id: i64, emulator: &str, name: &str) -> String {
    format!("{rom_id}:{emulator}:{name}")
}

impl State {
    /// The record for `emulator`'s save of `rom_id`, created if there is none yet.
    pub fn save_record(&mut self, rom_id: i64, emulator: &str) -> &mut SaveRecord {
        self.saves.entry(save_key(rom_id, emulator)).or_default()
    }

    pub fn sigil_state(&self, key: &str) -> Option<Vec<u8>> {
        use base64::Engine;
        let b64 = self.sigil.get(key)?;
        base64::engine::general_purpose::STANDARD.decode(b64).ok().filter(|b| !b.is_empty())
    }

    pub fn set_sigil_state(&mut self, key: &str, state: &[u8]) {
        use base64::Engine;
        if state.is_empty() {
            self.sigil.remove(key);
        } else {
            self.sigil.insert(key.to_string(), base64::engine::general_purpose::STANDARD.encode(state));
        }
    }

    /// Remember that `names` (flat state file names) belong to this game.
    pub fn attribute_states(&mut self, rom_id: i64, emulator: &str, names: &[String]) {
        if names.is_empty() {
            return;
        }
        let list = self.state_names.entry(save_key(rom_id, emulator)).or_default();
        list.extend(names.iter().cloned());
        list.sort();
        list.dedup();
    }

    pub fn load(path: &Path) -> Result<State> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomic(path, &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_missing_file() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("sub/state.json");
        assert!(State::load(&p).unwrap().roms.is_empty());
        let mut s = State::default();
        s.saves.insert(save_key(7, "dolphin"), SaveRecord { last_hash: Some("abc".into()), ..Default::default() });
        s.set_sigil_state("psp|ppsspp_standalone|/x", &[1, 2, 3]);
        s.save(&p).unwrap();
        let back = State::load(&p).unwrap();
        assert_eq!(back.saves["7:dolphin"].last_hash.as_deref(), Some("abc"));
        assert_eq!(back.sigil_state("psp|ppsspp_standalone|/x"), Some(vec![1, 2, 3]));
        assert_eq!(back.sigil_state("other"), None);
    }

    #[test]
    fn a_state_file_from_the_diff_era_still_loads() {
        // The old record had `files` and `fingerprint`; they're gone but must not break loading.
        let old = r#"{"roms":{},"saves":{"7:ppsspp":{"files":["SAVEDATA/X/DATA.BIN"],"last_hash":"h","fingerprint":"f","server_save_id":5}}}"#;
        let s: State = serde_json::from_str(old).unwrap();
        assert_eq!(s.saves["7:ppsspp"].last_hash.as_deref(), Some("h"));
        assert_eq!(s.saves["7:ppsspp"].server_save_id, Some(5));
        assert!(s.sigil.is_empty());
    }

    #[test]
    fn marking_a_save_synced_records_what_was_exchanged_and_clears_dirty() {
        let mut r = SaveRecord { dirty: true, ..Default::default() };
        r.mark_synced("h1", Some(42), Some("2026-01-02T03:04:05Z".into()));
        assert_eq!(r.last_hash.as_deref(), Some("h1"));
        assert_eq!(r.server_save_id, Some(42));
        assert_eq!(r.server_updated_at.as_deref(), Some("2026-01-02T03:04:05Z"));
        assert!(!r.dirty);
    }

    #[test]
    fn a_server_that_names_no_save_leaves_the_known_id_alone() {
        let mut r = SaveRecord { server_save_id: Some(7), ..Default::default() };
        r.mark_synced("h2", None, Some("2026-01-02T03:04:05Z".into()));
        assert_eq!(r.server_save_id, Some(7));
    }

    #[test]
    fn a_missing_server_time_is_now_so_the_save_cannot_look_newer_than_the_server_copy() {
        let mut r = SaveRecord::default();
        let before = saves::iso(SystemTime::now());
        r.mark_synced("h3", Some(1), None);
        let after = saves::iso(SystemTime::now());
        let t = r.server_updated_at.expect("a time is recorded");
        // The format is fixed-width and zero-padded, so a string comparison is a time comparison.
        assert!(before <= t && t <= after, "{before} <= {t} <= {after}");
    }

    #[test]
    fn save_record_creates_the_entry_once_and_keeps_it() {
        let mut s = State::default();
        s.save_record(3, "dolphin").mark_synced("a", Some(9), Some("t".into()));
        s.save_record(3, "dolphin").dirty = true;
        assert_eq!(s.saves.len(), 1);
        let rec = &s.saves["3:dolphin"];
        assert_eq!((rec.last_hash.as_deref(), rec.server_save_id, rec.dirty), (Some("a"), Some(9), true));
    }

    #[test]
    fn clearing_a_sigil_state_removes_it() {
        let mut s = State::default();
        s.set_sigil_state("k", &[9]);
        s.set_sigil_state("k", &[]);
        assert!(s.sigil.is_empty());
    }
}
