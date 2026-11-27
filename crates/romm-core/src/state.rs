//! Small JSON database of what's installed and what each save looked like at its
//! last successful sync. Written atomically.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::saves::{self, write_atomic};
use crate::shared::{merge3, FileLock, LOCK_WAIT};
use crate::unit::Identity;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocalRom {
    pub rom_id: i64,
    pub name: String,
    pub platform_slug: String,
    /// What was installed (a file, or a folder for multi-file ROMs).
    pub path: PathBuf,
    /// The ROM folder in force when it was installed. Uninstalling deletes `path` only if it lies
    /// inside this (or, for a game installed before this was recorded, the current ROM folder), so
    /// a changed setting does not strand old installs and a wrong `path` cannot delete elsewhere.
    #[serde(default)]
    pub root: Option<PathBuf>,
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
    /// The version last exchanged with the server was a hardcore save: it came down carrying
    /// Argosy's hardcore marker, or went up with it (see `hardcore`). While the save is unchanged
    /// it goes on being one.
    #[serde(default)]
    pub hardcore: bool,
    /// The changes made since that exchange were made in a hardcore session but have not reached
    /// the server yet (the upload failed, or the player is offline). Kept so that the next sync,
    /// which is no longer right after the session, still marks them as the hardcore saves they are.
    #[serde(default)]
    pub pending_hardcore: bool,
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
        self.pending_hardcore = false;
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// RomM 5.5+ snapshot sync: what this device holds of each game's channel,
    /// key = `channel_key`.
    #[serde(default)]
    pub channels: BTreeMap<String, ChannelRecord>,
    /// What the file held when this copy last read or wrote it: the common ancestor that `save`
    /// merges against, so that what another program saved meanwhile is kept (see `shared`).
    #[serde(skip)]
    base: Option<serde_json::Value>,
}

/// A copy of a `State` made to do slow work on while the original stays free, and what the copy
/// started from. Hand it back with `State::join`.
pub struct Fork {
    pub state: State,
    started_from: serde_json::Value,
}

pub fn save_key(rom_id: i64, emulator: &str) -> String {
    format!("{rom_id}:{emulator}")
}

/// The channel this app syncs a game through. RomM's own name for the channel a game's autosave
/// slot maps to; the app has no named channels of its own.
pub const DEFAULT_CHANNEL: &str = "default";

pub fn channel_key(rom_id: i64) -> String {
    format!("{rom_id}:{DEFAULT_CHANNEL}")
}

/// A game's channel as this device last knew it: which snapshot it holds (pushed or applied), and the
/// hashes of the save in it. What a sync decides against (see `snapshot_sync::decide`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelRecord {
    /// The channel's id on the server. A channel this device has not written to yet gets an id here
    /// first; the server creates it with the first push.
    pub channel_id: String,
    /// The ROM file the channel is keyed to. A channel whose file changed (the library was rescanned)
    /// is not this one any more.
    pub rom_file_id: i64,
    #[serde(default)]
    pub held_snapshot_id: Option<i64>,
    #[serde(default)]
    pub held_content_hash: Option<String>,
    #[serde(default)]
    pub held_identity_hash: Option<String>,
    /// The held snapshot is a hardcore one.
    #[serde(default)]
    pub held_hardcore: bool,
    /// Changes made since were made in a hardcore session and haven't been pushed yet, so a later
    /// sync, no longer right after that session, still pushes them as hardcore.
    #[serde(default)]
    pub pending_hardcore: bool,
}

pub fn state_key(rom_id: i64, emulator: &str, name: &str) -> String {
    format!("{rom_id}:{emulator}:{name}")
}

impl State {
    /// The record for `emulator`'s save of `rom_id`, created if there is none yet.
    pub fn save_record(&mut self, rom_id: i64, emulator: &str) -> &mut SaveRecord {
        self.saves.entry(save_key(rom_id, emulator)).or_default()
    }

    /// Whether the save this install last exchanged for `rom_id` was a hardcore one, and whether
    /// changes made since, in a hardcore session, still wait to be exchanged.
    ///
    /// Each engine keeps this in its own record, the older sync in the save's (`saves`) and snapshot
    /// sync in the game's channel record (`channels`), and each tells the other when it changes
    /// (`note_exchanged`, `note_pending`), so that it doesn't matter which one ran last, or which
    /// runs next. An install that has only ever used the older one has no channel record yet; what it
    /// knows is in the older record, and that is the answer.
    pub fn hardcore_memory(&self, rom_id: i64, emulator: &str) -> (bool, bool) {
        if let Some(c) = self.channels.get(&channel_key(rom_id)) {
            return (c.held_hardcore, c.pending_hardcore);
        }
        self.saves.get(&save_key(rom_id, emulator)).map_or((false, false), |r| (r.hardcore, r.pending_hardcore))
    }

    /// An exchange with the server has just made the save this install holds for `rom_id` a hardcore
    /// one, or not, and left nothing pending. Both engines' records are told, but only the older one
    /// is made for a casual save: nothing needs remembering of that.
    pub fn note_exchanged(&mut self, rom_id: i64, emulator: &str, hardcore: bool) {
        let key = save_key(rom_id, emulator);
        if hardcore || self.saves.contains_key(&key) {
            let rec = self.saves.entry(key).or_default();
            rec.hardcore = hardcore;
            rec.pending_hardcore = false;
        }
        if let Some(c) = self.channels.get_mut(&channel_key(rom_id)) {
            c.held_hardcore = hardcore;
            c.pending_hardcore = false;
        }
    }

    /// Changes to the save of `rom_id` were made in a hardcore session (or, with `false`, a casual
    /// one) and have not been exchanged yet. Both engines' records are told, where one exists; the
    /// older one is only made for a hardcore session.
    pub fn note_pending(&mut self, rom_id: i64, emulator: &str, pending: bool) {
        let key = save_key(rom_id, emulator);
        if pending || self.saves.contains_key(&key) {
            self.saves.entry(key).or_default().pending_hardcore = pending;
        }
        if let Some(c) = self.channels.get_mut(&channel_key(rom_id)) {
            c.pending_hardcore = pending;
        }
    }

    /// The content hash of what the older (slot-based) sync last exchanged for `emulator`'s save of
    /// `rom_id`, if it ever did. Unlike `save_record` this creates nothing, so asking is free.
    pub fn last_synced_hash(&self, rom_id: i64, emulator: &str) -> Option<&str> {
        self.saves.get(&save_key(rom_id, emulator)).and_then(|r| r.last_hash.as_deref()).filter(|h| !h.is_empty())
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
        let mut state = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str::<State>(&s)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e.into()),
        };
        state.base = state.snapshot();
        Ok(state)
    }

    /// This state as a JSON document: the form `merge3` works on. Everything that is compared goes
    /// through here, so a field a file lacks or an old version wrote is the same on every side.
    fn snapshot(&self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }

    /// Writes this state to `path`, keeping what another program saved there since this one last
    /// read or wrote it: under a lock, the file is read again and merged with this copy record by
    /// record (see `shared::merge3`), and this copy takes on the result. Without that, the desktop
    /// app and the CLI overwrote each other's installs and sync records with whatever they had loaded.
    pub fn save(&mut self, path: &Path) -> Result<()> {
        let _lock = FileLock::acquire(path, LOCK_WAIT);
        let mine = serde_json::to_value(&*self)?;
        let on_disk = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<State>(&text).ok())
            .and_then(|state| state.snapshot());
        let merged = match on_disk {
            Some(theirs) => merge3(self.base.as_ref().unwrap_or(&serde_json::Value::Null), &mine, &theirs),
            // Nothing there, or nothing readable: this copy is all there is.
            None => mine,
        };
        write_atomic(path, &serde_json::to_vec_pretty(&merged)?)?;
        if let Ok(mut adopted) = serde_json::from_value::<State>(merged.clone()) {
            adopted.base = Some(merged);
            *self = adopted;
        }
        Ok(())
    }

    /// A copy to do slow work on (a sync that waits on the network) so the original is not locked
    /// meanwhile.
    pub fn fork(&self) -> Fork {
        Fork { state: self.clone(), started_from: self.snapshot().unwrap_or_default() }
    }

    /// Takes what a fork changed into this state, record by record, and leaves alone whatever was
    /// changed here while the fork was away (an install that finished, a game uninstalled).
    pub fn join(&mut self, fork: Fork) {
        let (Some(theirs), Some(mine)) = (self.snapshot(), fork.state.snapshot()) else { return };
        let merged = merge3(&fork.started_from, &mine, &theirs);
        if let Ok(mut joined) = serde_json::from_value::<State>(merged) {
            joined.base = self.base.take();
            *self = joined;
        }
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

    fn rom(id: i64, name: &str) -> LocalRom {
        LocalRom { rom_id: id, name: name.into(), ..Default::default() }
    }

    fn synced(hash: &str) -> SaveRecord {
        SaveRecord { last_hash: Some(hash.into()), ..Default::default() }
    }

    /// Two programs that have both read the file, as the app and the CLI do at start.
    fn two_programs(path: &Path, start: impl FnOnce(&mut State)) -> (State, State) {
        let mut first = State::default();
        start(&mut first);
        first.save(path).unwrap();
        (State::load(path).unwrap(), State::load(path).unwrap())
    }

    #[test]
    fn a_save_keeps_what_another_program_saved_since_this_one_read_the_file() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("state.json");
        let (mut app, mut cli) = two_programs(&p, |s| {
            s.roms.insert(1, rom(1, "One"));
        });
        // The CLI installs a game and syncs another; then the app, which has not seen that, saves.
        cli.roms.insert(2, rom(2, "Two"));
        cli.saves.insert(save_key(1, "x"), synced("cli"));
        cli.save(&p).unwrap();
        app.saves.insert(save_key(9, "y"), synced("app"));
        app.save(&p).unwrap();

        let on_disk = State::load(&p).unwrap();
        assert!(on_disk.roms.contains_key(&2), "the CLI's install must survive the app's save");
        assert_eq!(on_disk.saves[&save_key(1, "x")].last_hash.as_deref(), Some("cli"));
        assert_eq!(on_disk.saves[&save_key(9, "y")].last_hash.as_deref(), Some("app"));
        // And the app now knows about it too, without having to be restarted.
        assert!(app.roms.contains_key(&2));
    }

    #[test]
    fn an_uninstall_by_one_program_is_not_undone_by_the_other_saving_something_else() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("state.json");
        let (mut app, mut cli) = two_programs(&p, |s| {
            s.roms.insert(1, rom(1, "One"));
            s.roms.insert(2, rom(2, "Two"));
        });
        app.roms.remove(&1);
        app.save(&p).unwrap();
        cli.saves.insert(save_key(2, "x"), synced("h"));
        cli.save(&p).unwrap();
        let on_disk = State::load(&p).unwrap();
        assert!(!on_disk.roms.contains_key(&1), "game 1 was uninstalled and stays so");
        assert!(on_disk.roms.contains_key(&2) && on_disk.saves.contains_key(&save_key(2, "x")));
    }

    #[test]
    fn a_record_both_programs_changed_goes_to_the_later_save_whole() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("state.json");
        let (mut app, mut cli) = two_programs(&p, |s| {
            s.saves.insert(save_key(1, "x"), SaveRecord { last_hash: Some("0".into()), server_save_id: Some(5), ..Default::default() });
        });
        cli.saves.get_mut(&save_key(1, "x")).unwrap().last_hash = Some("cli".into());
        cli.save(&p).unwrap();
        let record = app.saves.get_mut(&save_key(1, "x")).unwrap();
        record.last_hash = Some("app".into());
        record.server_save_id = Some(6);
        app.save(&p).unwrap();
        let r = &State::load(&p).unwrap().saves[&save_key(1, "x")];
        assert_eq!((r.last_hash.as_deref(), r.server_save_id), (Some("app"), Some(6)));
    }

    #[test]
    fn saving_over_a_missing_or_unreadable_file_just_writes_this_copy() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("d/state.json");
        let mut s = State::default();
        s.roms.insert(1, rom(1, "One"));
        s.save(&p).unwrap();
        assert!(State::load(&p).unwrap().roms.contains_key(&1));
        std::fs::write(&p, b"{ not json").unwrap();
        s.roms.insert(2, rom(2, "Two"));
        s.save(&p).unwrap();
        assert_eq!(State::load(&p).unwrap().roms.len(), 2, "a damaged file is replaced, as it always was");
    }

    #[test]
    fn many_programs_saving_at_once_lose_nothing() {
        // Every writer has read the (empty) file before any of them saves: the lost-update case.
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("state.json");
        State::default().save(&p).unwrap();
        let n = 8;
        let ready = std::sync::Arc::new(std::sync::Barrier::new(n));
        let handles: Vec<_> = (0..n as i64)
            .map(|i| {
                let (p, ready) = (p.clone(), ready.clone());
                std::thread::spawn(move || {
                    let mut mine = State::load(&p).unwrap();
                    ready.wait();
                    mine.roms.insert(i, rom(i, "g"));
                    mine.saves.insert(save_key(i, "x"), synced("h"));
                    mine.save(&p).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let on_disk = State::load(&p).unwrap();
        assert_eq!(on_disk.roms.len(), n, "every writer's install is in the file");
        assert_eq!(on_disk.saves.len(), n);
        let leftovers: Vec<_> = std::fs::read_dir(t.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".romm-tmp")).collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_fork_hands_back_only_what_it_changed() {
        let mut live = State::default();
        live.roms.insert(1, rom(1, "One"));
        live.roms.insert(2, rom(2, "Two"));
        let mut work = live.fork();
        // Meanwhile the live state changes (an install finishes, a game is uninstalled)...
        live.roms.insert(3, rom(3, "Three"));
        live.roms.remove(&2);
        // ...and the slow work records a sync for game 1 and attributes some state files.
        work.state.saves.insert(save_key(1, "x"), synced("synced"));
        work.state.set_sigil_state("route", &[1, 2, 3]);
        live.join(work);
        assert_eq!(live.saves[&save_key(1, "x")].last_hash.as_deref(), Some("synced"));
        assert_eq!(live.sigil_state("route"), Some(vec![1, 2, 3]));
        assert!(live.roms.contains_key(&3), "the install that finished meanwhile is kept");
        assert!(!live.roms.contains_key(&2), "the uninstall that happened meanwhile is kept");
        assert!(live.roms.contains_key(&1));
    }

    #[test]
    fn a_fork_that_changed_nothing_changes_nothing() {
        let mut live = State::default();
        live.roms.insert(1, rom(1, "One"));
        let work = live.fork();
        live.roms.insert(2, rom(2, "Two"));
        live.join(work);
        assert_eq!(live.roms.len(), 2);
    }

    #[test]
    fn a_forks_own_save_does_not_disturb_the_state_it_came_from() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("state.json");
        let mut live = State::load(&p).unwrap();
        live.roms.insert(1, rom(1, "One"));
        live.save(&p).unwrap();
        let mut work = live.fork();
        work.state.saves.insert(save_key(1, "x"), synced("h"));
        work.state.save(&p).unwrap(); // what session::pre_play does
        live.roms.insert(2, rom(2, "Two")); // not saved yet
        live.join(work);
        live.save(&p).unwrap();
        let on_disk = State::load(&p).unwrap();
        assert!(on_disk.roms.contains_key(&1) && on_disk.roms.contains_key(&2));
        assert_eq!(on_disk.saves[&save_key(1, "x")].last_hash.as_deref(), Some("h"));
    }

    #[test]
    fn a_state_file_from_before_hardcore_saves_loads_as_casual() {
        let old = r#"{"saves":{"7:snes9x":{"last_hash":"abc","server_save_id":11,"server_updated_at":"2026-01-02T03:04:05Z","dirty":false}}}"#;
        let st: State = serde_json::from_str(old).unwrap();
        let rec = &st.saves["7:snes9x"];
        assert_eq!(rec.last_hash.as_deref(), Some("abc"));
        assert!(!rec.hardcore && !rec.pending_hardcore);
    }

    #[test]
    fn a_finished_exchange_clears_what_a_hardcore_session_left_pending() {
        let mut rec = SaveRecord { pending_hardcore: true, ..Default::default() };
        rec.mark_synced("h", Some(1), None);
        assert!(!rec.pending_hardcore);
    }

    // -- how hardcore is remembered across the two sync engines -------------------------------------

    fn with_channel(st: &mut State, held: bool, pending: bool) {
        st.channels.insert(channel_key(7), ChannelRecord { channel_id: "c".into(), rom_file_id: 1, held_hardcore: held, pending_hardcore: pending, ..Default::default() });
    }

    #[test]
    fn nothing_is_remembered_of_a_game_never_synced() {
        assert_eq!(State::default().hardcore_memory(7, "snes9x"), (false, false));
    }

    #[test]
    fn an_install_that_only_used_the_older_engine_answers_from_its_record() {
        let mut st = State::default();
        *st.save_record(7, "snes9x") = SaveRecord { hardcore: true, pending_hardcore: true, ..Default::default() };
        assert_eq!(st.hardcore_memory(7, "snes9x"), (true, true));
        assert_eq!(st.hardcore_memory(7, "other-core"), (false, false), "another emulator's save is another record");
    }

    #[test]
    fn the_channel_record_outranks_the_older_one_once_there_is_one() {
        let mut st = State::default();
        st.save_record(7, "snes9x").hardcore = true;
        with_channel(&mut st, false, false);
        assert_eq!(st.hardcore_memory(7, "snes9x"), (false, false), "it is the newer of the two, and says casual");
    }

    #[test]
    fn an_exchange_tells_both_records() {
        let mut st = State::default();
        st.save_record(7, "snes9x").pending_hardcore = true;
        with_channel(&mut st, false, true);
        st.note_exchanged(7, "snes9x", true);
        assert_eq!(st.hardcore_memory(7, "snes9x"), (true, false));
        let r = &st.saves[&save_key(7, "snes9x")];
        assert!(r.hardcore && !r.pending_hardcore, "the older record agrees");

        st.note_exchanged(7, "snes9x", false);
        assert_eq!(st.hardcore_memory(7, "snes9x"), (false, false));
        assert!(!st.saves[&save_key(7, "snes9x")].hardcore, "and a later casual exchange is not hidden by an earlier hardcore one");
    }

    #[test]
    fn a_casual_exchange_makes_no_record_that_has_nothing_to_say() {
        let mut st = State::default();
        st.note_exchanged(7, "snes9x", false);
        assert!(st.saves.is_empty() && st.channels.is_empty());
        // The channel record is never made here (it needs the channel's id), only told.
        st.note_exchanged(7, "snes9x", true);
        assert_eq!(st.saves.len(), 1);
        assert!(st.channels.is_empty());
    }

    #[test]
    fn pending_hardcore_is_told_to_both_and_a_casual_session_clears_it() {
        let mut st = State::default();
        with_channel(&mut st, false, false);
        st.note_pending(7, "snes9x", true);
        assert!(st.saves[&save_key(7, "snes9x")].pending_hardcore && st.channels[&channel_key(7)].pending_hardcore);
        st.note_pending(7, "snes9x", false);
        assert!(!st.saves[&save_key(7, "snes9x")].pending_hardcore && !st.channels[&channel_key(7)].pending_hardcore);

        let mut casual_only = State::default();
        casual_only.note_pending(7, "snes9x", false);
        assert!(casual_only.saves.is_empty(), "a casual session adds nothing to remember");
    }
}
