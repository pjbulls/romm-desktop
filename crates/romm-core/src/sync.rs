//! Save sync against RomM's device-sync protocol: negotiate → apply operations → complete the
//! session.
//!
//! What a game's save *is*, and the bytes that travel, come from `route` and `unit` (Sigil, the
//! library Argosy uses), so a save uploaded here is one Argosy restores and vice versa. This module
//! is the protocol around that: what to tell the server, what to do with its answer, and what to
//! remember afterwards.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::api::types::*;
use crate::api::RommClient;
use crate::error::{Error, Result};
use crate::profiles::Profile;
use crate::retroarch;
use crate::route::{self, Kind, Plan};
use crate::xenia;
use crate::saves::{self, SLOT};
use crate::state::{save_key, LocalRom, SaveRecord, State};
use crate::unit::{self, Identity};
use crate::xbox;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Upload the local copy as an archival (slot-less) save, then take the server's.
    /// Nothing is lost on either side. The default.
    #[default]
    KeepBoth,
    /// Take the server's save (local copy is backed up first).
    ServerWins,
    /// Overwrite the server's save with the local one.
    DeviceWins,
    /// Don't resolve; report conflicts to the caller (a UI would prompt).
    Ask,
}

/// Where one emulator keeps its data on this machine.
#[derive(Debug, Clone)]
pub struct EmuEnv {
    pub profile: Profile,
    pub data_root: PathBuf,
    pub retroarch: Option<retroarch::Layout>,
    /// The libretro core in use (RetroArch only), named as RomM and Argosy name it (`snes9x`).
    /// Which files are a game's save, and the `emulator` RomM is told, depend on it.
    pub core: Option<String>,
}

/// The ids Sigil read from the ROM, cached in the state file. Argosy reads them once, when a game is
/// installed; here that happens the first time the game is synced (and again if its launch path
/// changes, which means the ROM did).
pub fn identity_for(state: &mut State, rom: &LocalRom) -> Identity {
    if let Some(id) = cached_identity(state, rom) {
        return id;
    }
    let id = unit::identify(&rom.launch_path, &rom.platform_slug);
    // Only remember a reading that found something; an unreadable ROM is tried again next time.
    let found = !id.title_id.is_empty() || !id.save_id.is_empty() || id.features != 0 || !id.n64_md5.is_empty();
    if found {
        if let Some(r) = state.roms.get_mut(&rom.rom_id) {
            r.identity = Some(id.clone());
        }
    }
    id
}

/// The cached reading of `rom`, if it is still the reading of this launch file on this platform.
fn cached_identity(state: &State, rom: &LocalRom) -> Option<Identity> {
    let id = state.roms.get(&rom.rom_id)?.identity.as_ref()?;
    (id.source == rom.launch_path.to_string_lossy() && id.platform == unit::platform_slug(&rom.platform_slug)).then(|| id.clone())
}

/// What a game's local save is made of.
#[derive(Debug, Clone)]
pub enum LocalKind {
    /// One file, uploaded as it is (RetroArch cores without a Sigil layout).
    Raw(PathBuf),
    /// A unit Sigil collected from the emulator's files.
    Unit(unit::Unit),
    /// Bytes read out of something that is not a file of its own (a folder in xemu's disk image).
    /// Put back with `xbox::import_save`, never by writing a file.
    Bytes(Vec<u8>),
    /// A game's saved-game folder, zipped by `xenia.rs`.
    Folder(xenia::Collected),
}

#[derive(Debug, Clone)]
pub struct LocalUnit {
    /// The `emulator` RomM is told.
    pub emulator: String,
    pub slot: String,
    /// The name it is uploaded under: `<rom stem>.<extension>`, as Argosy names it.
    pub file_name: String,
    pub kind: LocalKind,
    pub size: u64,
    /// When the files holding the save were last written.
    pub modified: SystemTime,
    /// RomM's `content_hash` of the bytes as they would be uploaded.
    pub hash: String,
    /// Differs from what was last exchanged with the server.
    pub changed: bool,
}

impl LocalUnit {
    /// The bytes that would be uploaded. Only a raw save has to be read; the rest are already here.
    fn data(&self) -> Result<Cow<'_, [u8]>> {
        Ok(match &self.kind {
            LocalKind::Raw(p) => Cow::Owned(std::fs::read(p)?),
            LocalKind::Unit(u) => Cow::Borrowed(&u.data),
            LocalKind::Bytes(b) => Cow::Borrowed(b),
            LocalKind::Folder(c) => Cow::Borrowed(&c.data),
        })
    }

    /// The bytes that would be uploaded.
    pub fn bytes(&self) -> Result<Vec<u8>> {
        Ok(self.data()?.into_owned())
    }

    /// Keeps a copy in `dir`, in the form that would have been uploaded, before something replaces it.
    fn backup_to(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(&self.file_name), self.data()?)?;
        Ok(())
    }

    fn detail(&self) -> Option<String> {
        let n = match &self.kind {
            LocalKind::Raw(_) | LocalKind::Bytes(_) => return None,
            LocalKind::Folder(c) => c.files,
            LocalKind::Unit(u) => {
                if u.shape == unit::Shape::Single {
                    return None;
                }
                let mut z = zip::ZipArchive::new(std::io::Cursor::new(&u.data)).ok()?;
                (0..z.len()).filter(|i| z.by_index(*i).is_ok_and(|f| !f.name().ends_with('/'))).count()
            }
        };
        Some(format!("{n} file{}", if n == 1 { "" } else { "s" }))
    }
}

/// Collects `rom`'s local save under `plan`, if it has one. `rec` and `sigil_state` are what the last
/// sync left behind; they decide `changed`.
pub fn gather(rom: &LocalRom, id: &Identity, plan: &Plan, rec: Option<&SaveRecord>, sigil_state: Option<&[u8]>) -> Result<Option<LocalUnit>> {
    let last = rec.and_then(|r| r.last_hash.as_deref());
    match &plan.kind {
        Kind::Raw { path, exists } => {
            if !*exists {
                return Ok(None);
            }
            let meta = std::fs::metadata(path)?;
            let hash = saves::md5_file(path)?;
            let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
            Ok(Some(LocalUnit {
                emulator: plan.label.clone(),
                slot: SLOT.into(),
                file_name: upload_name(&rom.stem, &ext),
                size: meta.len(),
                modified: meta.modified()?,
                changed: last != Some(hash.as_str()),
                hash,
                kind: LocalKind::Raw(path.clone()),
            }))
        }
        Kind::Xbox { image, save_id } => {
            if !saves::is_title_id(save_id) {
                return Err(Error::msg(format!(
                    "can't tell which game \"{}\" is: no Xbox title id could be read from the ROM, so its save can't be found in xemu's disk image. \
                     The ROM needs to be an XISO (or a bare default.xbe)",
                    rom.stem
                )));
            }
            // Another program (xemu) with the image open can be mid-write, and so can what it shows.
            if let Some(why) = xbox::image_in_use(image) {
                return Err(Error::msg(format!(
                    "xemu's hard disk image {} is in use ({why}), so its saves were not read. Close xemu and sync again",
                    image.display()
                )));
            }
            let mut hdd = xbox::FileHdd::open(image)
                .map_err(|e| Error::msg(format!("reading xemu's hard disk image {}: {e}", image.display())))?;
            // Not closed cleanly, or xemu still has it open: what is on it may be half-written, and
            // an upload of that could replace a good save on the server.
            if hdd.device().is_dirty() {
                return Err(Error::msg(format!(
                    "xemu's hard disk image {} is marked as in use (xemu is running, or it did not close cleanly), so its saves were not read. \
                     Close xemu (if it crashed, start it and close it once) and sync again",
                    image.display()
                )));
            }
            let Some(save) = hdd.export_save(save_id)? else { return Ok(None) };
            let data = save.zip;
            let hash = unit::content_hash(&data);
            Ok(Some(LocalUnit {
                emulator: plan.label.clone(),
                slot: SLOT.into(),
                file_name: upload_name(&rom.stem, "zip"),
                size: data.len() as u64,
                // How old the save is: the newest time the console stamped on one of its files. NOT
                // the disk image's own timestamp, which changes whenever xemu runs or the image is
                // copied, and would make an old save in a fresh copy look newer than the server's.
                // Only a disk whose clock holds nothing usable falls back to the image's time.
                modified: match save.modified {
                    Some(t) => t,
                    None => std::fs::metadata(image)?.modified()?,
                },
                changed: last != Some(hash.as_str()),
                hash,
                kind: LocalKind::Bytes(data),
            }))
        }
        Kind::Unit(route) => {
            ensure_identified(route, id, rom)?;
            let Some(u) = unit::collect(route, id, &plan.content_name, sigil_state)? else { return Ok(None) };
            let modified = unit::newest_mtime(route, id, &plan.content_name).unwrap_or_else(SystemTime::now);
            Ok(Some(LocalUnit {
                emulator: plan.label.clone(),
                slot: SLOT.into(),
                file_name: upload_name(&rom.stem, &u.extension()),
                size: u.data.len() as u64,
                modified,
                hash: u.content_hash.clone(),
                changed: u.changed || rec.is_some_and(|r| r.dirty),
                kind: LocalKind::Unit(u),
            }))
        }
        Kind::Folder { content, title_id } => {
            let title_id = ensure_title_id(title_id, &name_base(rom, Some(plan)))?;
            let Some(dir) = xenia::find_save_dir(content, title_id) else { return Ok(None) };
            let Some(c) = xenia::collect(&dir)? else { return Ok(None) };
            Ok(Some(LocalUnit {
                emulator: plan.label.clone(),
                slot: SLOT.into(),
                file_name: upload_name(&name_base(rom, Some(plan)), "zip"),
                size: c.data.len() as u64,
                modified: c.modified,
                changed: last != Some(c.content_hash.as_str()),
                hash: c.content_hash.clone(),
                kind: LocalKind::Folder(c),
            }))
        }
    }
}

/// `gather` with what the last sync left behind in `state`.
fn gather_from_state(state: &State, rom: &LocalRom, id: &Identity, plan: &Plan) -> Result<Option<LocalUnit>> {
    let rec = state.saves.get(&save_key(rom.rom_id, &plan.label));
    let blob = match &plan.kind {
        Kind::Unit(r) => state.sigil_state(&r.state_key()),
        Kind::Raw { .. } | Kind::Xbox { .. } | Kind::Folder { .. } => None,
    };
    gather(rom, id, plan, rec, blob.as_deref())
}

/// The unowned saves that appeared since `previous` was recorded. With nothing recorded there is no
/// "since": whatever is already on the volume is not claimed for the game that happens to run.
fn new_unowned(previous: Option<&[String]>, now: &[String]) -> Vec<String> {
    match previous {
        None => Vec::new(),
        Some(before) => now.iter().filter(|n| !before.contains(n)).cloned().collect(),
    }
}

/// A game whose saves are found by id must have one, or there is no telling which of the
/// emulator's files are its saves. Sigil would only say "invalid argument"; say what to do instead.
fn ensure_identified(route: &unit::Route, id: &Identity, rom: &LocalRom) -> Result<()> {
    if unit::needs_game_id(&route.platform) && !id.has_id() {
        return Err(Error::msg(format!(
            "can't tell which game \"{}\" is: no game id could be read from the ROM or its file name, so its saves can't be located. \
             Naming the file with its id (like \"Daxter [ULUS10064].iso\") is enough",
            rom.stem
        )));
    }
    Ok(())
}

/// An Xbox 360 game's saves are in a folder named by its title id, which Sigil reads from the ROM's
/// `default.xex` (or the same file inside a disc image or `.zar`). A package it can't open (Games on
/// Demand, an STFS container) has none, and there is no file name to fall back on.
fn ensure_title_id<'a>(title_id: &'a str, game: &str) -> Result<&'a str> {
    if title_id.is_empty() {
        return Err(Error::msg(format!(
            "can't tell which Xbox 360 game \"{}\" is: its title id could not be read from the ROM, so its saves can't be located. \
             Xenia's saves are found by title id, which is read from default.xex, an .xex file, a disc image (.iso) or a .zar; \
             a Games on Demand or STFS package can't be identified",
            game
        )));
    }
    Ok(title_id)
}

/// What a game is called in a save's file name and in messages. Normally the launch file's stem (what
/// RetroArch names its saves after). For a ROM extracted into its own folder whose save is kept as a
/// folder it is that folder's name instead: an Xbox 360 disc's launch file is always `default.xex`,
/// which would name every game's save `default.zip`.
fn name_base(rom: &LocalRom, plan: Option<&Plan>) -> String {
    if matches!(plan.map(|p| &p.kind), Some(Kind::Folder { .. })) && rom.path.is_dir() {
        if let Some(n) = rom.path.file_name() {
            return n.to_string_lossy().into_owned();
        }
    }
    rom.stem.clone()
}

/// Argosy uploads as `<rom base name>.<extension of what it uploads>`.
fn upload_name(stem: &str, ext: &str) -> String {
    if ext.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{ext}")
    }
}

/// What to tell the server about a local save. While the save is unchanged since the last transfer
/// the server's own hash and timestamp are reported back (as Argosy does), so a save that was only
/// restored, moved or touched can never look newer than the server's copy.
fn reported(local: &LocalUnit, rec: Option<&SaveRecord>) -> (String, String) {
    if let Some(last) = rec.and_then(|r| r.last_hash.clone()).filter(|_| !local.changed) {
        let when = rec.and_then(|r| r.server_updated_at.clone()).unwrap_or_else(|| saves::iso(local.modified));
        return (last, when);
    }
    (local.hash.clone(), saves::iso(local.modified))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveStatus {
    /// Matches what was last exchanged with the server.
    Synced,
    /// Changed on this machine since the last sync.
    Modified,
    NeverSynced,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SaveView {
    pub name: String,
    pub size: u64,
    pub modified: Option<String>,
    /// e.g. "2 files" for a save made of several
    pub detail: Option<String>,
    pub status: SaveStatus,
}

/// Local view of a game's saves and whether each differs from the last sync. No network.
pub fn saves_overview(env: &EmuEnv, rom: &LocalRom, db: &State) -> Result<Vec<SaveView>> {
    let id = cached_identity(db, rom).unwrap_or_else(|| unit::identify(&rom.launch_path, &rom.platform_slug));
    let Some(plan) = route::plan(env, rom, &id) else { return Ok(vec![]) };
    let Some(local) = gather_from_state(db, rom, &id, &plan)? else { return Ok(vec![]) };
    let status = match db.saves.get(&save_key(rom.rom_id, &plan.label)).and_then(|r| r.last_hash.as_deref()) {
        None => SaveStatus::NeverSynced,
        Some(_) if local.changed => SaveStatus::Modified,
        Some(_) => SaveStatus::Synced,
    };
    Ok(vec![SaveView { detail: local.detail(), name: local.file_name, size: local.size, modified: Some(saves::iso(local.modified)), status }])
}

#[derive(Debug, Clone)]
pub struct PendingConflict {
    pub rom_id: i64,
    pub file_name: String,
    pub server_updated_at: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Default)]
pub struct SyncReport {
    pub uploaded: Vec<String>,
    pub downloaded: Vec<String>,
    /// Local copies preserved as archival saves during conflict resolution.
    pub archived: Vec<String>,
    pub conflicts: Vec<PendingConflict>,
    pub failed: Vec<String>,
    pub unchanged: usize,
}

impl SyncReport {
    pub fn is_clean(&self) -> bool {
        self.failed.is_empty() && self.conflicts.is_empty()
    }

    /// Adds `other` to this report, to total up several games. Every field is named on purpose: a
    /// field added to the report is a compile error here until it is counted, so it can't be left
    /// out of a total.
    pub fn merge(&mut self, other: SyncReport) {
        let SyncReport { uploaded, downloaded, archived, conflicts, failed, unchanged } = other;
        self.uploaded.extend(uploaded);
        self.downloaded.extend(downloaded);
        self.archived.extend(archived);
        self.conflicts.extend(conflicts);
        self.failed.extend(failed);
        self.unchanged += unchanged;
    }
}

pub struct Syncer<'a> {
    pub client: &'a RommClient,
    pub device_id: &'a str,
    pub state: &'a mut State,
    pub backup_dir: PathBuf,
    pub policy: ConflictPolicy,
}

/// Everything an operation needs to know about the game being synced.
struct Cx<'x> {
    rom: &'x LocalRom,
    env: &'x EmuEnv,
    id: &'x Identity,
    plan: Option<&'x Plan>,
    local: Option<&'x LocalUnit>,
    session_id: i64,
}

impl<'a> Syncer<'a> {
    pub async fn sync_rom(&mut self, rom: &LocalRom, env: &EmuEnv, play: Option<PlaySession>) -> Result<SyncReport> {
        let id = identity_for(self.state, rom);
        let plan = route::plan(env, rom, &id);
        let label = plan.as_ref().map_or_else(|| env.profile.id.clone(), |p| p.label.clone());
        let mut report = SyncReport::default();

        // A save Sigil refuses to read (two candidate memory cards, a damaged card) must not stop
        // playtime being recorded, nor be mistaken for "no save": sync nothing for this game and say why.
        if let Some(p @ Plan { kind: Kind::Unit(route), .. }) = &plan {
            if unit::has_unowned_saves(&route.platform) {
                self.claim_unowned(rom, &id, p, route, play.is_some());
            }
        }
        let (local, unreadable) = match &plan {
            None => (None, None),
            Some(p) => match gather_from_state(self.state, rom, &id, p) {
                Ok(l) => (l, None),
                Err(e) => (None, Some(e)),
            },
        };
        if let Some(e) = &unreadable {
            report.failed.push(format!("{}: {e}", name_base(rom, plan.as_ref())));
        }

        let mut saves_list = Vec::new();
        if let Some(l) = &local {
            let (hash, updated_at) = reported(l, self.state.saves.get(&save_key(rom.rom_id, &label)));
            saves_list.push(NegotiateSave {
                rom_id: rom.rom_id,
                file_name: l.file_name.clone(),
                slot: Some(l.slot.clone()),
                emulator: Some(l.emulator.clone()),
                content_hash: hash,
                updated_at,
                file_size_bytes: l.size,
            });
        }
        let resp = self
            .client
            .negotiate(&NegotiateRequest {
                device_id: self.device_id.to_string(),
                saves: saves_list,
                rom_ids: if unreadable.is_some() { vec![] } else { vec![rom.rom_id] },
                restore_unlisted: false,
                // No filter, as in Argosy: other clients label the same save differently (a libretro
                // core's name, `ppsspp_gold`), and a filter would hide theirs from us.
                emulators: None,
            })
            .await?;

        let (mut ok, mut bad) = (0u32, 0u32);
        for op in &resp.operations {
            let cx = Cx { rom, env, id: &id, plan: plan.as_ref(), local: local.as_ref(), session_id: resp.session_id };
            match self.apply(op, &cx, &mut report).await {
                Ok(()) => ok += 1,
                Err(e) => {
                    bad += 1;
                    report.failed.push(format!("{}: {e}", op.file_name));
                }
            }
        }
        // Completing the session is what records playtime; failing to do so
        // shouldn't discard the save work already done.
        if let Err(e) = self
            .client
            .complete_session(
                resp.session_id,
                &CompleteRequest { operations_completed: ok, operations_failed: bad, play_sessions: play.into_iter().collect() },
            )
            .await
        {
            report.failed.push(format!("completing sync session: {e}"));
        }
        Ok(report)
    }

    /// Sega CD, Dreamcast and Saturn saves carry no game id, so on a shared volume Sigil can't say whose
    /// they are until a client claims them (without this such a game would silently never sync).
    /// Argosy claims what appeared on the volume during a session, for the game that was played:
    /// every collect records which unowned saves exist, and after a play session the new ones are
    /// claimed. Nothing is claimed the first time a volume is seen, or outside a play session.
    fn claim_unowned(&mut self, rom: &LocalRom, id: &Identity, plan: &Plan, route: &unit::Route, claim_new: bool) {
        let key = route.state_key();
        let blob = self.state.sigil_state(&key);
        // A refusal is reported by the collect that follows; this only maintains the baseline.
        let Ok(first) = unit::collect_claiming(route, id, &plan.content_name, blob.as_deref(), &[]) else { return };
        let fresh = if claim_new { new_unowned(self.state.sigil_unowned.get(&key).map(Vec::as_slice), &first.unowned) } else { Vec::new() };
        if fresh.is_empty() {
            self.state.sigil_unowned.insert(key, first.unowned);
            return;
        }
        match unit::collect_claiming(route, id, &plan.content_name, blob.as_deref(), &fresh) {
            Ok(claimed) => {
                // The claim is recorded only in the state Sigil returns, so keep that now. The save has
                // not reached the server yet: until it does, it must not look unchanged.
                if let Some(u) = &claimed.unit {
                    self.state.set_sigil_state(&key, &u.state);
                }
                self.state.save_record(rom.rom_id, &plan.label).dirty = true;
                self.state.sigil_unowned.insert(key, claimed.unowned);
            }
            Err(e) => {
                tracing::warn!("could not claim {fresh:?} for {}: {e}", rom.stem);
                self.state.sigil_unowned.insert(key, first.unowned);
            }
        }
    }

    async fn apply(&mut self, op: &Operation, cx: &Cx<'_>, report: &mut SyncReport) -> Result<()> {
        match op.action {
            Action::NoOp | Action::Unknown => {
                report.unchanged += 1;
                // The server already holds exactly this: remember it, so later changes are measured from here.
                if let Some(local) = cx.local {
                    let hash = op.server_content_hash.clone().unwrap_or_else(|| local.hash.clone());
                    self.record_synced(cx, local, &hash, op.save_id, op.server_updated_at.clone());
                }
                Ok(())
            }
            Action::Upload => {
                let local = cx.local.ok_or_else(|| Error::msg("server asked for an upload but nothing is saved locally"))?;
                self.upload(cx, local, Some(SLOT.to_string()), local.file_name.clone(), report).await
            }
            Action::Download => {
                let id = op.save_id.ok_or_else(|| Error::msg("download operation without save_id"))?;
                self.download(cx, id, op.server_updated_at.clone(), report).await
            }
            Action::Conflict => self.resolve_conflict(op, cx, report).await,
            Action::Delete => {
                // The slot was emptied on the server. A raw save is backed up then removed; anything
                // Sigil cut out of an emulator's own files (cards, folders) is never deleted, only untracked.
                if let Some(l) = cx.local {
                    match &l.kind {
                        LocalKind::Raw(p) => {
                            // Where every other backup goes (a download's is `backup/<stamp>/<emulator>`), so
                            // this one can't be overwritten by the next, and it is found in the same place.
                            saves::backup_file(p, &saves::backup_dir_for(&self.backup_dir, &l.emulator), &l.file_name)?;
                            std::fs::remove_file(p)?;
                        }
                        LocalKind::Unit(_) => {
                            if let Some(Plan { kind: Kind::Unit(r), .. }) = cx.plan {
                                self.state.set_sigil_state(&r.state_key(), &[]);
                            }
                        }
                        // Read out of a disk image: nothing of ours to remove, only to stop tracking.
                        LocalKind::Bytes(_) => {}
                        // The folder is Xenia's own, like a card Sigil cut a unit out of: untracked, not deleted.
                        LocalKind::Folder(_) => {}
                    }
                    self.state.saves.remove(&save_key(cx.rom.rom_id, &l.emulator));
                }
                Ok(())
            }
        }
    }

    async fn upload(&mut self, cx: &Cx<'_>, local: &LocalUnit, slot: Option<String>, file_name: String, report: &mut SyncReport) -> Result<()> {
        let bytes = local.bytes()?;
        let hash = unit::content_hash(&bytes);
        let params = self.upload_params(cx, &local.emulator, slot.clone(), &hash, &file_name);
        match self.client.upload_save(&params, bytes).await {
            Ok(saved) => {
                if slot.is_some() {
                    // Only now has the save reached RomM, so only now does Sigil's state advance.
                    let stored = saved.content_hash.clone().unwrap_or(hash);
                    self.record_synced(cx, local, &stored, Some(saved.id), saved.updated_at.clone());
                    report.uploaded.push(file_name);
                } else {
                    report.archived.push(file_name);
                }
                Ok(())
            }
            // The server refuses to overwrite a slot another device filled.
            Err(e) if e.status() == Some(409) => {
                report.conflicts.push(PendingConflict {
                    rom_id: cx.rom.rom_id,
                    file_name,
                    server_updated_at: None,
                    reason: Some("server holds a newer save for this slot".into()),
                });
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    async fn download(&mut self, cx: &Cx<'_>, save_id: i64, server_updated_at: Option<String>, report: &mut SyncReport) -> Result<()> {
        let plan = cx.plan.ok_or_else(|| Error::msg("this emulator has no save rule"))?;
        // Find out before downloading anything whether the disk can take it.
        if let Kind::Xbox { image, .. } = &plan.kind {
            xbox::check_writable(image).map_err(|e| Error::msg(format!("the server's save was not applied: {e}. Close xemu and sync again")))?;
        }
        let bytes = self.client.download_save(save_id, self.device_id, cx.session_id).await?;
        let hash = unit::content_hash(&bytes);
        let backup = saves::backup_dir_for(&self.backup_dir, &plan.label);

        let name = match &plan.kind {
            Kind::Xbox { image, save_id: title } => {
                if !saves::is_zip(&bytes) {
                    return Err(Error::msg("the server's save for this game isn't a zip of its UDATA folder, so it wasn't applied to xemu"));
                }
                if let Some(local) = cx.local {
                    local.backup_to(&backup)?;
                }
                // All or nothing, verified by reading it back; a failure leaves the disk as it was.
                xbox::import_save(image, title, &bytes)?;
                self.remember_download(cx, &plan.label, &hash, save_id, server_updated_at);
                upload_name(&cx.rom.stem, "zip")
            }
            Kind::Raw { path, exists } => {
                if saves::is_zip(&bytes) {
                    return Err(Error::msg(format!(
                        "the server's save is a unit from a core with its own save layout; {} keeps a plain save file, so it wasn't applied",
                        plan.label
                    )));
                }
                let fname = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if *exists {
                    saves::backup_file(path, &backup, &fname)?;
                }
                saves::write_atomic(path, &bytes)?;
                self.remember_download(cx, &plan.label, &hash, save_id, server_updated_at);
                fname
            }
            Kind::Unit(route) => {
                if saves::is_legacy_bundle(&bytes) {
                    self.download_legacy(cx, plan, route, &bytes, &backup, save_id, &hash, report).await?;
                    return Ok(());
                }
                ensure_identified(route, cx.id, cx.rom)?;
                if let Some(local) = cx.local {
                    local.backup_to(&backup)?;
                }
                let blob = self.state.sigil_state(&route.state_key());
                let restored = unit::restore(route, cx.id, &plan.content_name, &bytes, blob.as_deref())?;
                self.state.set_sigil_state(&route.state_key(), &restored.state);
                self.remember_download(cx, &plan.label, &hash, save_id, server_updated_at);
                upload_name(&cx.rom.stem, &extension_of_unit(&bytes))
            }
            Kind::Folder { content, title_id } => {
                let title_id = ensure_title_id(title_id, &name_base(cx.rom, Some(plan)))?;
                if let Some(local) = cx.local {
                    local.backup_to(&backup)?;
                }
                let restored = xenia::restore(content, title_id, &bytes)?;
                tracing::debug!("restored {} file(s) into {}, {} header(s) written", restored.files, restored.dir.display(), restored.headers_written);
                self.remember_download(cx, &plan.label, &hash, save_id, server_updated_at);
                upload_name(&name_base(cx.rom, Some(plan)), "zip")
            }
        };
        report.downloaded.push(name);
        Ok(())
    }

    /// An older romm-desktop uploaded a zip with a manifest and paths relative to the emulator's data
    /// folder. Put it back, then replace the server's copy with the same save in Argosy's form, so
    /// every client can read it from here on.
    #[allow(clippy::too_many_arguments)]
    async fn download_legacy(
        &mut self,
        cx: &Cx<'_>,
        plan: &Plan,
        route: &unit::Route,
        bytes: &[u8],
        backup: &Path,
        save_id: i64,
        legacy_hash: &str,
        report: &mut SyncReport,
    ) -> Result<()> {
        saves::unpack_bundle(bytes, &cx.env.data_root, backup)?;
        // Converting needs the game's id; without one the files are back in place and the server's
        // copy simply stays as it was.
        let converted = if ensure_identified(route, cx.id, cx.rom).is_ok() { unit::collect(route, cx.id, &plan.content_name, None)? } else { None };
        let Some(u) = converted else {
            // Put back, but Sigil can't find it by id (the ROM couldn't be identified): leave the
            // server's copy as it was.
            self.remember_download(cx, &plan.label, legacy_hash, save_id, None);
            report.downloaded.push(upload_name(&cx.rom.stem, "zip"));
            return Ok(());
        };
        let file_name = upload_name(&cx.rom.stem, &u.extension());
        let params = self.upload_params(cx, &plan.label, Some(SLOT.to_string()), &u.content_hash, &file_name);
        let saved = self.client.replace_save(save_id, &params, u.data.clone()).await?;
        let stored = saved.content_hash.clone().unwrap_or_else(|| u.content_hash.clone());
        self.remember_download(cx, &plan.label, &stored, saved.id, saved.updated_at.clone());
        self.state.set_sigil_state(&route.state_key(), &u.state);
        report.downloaded.push(file_name);
        Ok(())
    }

    fn remember_download(&mut self, cx: &Cx<'_>, label: &str, hash: &str, save_id: i64, server_updated_at: Option<String>) {
        self.state.save_record(cx.rom.rom_id, label).mark_synced(hash, Some(save_id), server_updated_at);
    }

    fn upload_params(&self, cx: &Cx<'_>, emulator: &str, slot: Option<String>, content_hash: &str, file_name: &str) -> UploadParams {
        UploadParams {
            rom_id: cx.rom.rom_id,
            emulator: Some(emulator.to_string()),
            slot,
            device_id: Some(self.device_id.to_string()),
            session_id: Some(cx.session_id),
            content_hash: Some(content_hash.to_string()),
            file_name: file_name.to_string(),
        }
    }

    async fn resolve_conflict(&mut self, op: &Operation, cx: &Cx<'_>, report: &mut SyncReport) -> Result<()> {
        let server_id = op.save_id.ok_or_else(|| Error::msg("conflict without save_id"))?;
        let Some(local) = cx.local else {
            // Nothing local to protect: just take the server's.
            return self.download(cx, server_id, op.server_updated_at.clone(), report).await;
        };
        // Both of these end by writing the server's save to xemu's disk. If it can't be written right
        // now, find out before the first half: keeping both would otherwise archive another copy of the
        // local save on the server at every sync while the restore keeps failing.
        if let Some(Plan { kind: Kind::Xbox { image, .. }, .. }) = cx.plan {
            if matches!(self.policy, ConflictPolicy::KeepBoth | ConflictPolicy::ServerWins) {
                xbox::check_writable(image).map_err(|e| Error::msg(format!("the conflict was not settled: {e}. Close xemu and sync again")))?;
            }
        }
        match self.policy {
            ConflictPolicy::Ask => {
                report.conflicts.push(PendingConflict {
                    rom_id: cx.rom.rom_id,
                    file_name: local.file_name.clone(),
                    server_updated_at: op.server_updated_at.clone(),
                    reason: op.reason.clone(),
                });
                Ok(())
            }
            ConflictPolicy::ServerWins => self.download(cx, server_id, op.server_updated_at.clone(), report).await,
            ConflictPolicy::KeepBoth => {
                let ext = Path::new(&local.file_name).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
                let archive_name = upload_name(&saves::conflict_name(&name_base(cx.rom, cx.plan)), &ext);
                // 1. preserve the local copy on the server as an archival save (no slot, never paired)
                self.upload(cx, local, None, archive_name, report).await?;
                // 2. then adopt the server's version; download() also backs the local save up on disk
                self.download(cx, server_id, op.server_updated_at.clone(), report).await
            }
            ConflictPolicy::DeviceWins => {
                let bytes = local.bytes()?;
                let hash = unit::content_hash(&bytes);
                let params = self.upload_params(cx, &local.emulator, Some(local.slot.clone()), &hash, &local.file_name);
                let saved = self.client.replace_save(server_id, &params, bytes).await?;
                let stored = saved.content_hash.clone().unwrap_or(hash);
                self.record_synced(cx, local, &stored, Some(saved.id), saved.updated_at.clone());
                report.uploaded.push(local.file_name.clone());
                Ok(())
            }
        }
    }

    /// The save is now on the server: remember what was exchanged, and let Sigil's state catch up
    /// (it must not advance before this, or a failed upload would lose the change marker).
    fn record_synced(&mut self, cx: &Cx<'_>, local: &LocalUnit, hash: &str, save_id: Option<i64>, server_updated_at: Option<String>) {
        self.state.save_record(cx.rom.rom_id, &local.emulator).mark_synced(hash, save_id, server_updated_at);
        if let (LocalKind::Unit(u), Some(Plan { kind: Kind::Unit(route), .. })) = (&local.kind, cx.plan) {
            self.state.set_sigil_state(&route.state_key(), &u.state);
        }
    }
}

/// `zip` for a zip unit, else nothing: the extension a restored unit is reported under.
fn extension_of_unit(bytes: &[u8]) -> String {
    if saves::is_zip(bytes) { "zip".into() } else { String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom(platform: &str, launch: &str) -> LocalRom {
        LocalRom { rom_id: 5, platform_slug: platform.into(), launch_path: PathBuf::from(launch), stem: "Game".into(), ..Default::default() }
    }

    fn state_with(identity: Identity) -> State {
        let mut s = State::default();
        s.roms.insert(5, LocalRom { rom_id: 5, identity: Some(identity), ..Default::default() });
        s
    }

    fn read_as(platform: &str, source: &str) -> Identity {
        Identity { source: source.into(), platform: unit::platform_slug(platform), title_id: "T".into(), ..Default::default() }
    }

    #[test]
    fn a_cached_identity_is_used_while_the_launch_file_and_platform_are_unchanged() {
        let state = state_with(read_as("ngc", "/roms/Game.iso"));
        // `ngc` and `gc` are the same platform to Sigil, so a RomM that renames the slug is not a new game.
        assert_eq!(cached_identity(&state, &rom("gc", "/roms/Game.iso")).map(|i| i.title_id), Some("T".into()));
    }

    #[test]
    fn a_cached_identity_is_dropped_when_the_launch_file_changes() {
        let state = state_with(read_as("psp", "/roms/Old.iso"));
        assert!(cached_identity(&state, &rom("psp", "/roms/New.iso")).is_none());
    }

    #[test]
    fn a_cached_identity_is_dropped_when_the_platform_changes() {
        // What the saves overview used to miss: it checked the file but not the platform, so a game
        // moved to another platform kept being described by the reading of the old one.
        let state = state_with(read_as("psp", "/roms/Game.iso"));
        assert!(cached_identity(&state, &rom("psvita", "/roms/Game.iso")).is_none());
    }

    #[test]
    fn merging_reports_adds_every_outcome_and_keeps_their_order() {
        let conflict = |n: &str| PendingConflict { rom_id: 1, file_name: n.into(), server_updated_at: None, reason: None };
        let mut total = SyncReport {
            uploaded: vec!["a".into()],
            downloaded: vec!["b".into()],
            archived: vec!["c".into()],
            conflicts: vec![conflict("d")],
            failed: vec!["e".into()],
            unchanged: 2,
        };
        total.merge(SyncReport {
            uploaded: vec!["a2".into()],
            downloaded: vec!["b2".into()],
            archived: vec!["c2".into()],
            conflicts: vec![conflict("d2")],
            failed: vec!["e2".into()],
            unchanged: 3,
        });
        assert_eq!(total.uploaded, ["a", "a2"]);
        assert_eq!(total.downloaded, ["b", "b2"]);
        // The ones a hand-written merge forgot: a total must not lose that a local copy was archived.
        assert_eq!(total.archived, ["c", "c2"]);
        assert_eq!(total.conflicts.iter().map(|c| c.file_name.as_str()).collect::<Vec<_>>(), ["d", "d2"]);
        assert_eq!(total.failed, ["e", "e2"]);
        assert_eq!(total.unchanged, 5);
    }

    #[test]
    fn nothing_is_cached_for_a_game_that_was_never_read() {
        assert!(cached_identity(&State::default(), &rom("psp", "/roms/Game.iso")).is_none());
    }
}
