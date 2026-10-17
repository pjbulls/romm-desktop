//! Save-state sync.
//!
//! RomM's device-sync protocol covers saves only. States are plain files: one RomM
//! entity per file, no slots, no per-device baseline and no content hash in the API.
//! So this module keeps its own baseline per state file (what it looked like at the
//! last successful sync) and decides from three facts: local changed? server changed?
//! both?
//!
//! * "server changed" = the server's `updated_at` or size differs from the baseline;
//! * "local changed"  = the local MD5 differs from the baseline (stat-cached so big
//!   unchanged files aren't re-hashed);
//! * a file seen on both sides with no baseline can't be compared without a hash, so
//!   it is downloaded once to a temp file and checked ("verify").
//!
//! Nothing is ever overwritten without a local backup, and deletions never propagate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;

use crate::api::types::StateInfo;
use crate::api::RommClient;
use crate::error::{Error, Result};
use crate::profiles::SaveMode;
use crate::retroarch;
use crate::saves::{backup_file, md5_file, mtime_ns};
use crate::state::{save_key, state_key, LocalRom, State, StateRecord};
use crate::sync::{ConflictPolicy, EmuEnv};

/// Archived copies kept during conflict resolution carry this marker and are never synced back.
pub const CONFLICT_MARK: &str = ".conflict-";

#[derive(Debug, Clone)]
pub struct StateOptions {
    /// Larger states are skipped (neither uploaded nor downloaded).
    pub max_bytes: u64,
}

impl Default for StateOptions {
    fn default() -> Self {
        StateOptions { max_bytes: 512 * 1024 * 1024 }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct StateReport {
    pub uploaded: Vec<String>,
    pub downloaded: Vec<String>,
    /// Local copies preserved on the server as `<name>.conflict-<time>`.
    pub archived: Vec<String>,
    pub conflicts: Vec<String>,
    /// Too large for `max_bytes`.
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
    pub unchanged: usize,
}

impl StateReport {
    pub fn is_empty(&self) -> bool {
        self.uploaded.is_empty()
            && self.downloaded.is_empty()
            && self.archived.is_empty()
            && self.conflicts.is_empty()
            && self.skipped.is_empty()
            && self.failed.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    Unchanged,
    /// Local only → create on the server.
    Upload,
    /// Both exist, only local changed → replace the server's.
    Update,
    /// Server has something we should take.
    Download,
    /// Both exist, never compared: fetch the server's and compare hashes.
    Verify,
    Conflict,
    /// The server no longer has it and we haven't touched it: drop the baseline, don't re-upload.
    Forget,
}

/// The decision table. Pure so it can be tested exhaustively.
pub fn plan(local_hash: Option<&str>, server: Option<&StateInfo>, base: Option<&StateRecord>) -> Plan {
    let server_changed =
        |s: &StateInfo, b: &StateRecord| s.updated_at != b.server_updated_at || s.file_size_bytes != b.server_size;
    match (local_hash, server, base) {
        (None, None, _) => Plan::Unchanged,
        (Some(_), None, None) => Plan::Upload,
        (Some(l), None, Some(b)) => {
            if l == b.local_hash {
                Plan::Forget
            } else {
                Plan::Upload
            }
        }
        (None, Some(_), None) => Plan::Download,
        // Deleted locally on purpose: only bring it back if the server's copy changed.
        (None, Some(s), Some(b)) => {
            if server_changed(s, b) {
                Plan::Download
            } else {
                Plan::Unchanged
            }
        }
        (Some(_), Some(_), None) => Plan::Verify,
        (Some(l), Some(s), Some(b)) => match (l != b.local_hash, server_changed(s, b)) {
            (false, false) => Plan::Unchanged,
            (true, false) => Plan::Update,
            (false, true) => Plan::Download,
            (true, true) => Plan::Conflict,
        },
    }
}

// --------------------------------------------------------------------- locating files

impl EmuEnv {
    /// Folder holding this emulator's state files.
    pub fn states_dir(&self) -> Option<PathBuf> {
        let rule = self.profile.states.as_ref()?;
        match rule.mode {
            SaveMode::Stem => self.retroarch.as_ref().map(|l| l.states.clone()),
            SaveMode::Diff => rule.dir.as_ref().map(|d| self.data_root.join(d)),
        }
    }

    /// Folders (relative to the data root) to watch for state changes during a session.
    pub fn state_watch_roots(&self) -> Vec<String> {
        match &self.profile.states {
            Some(r) if r.mode == SaveMode::Diff => r.dir.clone().into_iter().collect(),
            _ => vec![],
        }
    }

    pub fn state_ignored(&self, name: &str) -> bool {
        let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        self.profile.states.as_ref().is_some_and(|r| r.ignore_extensions.iter().any(|i| i.eq_ignore_ascii_case(&ext)))
    }

    /// Where a state would be written if we don't already have it.
    fn state_destination(&self, name: &str) -> Result<PathBuf> {
        if let Some(l) = &self.retroarch {
            if l.sort_states_by_core || l.sort_states_by_content {
                return Err(Error::msg(
                    "RetroArch sorts save states into subfolders, so a new state can't be placed correctly. \
                     Turn off Settings → Directory → Sort Save States into Folders, or play the game once to create it",
                ));
            }
        }
        let dir = self.states_dir().ok_or_else(|| Error::msg("this emulator has no save-state folder"))?;
        Ok(dir.join(name))
    }
}

/// State files that exist locally for this game: name → path.
pub fn local_states(env: &EmuEnv, rom: &LocalRom, db: &State) -> BTreeMap<String, PathBuf> {
    let Some(rule) = env.profile.states.as_ref() else { return BTreeMap::new() };
    match rule.mode {
        SaveMode::Stem => env
            .retroarch
            .as_ref()
            .map(|l| retroarch::find_state_files(l, &rom.stem).into_iter().collect())
            .unwrap_or_default(),
        SaveMode::Diff => {
            let (Some(dir), Some(names)) = (env.states_dir(), db.state_names.get(&save_key(rom.rom_id, &env.profile.id)))
            else {
                return BTreeMap::new();
            };
            names.iter().filter(|n| dir.join(n).is_file()).map(|n| (n.clone(), dir.join(n))).collect()
        }
    }
}

struct Hashed {
    hash: String,
    size: u64,
    mtime_ns: u128,
}

/// MD5 of a local file, reusing the baseline's hash when size and mtime are unchanged.
fn hash_local(path: &Path, base: Option<&StateRecord>) -> Result<Hashed> {
    let meta = std::fs::metadata(path)?;
    let (size, mt) = (meta.len(), mtime_ns(meta.modified()?));
    if let Some(b) = base {
        if !b.local_hash.is_empty() && b.local_size == size && b.local_mtime_ns == mt {
            return Ok(Hashed { hash: b.local_hash.clone(), size, mtime_ns: mt });
        }
    }
    Ok(Hashed { hash: md5_file(path)?, size, mtime_ns: mt })
}

// -------------------------------------------------------------------------- the sync

struct Ctx<'a> {
    client: &'a RommClient,
    backup_dir: &'a Path,
    rom: &'a LocalRom,
    env: &'a EmuEnv,
    policy: ConflictPolicy,
    opts: &'a StateOptions,
    emu: String,
}

pub async fn sync_states(
    client: &RommClient,
    db: &mut State,
    backup_dir: &Path,
    rom: &LocalRom,
    env: &EmuEnv,
    policy: ConflictPolicy,
    opts: &StateOptions,
) -> Result<StateReport> {
    let mut report = StateReport::default();
    if env.profile.states.is_none() {
        return Ok(report);
    }
    let emu = env.profile.id.clone();
    let server: BTreeMap<String, StateInfo> = client
        .list_states(rom.rom_id)
        .await?
        .into_iter()
        .filter(|s| s.emulator.as_deref() == Some(emu.as_str()) && !s.missing_from_fs && !s.file_name.contains(CONFLICT_MARK))
        .map(|s| (s.file_name.clone(), s))
        .collect();
    let locals = local_states(env, rom, db);
    let names: BTreeSet<String> = server.keys().chain(locals.keys()).cloned().collect();

    let ctx = Ctx { client, backup_dir, rom, env, policy, opts, emu };
    for name in names {
        if let Err(e) = sync_one(&ctx, db, &name, locals.get(&name), server.get(&name), &mut report).await {
            report.failed.push(format!("{name}: {e}"));
        }
    }
    Ok(report)
}

async fn sync_one(
    ctx: &Ctx<'_>,
    db: &mut State,
    name: &str,
    local: Option<&PathBuf>,
    server: Option<&StateInfo>,
    report: &mut StateReport,
) -> Result<()> {
    let key = state_key(ctx.rom.rom_id, &ctx.emu, name);
    let base = db.states.get(&key).cloned();
    let hashed = local.map(|p| hash_local(p, base.as_ref())).transpose()?;
    let p = plan(hashed.as_ref().map(|h| h.hash.as_str()), server, base.as_ref());

    match p {
        Plan::Unchanged => report.unchanged += 1,
        Plan::Forget => {
            db.states.remove(&key);
            report.unchanged += 1;
        }
        Plan::Upload | Plan::Update => {
            let (path, h) = (local.expect("plan implies local"), hashed.as_ref().expect("plan implies local"));
            let existing = if p == Plan::Update { server.map(|s| s.id) } else { None };
            upload(ctx, db, name, path, h, existing, None, report).await?;
        }
        Plan::Download => {
            let s = server.expect("plan implies server");
            adopt_server(ctx, db, name, s, local, None, report).await?;
        }
        Plan::Verify => {
            let (path, h, s) = (local.expect("local"), hashed.as_ref().expect("local"), server.expect("server"));
            if s.file_size_bytes > ctx.opts.max_bytes {
                report.skipped.push(name.to_string());
                return Ok(());
            }
            let tmp = temp_path(path);
            ctx.client.download_state(s.id, &tmp, &mut |_, _| {}).await?;
            if md5_file(&tmp)? == h.hash {
                let _ = std::fs::remove_file(&tmp);
                record(db, &key, s, h);
                report.unchanged += 1;
            } else {
                resolve_conflict(ctx, db, name, path, h, s, Some(tmp), report).await?;
            }
        }
        Plan::Conflict => {
            let (path, h, s) = (local.expect("local"), hashed.as_ref().expect("local"), server.expect("server"));
            resolve_conflict(ctx, db, name, path, h, s, None, report).await?;
        }
    }
    Ok(())
}

fn temp_path(target: &Path) -> PathBuf {
    let mut t = target.as_os_str().to_owned();
    t.push(".romm-dl");
    PathBuf::from(t)
}

fn record(db: &mut State, key: &str, s: &StateInfo, h: &Hashed) {
    db.states.insert(
        key.to_string(),
        StateRecord {
            server_id: Some(s.id),
            local_hash: h.hash.clone(),
            server_updated_at: s.updated_at.clone(),
            server_size: s.file_size_bytes,
            local_size: h.size,
            local_mtime_ns: h.mtime_ns,
        },
    );
}

/// Upload `path`. With `as_name` set the copy is an archive: stored under that name and not baselined.
#[allow(clippy::too_many_arguments)]
async fn upload(
    ctx: &Ctx<'_>,
    db: &mut State,
    name: &str,
    path: &Path,
    h: &Hashed,
    existing_id: Option<i64>,
    as_name: Option<&str>,
    report: &mut StateReport,
) -> Result<()> {
    if h.size > ctx.opts.max_bytes {
        report.skipped.push(name.to_string());
        return Ok(());
    }
    // RetroArch writes <state>.png thumbnails; RomM can show them.
    let png_path = {
        let mut p = path.as_os_str().to_owned();
        p.push(".png");
        PathBuf::from(p)
    };
    let png = png_path.exists().then_some(png_path);
    let upload_name = as_name.unwrap_or(name);
    let info = ctx
        .client
        .upload_state(ctx.rom.rom_id, &ctx.emu, existing_id, upload_name, path, png.as_deref())
        .await?;
    match as_name {
        Some(archive) => report.archived.push(archive.to_string()),
        None => {
            record(db, &state_key(ctx.rom.rom_id, &ctx.emu, name), &info, h);
            report.uploaded.push(name.to_string());
        }
    }
    Ok(())
}

/// Replace/create the local file with the server's. The old local file is backed up first.
async fn adopt_server(
    ctx: &Ctx<'_>,
    db: &mut State,
    name: &str,
    s: &StateInfo,
    local: Option<&PathBuf>,
    already_downloaded: Option<PathBuf>,
    report: &mut StateReport,
) -> Result<()> {
    let target = match local {
        Some(p) => p.clone(),
        None => ctx.env.state_destination(name)?,
    };
    if already_downloaded.is_none() && s.file_size_bytes > ctx.opts.max_bytes {
        report.skipped.push(name.to_string());
        return Ok(());
    }
    let tmp = match already_downloaded {
        Some(t) => t,
        None => {
            let t = temp_path(&target);
            ctx.client.download_state(s.id, &t, &mut |_, _| {}).await?;
            t
        }
    };
    if target.is_file() {
        let stamp = Utc::now().format("%Y%m%dT%H%M%S").to_string();
        let dir = ctx.backup_dir.join(stamp).join(&ctx.emu).join("states");
        backup_file(&target, &dir, name)?;
    }
    std::fs::rename(&tmp, &target)?;
    let h = hash_local(&target, None)?;
    record(db, &state_key(ctx.rom.rom_id, &ctx.emu, name), s, &h);
    if ctx.env.save_mode_for_states() == Some(SaveMode::Diff) {
        db.attribute_states(ctx.rom.rom_id, &ctx.emu, &[name.to_string()]);
    }
    report.downloaded.push(name.to_string());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn resolve_conflict(
    ctx: &Ctx<'_>,
    db: &mut State,
    name: &str,
    path: &Path,
    h: &Hashed,
    s: &StateInfo,
    downloaded: Option<PathBuf>,
    report: &mut StateReport,
) -> Result<()> {
    let local = path.to_path_buf();
    let discard = |t: &Option<PathBuf>| {
        if let Some(t) = t {
            let _ = std::fs::remove_file(t);
        }
    };
    match ctx.policy {
        ConflictPolicy::Ask => {
            discard(&downloaded);
            report.conflicts.push(name.to_string());
        }
        ConflictPolicy::ServerWins => adopt_server(ctx, db, name, s, Some(&local), downloaded, report).await?,
        ConflictPolicy::DeviceWins => {
            discard(&downloaded);
            upload(ctx, db, name, path, h, Some(s.id), None, report).await?;
        }
        ConflictPolicy::KeepBoth => {
            let archive = format!("{name}{CONFLICT_MARK}{}", Utc::now().format("%Y%m%dT%H%M%S"));
            if let Err(e) = upload(ctx, db, name, path, h, None, Some(&archive), report).await {
                discard(&downloaded);
                return Err(e);
            }
            if report.skipped.last().map(String::as_str) == Some(name) {
                // Too big to archive on the server: don't discard the only copy by adopting the server's.
                discard(&downloaded);
                return Ok(());
            }
            adopt_server(ctx, db, name, s, Some(&local), downloaded, report).await?;
        }
    }
    Ok(())
}

impl EmuEnv {
    fn save_mode_for_states(&self) -> Option<SaveMode> {
        self.profile.states.as_ref().map(|r| r.mode)
    }
}

// ------------------------------------------------------------------------ for the UI

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateStatus {
    InSync,
    LocalNewer,
    ServerNewer,
    LocalOnly,
    ServerOnly,
    Conflict,
    /// Present on both sides but never compared (will be checked on the next sync).
    Unverified,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateView {
    pub name: String,
    pub local_size: Option<u64>,
    pub local_modified: Option<String>,
    pub server_size: Option<u64>,
    pub server_updated_at: Option<String>,
    pub status: StateStatus,
}

/// What a sync would do, without doing it. `client = None` shows local files only (offline).
pub async fn overview(client: Option<&RommClient>, db: &State, rom: &LocalRom, env: &EmuEnv) -> Result<Vec<StateView>> {
    if env.profile.states.is_none() {
        return Ok(vec![]);
    }
    let emu = env.profile.id.clone();
    let server: BTreeMap<String, StateInfo> = match client {
        Some(c) => c
            .list_states(rom.rom_id)
            .await?
            .into_iter()
            .filter(|s| s.emulator.as_deref() == Some(emu.as_str()) && !s.missing_from_fs && !s.file_name.contains(CONFLICT_MARK))
            .map(|s| (s.file_name.clone(), s))
            .collect(),
        None => BTreeMap::new(),
    };
    let locals = local_states(env, rom, db);
    let names: BTreeSet<String> = server.keys().chain(locals.keys()).cloned().collect();
    let mut out = Vec::new();
    for name in names {
        let base = db.states.get(&state_key(rom.rom_id, &emu, &name));
        let local = locals.get(&name);
        let hashed = local.map(|p| hash_local(p, base)).transpose()?;
        let srv = server.get(&name);
        let status = match (local.is_some(), client.is_some() && srv.is_some()) {
            (true, false) => StateStatus::LocalOnly,
            (false, _) => StateStatus::ServerOnly,
            (true, true) => match plan(hashed.as_ref().map(|h| h.hash.as_str()), srv, base) {
                Plan::Unchanged | Plan::Forget => StateStatus::InSync,
                Plan::Update | Plan::Upload => StateStatus::LocalNewer,
                Plan::Download => StateStatus::ServerNewer,
                Plan::Verify => StateStatus::Unverified,
                Plan::Conflict => StateStatus::Conflict,
            },
        };
        let local_modified = local
            .and_then(|p| std::fs::metadata(p).ok()?.modified().ok())
            .map(crate::saves::iso);
        out.push(StateView {
            name,
            local_size: hashed.as_ref().map(|h| h.size),
            local_modified,
            server_size: srv.map(|s| s.file_size_bytes),
            server_updated_at: srv.map(|s| s.updated_at.clone()),
            status,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(updated: &str, size: u64) -> StateInfo {
        StateInfo { id: 1, rom_id: 7, file_name: "x".into(), file_size_bytes: size, emulator: Some("e".into()), updated_at: updated.into(), missing_from_fs: false }
    }
    fn base(hash: &str, updated: &str, size: u64) -> StateRecord {
        StateRecord { server_id: Some(1), local_hash: hash.into(), server_updated_at: updated.into(), server_size: size, ..Default::default() }
    }

    #[test]
    fn decision_table() {
        let s = srv("t1", 10);
        let b = base("h1", "t1", 10);
        // one side only
        assert_eq!(plan(Some("h1"), None, None), Plan::Upload);
        assert_eq!(plan(None, Some(&s), None), Plan::Download);
        // never compared
        assert_eq!(plan(Some("h1"), Some(&s), None), Plan::Verify);
        // baseline present
        assert_eq!(plan(Some("h1"), Some(&s), Some(&b)), Plan::Unchanged);
        assert_eq!(plan(Some("h2"), Some(&s), Some(&b)), Plan::Update, "only local changed");
        assert_eq!(plan(Some("h1"), Some(&srv("t2", 10)), Some(&b)), Plan::Download, "server updated_at moved");
        assert_eq!(plan(Some("h1"), Some(&srv("t1", 11)), Some(&b)), Plan::Download, "server size moved");
        assert_eq!(plan(Some("h2"), Some(&srv("t2", 10)), Some(&b)), Plan::Conflict, "both changed");
        // deleted locally on purpose: stays deleted unless the server's changed
        assert_eq!(plan(None, Some(&s), Some(&b)), Plan::Unchanged);
        assert_eq!(plan(None, Some(&srv("t2", 10)), Some(&b)), Plan::Download);
        // deleted on the server: not resurrected unless we changed it
        assert_eq!(plan(Some("h1"), None, Some(&b)), Plan::Forget);
        assert_eq!(plan(Some("h2"), None, Some(&b)), Plan::Upload);
        assert_eq!(plan(None, None, Some(&b)), Plan::Unchanged);
    }

    #[test]
    fn stat_cache_skips_rehash_only_when_unchanged() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("a.state");
        std::fs::write(&p, "hello").unwrap();
        let first = hash_local(&p, None).unwrap();
        let b = StateRecord { local_hash: "CACHED".into(), local_size: first.size, local_mtime_ns: first.mtime_ns, ..Default::default() };
        assert_eq!(hash_local(&p, Some(&b)).unwrap().hash, "CACHED", "unchanged stat reuses the cached hash");
        std::fs::write(&p, "hello!!").unwrap();
        assert_ne!(hash_local(&p, Some(&b)).unwrap().hash, "CACHED", "changed file is rehashed");
    }
}
