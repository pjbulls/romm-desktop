//! Small JSON database of what's installed and what each save looked like at its
//! last successful sync. Written atomically.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::saves::write_atomic;

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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SaveRecord {
    /// Bundle members (relative to the emulator data root); empty for single-file saves.
    #[serde(default)]
    pub files: Vec<String>,
    /// md5 of the bytes at the last successful upload/download.
    #[serde(default)]
    pub last_hash: Option<String>,
    /// Content identity of `files` at that moment (see `saves::fingerprint`).
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub server_save_id: Option<i64>,
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
}

pub fn save_key(rom_id: i64, emulator: &str) -> String {
    format!("{rom_id}:{emulator}")
}

pub fn state_key(rom_id: i64, emulator: &str, name: &str) -> String {
    format!("{rom_id}:{emulator}:{name}")
}

impl State {
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
        s.save(&p).unwrap();
        let back = State::load(&p).unwrap();
        assert_eq!(back.saves["7:dolphin"].last_hash.as_deref(), Some("abc"));
    }
}
