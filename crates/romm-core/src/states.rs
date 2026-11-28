//! Save-state sync.
//!
//! RomM's device-sync protocol covers saves only. States are plain files: one RomM
//! entity per file, no slots and no per-device baseline. So this module keeps its own
//! baseline per state file (what it looked like at the last successful sync) and
//! decides from three facts: local changed? server changed? both?
//!
//! * "server changed" = the server's `content_hash` differs from the baseline's, when RomM
//!   gives one (5.5+, for a state it has hashed), else its `updated_at` or size does. The
//!   hash comes first because `updated_at` moves without a change: a RomM up to 5.5.0-beta.1
//!   bumps it on every state a snapshot prune keeps (rommapp/romm#5718);
//! * "local changed"  = the local MD5 differs from the baseline (stat-cached so big
//!   unchanged files aren't re-hashed);
//! * a file seen on both sides with no baseline can't be compared without a hash, so
//!   it is downloaded once to a temp file and checked ("verify").
//!
//! Where a state is taken from the server the bytes are compared first too, because for a state
//! with no hash only they can say that "changed" was not. Nothing is ever overwritten without
//! a local backup, and deletions never propagate.
//!
//! RomM 5.5 adds a second owner for a state: a snapshot, which is how Argosy 3.0 pushes states. A state a
//! sync channel holds can be read but never rewritten directly (RomM answers 409), so such a state is left as
//! the server has it and reported as `managed`, not as a failure. Putting this app's states into snapshots
//! too is what would make them travel between the two; until then they stay in the plain `/api/states` store.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::api::types::StateInfo;
use crate::api::RommClient;
use crate::error::{Error, Result};
use crate::profiles::SaveMode;
use crate::report::{PendingConflict, TransferReport};
use crate::retroarch;
use crate::saves::{self, backup_file, conflict_name, md5_file, mtime_ns, CONFLICT_MARK};
use crate::state::{save_key, state_key, LocalRom, State, StateRecord};
use crate::sync::{ConflictPolicy, EmuEnv};

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

/// What syncing a game's save states did (`skipped` lists the ones too large for `max_bytes`).
pub type StateReport = TransferReport;

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

/// Does the server's `content_hash` say its copy is the file with this hash? A match proves it. A mismatch, or
/// no hash, proves nothing (see `StateInfo::content_hash`).
fn server_holds(s: &StateInfo, hash: &str) -> bool {
    s.content_hash.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(hash))
}

/// Did the server's copy change since the baseline `b`?
///
/// By hash when RomM gives one: against the hash it gave then, or else against the MD5 of what was last synced,
/// which is the same bytes whichever side they came from. `updated_at` alone would call a state changed whenever
/// a RomM up to 5.5.0-beta.1 pruned a snapshot: an untouched state would be downloaded again, an edited one
/// would be a conflict, and one deleted on purpose would come back. With no hash it is `updated_at` and the
/// size, as it was before RomM had one.
fn server_changed(s: &StateInfo, b: &StateRecord) -> bool {
    match s.content_hash.as_deref() {
        Some(now) if !b.server_hash.is_empty() => !now.eq_ignore_ascii_case(&b.server_hash),
        Some(now) if now.eq_ignore_ascii_case(&b.local_hash) => false,
        _ => s.updated_at != b.server_updated_at || s.file_size_bytes != b.server_size,
    }
}

/// The decision table. Pure so it can be tested exhaustively.
pub fn plan(local_hash: Option<&str>, server: Option<&StateInfo>, base: Option<&StateRecord>) -> Plan {
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

/// Is this RomM's refusal to write a state that a sync channel holds? It is a 409 that says so in words, and the
/// only 409 `POST /api/states` and `PUT /api/states/{id}` have. The `channel_id` a listing carries predicts it,
/// but only RomM knows which snapshots hold what, so a write that is refused all the same is taken the same way.
fn refused_by_channel(e: &Error) -> bool {
    matches!(e, Error::Status { status: 409, body, .. } if body.to_ascii_lowercase().contains("sync channel"))
}

// --------------------------------------------------------------------- locating files

impl EmuEnv {
    /// Folder holding this emulator's state files.
    pub fn states_dir(&self) -> Option<PathBuf> {
        let rule = self.profile.states.as_ref()?;
        match rule.mode {
            SaveMode::Stem => self.retroarch.as_ref().map(|l| l.states.clone()),
            SaveMode::Diff => rule.dir.as_ref().map(|d| self.data_root.join(d)),
            // Save-only modes: no emulator has a state rule like them.
            SaveMode::Hdd | SaveMode::Folder => None,
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
        SaveMode::Hdd | SaveMode::Folder => BTreeMap::new(),
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

/// `emu`'s states for `rom` as the server lists them, by file name. What a conflict archived
/// (`CONFLICT_MARK`) and what the server no longer has on disk are not states to sync.
async fn server_states(client: &RommClient, rom: &LocalRom, emu: &str) -> Result<BTreeMap<String, StateInfo>> {
    Ok(client
        .list_states(rom.rom_id)
        .await?
        .into_iter()
        .filter(|s| s.emulator.as_deref() == Some(emu) && !s.missing_from_fs && !s.file_name.contains(CONFLICT_MARK))
        .map(|s| (s.file_name.clone(), s))
        .collect())
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
    let server = server_states(client, rom, &emu).await?;
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
    // A state's name comes from the server and becomes a path under the emulator's states folder
    // (`state_destination` joins it on, so `..` or an absolute name would leave that folder).
    // ROM, firmware and bundle names are checked the same way.
    if saves::safe_component(name).is_none() {
        return Err(Error::msg(format!("the server gave an unsafe state file name ({name:?}); it was ignored")));
    }
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
            if p == Plan::Update && server.is_some_and(StateInfo::is_channel_managed) {
                // Only a snapshot changes it; asking would only be refused.
                report.managed.push(name.to_string());
                return Ok(());
            }
            let existing = if p == Plan::Update { server.map(|s| s.id) } else { None };
            upload(ctx, db, name, path, h, existing, None, report).await?;
        }
        Plan::Download => {
            let s = server.expect("plan implies server");
            let here = local.zip(hashed.as_ref()).map(|(p, h)| (p.as_path(), h));
            adopt_server(ctx, db, name, s, here, None, report).await?;
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
            server_hash: s.content_hash.clone().unwrap_or_default(),
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
    let info = match ctx.client.upload_state(ctx.rom.rom_id, &ctx.emu, existing_id, upload_name, path, png.as_deref()).await {
        Ok(info) => info,
        // An archive has a name of its own, which no channel can hold; anything else is a state that one does.
        Err(e) if as_name.is_none() && refused_by_channel(&e) => {
            report.managed.push(name.to_string());
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    match as_name {
        Some(archive) => report.archived.push(archive.to_string()),
        None => {
            record(db, &state_key(ctx.rom.rom_id, &ctx.emu, name), &info, h);
            report.uploaded.push(name.to_string());
        }
    }
    Ok(())
}

/// Replace/create the local file with the server's. The old local file is backed up first, unless the server's
/// copy turns out to be the same bytes: then nothing is replaced, only the baseline brought up to date.
async fn adopt_server(
    ctx: &Ctx<'_>,
    db: &mut State,
    name: &str,
    s: &StateInfo,
    local: Option<(&Path, &Hashed)>,
    already_downloaded: Option<PathBuf>,
    report: &mut StateReport,
) -> Result<()> {
    let target = match local {
        Some((p, _)) => p.to_path_buf(),
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
    // RomM can call a state changed that wasn't (`server_changed`), and for one it has no hash of only the bytes
    // can say so. Replacing the file with itself would still cost a backup and move its mtime for nothing.
    if let Some((_, here)) = local {
        if md5_file(&tmp)? == here.hash {
            let _ = std::fs::remove_file(&tmp);
            record(db, &state_key(ctx.rom.rom_id, &ctx.emu, name), s, here);
            report.unchanged += 1;
            return Ok(());
        }
    }
    if target.is_file() {
        let dir = saves::backup_dir_for(ctx.backup_dir, &ctx.emu).join("states");
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
    let discard = |t: &Option<PathBuf>| {
        if let Some(t) = t {
            let _ = std::fs::remove_file(t);
        }
    };
    if server_holds(s, &h.hash) {
        // Both sides came to the same bytes (or RomM only says the server's changed): nothing to choose between.
        discard(&downloaded);
        record(db, &state_key(ctx.rom.rom_id, &ctx.emu, name), s, h);
        report.unchanged += 1;
        return Ok(());
    }
    match ctx.policy {
        ConflictPolicy::Ask => {
            discard(&downloaded);
            report.conflicts.push(PendingConflict {
                rom_id: ctx.rom.rom_id,
                file_name: name.to_string(),
                server_updated_at: Some(s.updated_at.clone()).filter(|u| !u.is_empty()),
                reason: None,
            });
        }
        ConflictPolicy::ServerWins => adopt_server(ctx, db, name, s, Some((path, h)), downloaded, report).await?,
        ConflictPolicy::DeviceWins => {
            discard(&downloaded);
            if s.is_channel_managed() {
                // This device's copy can't replace one a channel holds, and the local file is the user's, so it
                // stays as it is: neither overwritten by the server's nor lost.
                report.managed.push(name.to_string());
                return Ok(());
            }
            upload(ctx, db, name, path, h, Some(s.id), None, report).await?;
        }
        ConflictPolicy::KeepBoth => {
            let archive = conflict_name(name);
            if let Err(e) = upload(ctx, db, name, path, h, None, Some(&archive), report).await {
                discard(&downloaded);
                return Err(e);
            }
            if report.skipped.last().map(String::as_str) == Some(name) {
                // Too big to archive on the server: don't discard the only copy by adopting the server's.
                discard(&downloaded);
                return Ok(());
            }
            adopt_server(ctx, db, name, s, Some((path, h)), downloaded, report).await?;
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
    /// Changed here, but a RomM sync channel holds the server's copy and can't have it rewritten: a sync
    /// leaves both as they are.
    Managed,
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
    let server = match client {
        Some(c) => server_states(c, rom, &emu).await?,
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
                Plan::Update if srv.is_some_and(StateInfo::is_channel_managed) => StateStatus::Managed,
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

/// `overview`, falling back to the local files alone when the server can't be asked. The second
/// part is why the server wasn't consulted, for the caller to tell the user.
pub async fn overview_or_local(client: Option<&RommClient>, db: &State, rom: &LocalRom, env: &EmuEnv) -> Result<(Vec<StateView>, Option<String>)> {
    match overview(client, db, rom, env).await {
        Ok(views) => Ok((views, None)),
        Err(e) => Ok((overview(None, db, rom, env).await?, Some(e.to_string()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(updated: &str, size: u64) -> StateInfo {
        StateInfo { id: 1, rom_id: 7, file_name: "x".into(), file_size_bytes: size, emulator: Some("e".into()), updated_at: updated.into(), missing_from_fs: false, channel_id: None, content_hash: None }
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

    fn hashed(mut s: StateInfo, hash: &str) -> StateInfo {
        s.content_hash = Some(hash.into());
        s
    }
    fn base_with_hash(local: &str, server_hash: &str) -> StateRecord {
        StateRecord { server_hash: server_hash.into(), ..base(local, "t1", 10) }
    }

    #[test]
    fn a_moved_updated_at_is_not_a_change_when_the_hash_says_so() {
        // RomM up to 5.5.0-beta.1 moves updated_at on every state a snapshot prune keeps.
        let b = base_with_hash("h1", "H1");
        let pruned = hashed(srv("t-prune", 10), "H1");
        assert_eq!(plan(Some("h1"), Some(&pruned), Some(&b)), Plan::Unchanged, "was a download of the same bytes");
        assert_eq!(plan(Some("h2"), Some(&pruned), Some(&b)), Plan::Update, "was a conflict over a change only here");
        assert_eq!(plan(None, Some(&pruned), Some(&b)), Plan::Unchanged, "was a state the user deleted, brought back");
        // The hash is not case-sensitive.
        assert_eq!(plan(Some("h1"), Some(&hashed(srv("t-prune", 10), "h1")), Some(&b)), Plan::Unchanged);
    }

    #[test]
    fn a_different_hash_is_a_change_even_when_updated_at_and_size_are_not() {
        let b = base_with_hash("h1", "H1");
        let edited = hashed(srv("t1", 10), "H2");
        assert_eq!(plan(Some("h1"), Some(&edited), Some(&b)), Plan::Download);
        assert_eq!(plan(Some("h2"), Some(&edited), Some(&b)), Plan::Conflict);
    }

    #[test]
    fn a_baseline_from_before_the_hash_is_judged_by_the_bytes_it_synced() {
        // No server hash was kept then, but the hash RomM gives now can still be the MD5 of what was last synced.
        let b = base("h1", "t1", 10);
        assert_eq!(plan(None, Some(&hashed(srv("t-prune", 10), "h1")), Some(&b)), Plan::Unchanged);
        // When it is not, only updated_at and the size are left, as for a state with no hash at all.
        assert_eq!(plan(Some("h1"), Some(&hashed(srv("t1", 10), "other")), Some(&b)), Plan::Unchanged);
        assert_eq!(plan(Some("h1"), Some(&hashed(srv("t2", 10), "other")), Some(&b)), Plan::Download);
    }

    #[test]
    fn a_state_with_no_hash_is_judged_as_it_always_was() {
        let b = base_with_hash("h1", "H1");
        assert_eq!(plan(Some("h1"), Some(&srv("t1", 10)), Some(&b)), Plan::Unchanged);
        assert_eq!(plan(Some("h1"), Some(&srv("t2", 10)), Some(&b)), Plan::Download);
        assert_eq!(plan(Some("h1"), Some(&srv("t1", 11)), Some(&b)), Plan::Download);
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
