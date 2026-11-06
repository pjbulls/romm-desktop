//! Save sync against RomM's device-sync protocol: negotiate → apply operations → complete the
//! session.
//!
//! What a game's save *is*, and the bytes that travel, come from `route` and `unit` (Sigil, the
//! library Argosy uses), so a save uploaded here is one Argosy restores and vice versa. This module
//! is the protocol around that: what to tell the server, what to do with its answer, and what to
//! remember afterwards.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::Utc;

use crate::api::types::*;
use crate::api::RommClient;
use crate::error::{Error, Result};
use crate::profiles::{Profile, SaveMode};
use crate::retroarch;
use crate::route::{self, Kind, Plan};
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

impl EmuEnv {
    pub fn save_mode(&self) -> Option<SaveMode> {
        self.profile.saves.as_ref().map(|s| s.mode)
    }
}

/// The ids Sigil read from the ROM, cached in the state file. Argosy reads them once, when a game is
/// installed; here that happens the first time the game is synced (and again if its launch path
/// changes, which means the ROM did).
pub fn identity_for(state: &mut State, rom: &LocalRom) -> Identity {
    let source = rom.launch_path.to_string_lossy();
    let platform = unit::platform_slug(&rom.platform_slug);
    if let Some(id) = state.roms.get(&rom.rom_id).and_then(|r| r.identity.as_ref()) {
        if id.source == source && id.platform == platform {
            return id.clone();
        }
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
    /// The bytes that would be uploaded.
    pub fn bytes(&self) -> Result<Vec<u8>> {
        match &self.kind {
            LocalKind::Raw(p) => Ok(std::fs::read(p)?),
            LocalKind::Unit(u) => Ok(u.data.clone()),
            LocalKind::Bytes(b) => Ok(b.clone()),
        }
    }

    fn detail(&self) -> Option<String> {
        let LocalKind::Unit(u) = &self.kind else { return None };
        if u.shape == unit::Shape::Single {
            return None;
        }
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(&u.data)).ok()?;
        let n = (0..z.len()).filter(|i| z.by_index(*i).is_ok_and(|f| !f.name().ends_with('/'))).count();
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
            if !xbox::is_title_id(save_id) {
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
            let Some(data) = hdd.export_zip(save_id)? else { return Ok(None) };
            let hash = unit::content_hash(&data);
            Ok(Some(LocalUnit {
                emulator: plan.label.clone(),
                slot: SLOT.into(),
                file_name: upload_name(&rom.stem, "zip"),
                size: data.len() as u64,
                // The disk image's own timestamp: the folder's are not read. A save that has not
                // changed is reported with the server's timestamp anyway (see `reported`).
                modified: std::fs::metadata(image)?.modified()?,
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
    }
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
    let id = db
        .roms
        .get(&rom.rom_id)
        .and_then(|r| r.identity.clone())
        .filter(|i| i.source == rom.launch_path.to_string_lossy())
        .unwrap_or_else(|| unit::identify(&rom.launch_path, &rom.platform_slug));
    let Some(plan) = route::plan(env, rom, &id) else { return Ok(vec![]) };
    let rec = db.saves.get(&save_key(rom.rom_id, &plan.label));
    let blob = match &plan.kind {
        Kind::Unit(r) => db.sigil_state(&r.state_key()),
        Kind::Raw { .. } | Kind::Xbox { .. } => None,
    };
    let Some(local) = gather(rom, &id, &plan, rec, blob.as_deref())? else { return Ok(vec![]) };
    let status = match rec.and_then(|r| r.last_hash.as_deref()) {
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
            Some(p) => {
                let rec = self.state.saves.get(&save_key(rom.rom_id, &p.label));
                let blob = match &p.kind {
                    Kind::Unit(r) => self.state.sigil_state(&r.state_key()),
                    Kind::Raw { .. } | Kind::Xbox { .. } => None,
                };
                match gather(rom, &id, p, rec, blob.as_deref()) {
                    Ok(l) => (l, None),
                    Err(e) => (None, Some(e)),
                }
            }
        };
        if let Some(e) = &unreadable {
            report.failed.push(format!("{}: {e}", rom.stem));
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
                self.state.saves.entry(save_key(rom.rom_id, &plan.label)).or_default().dirty = true;
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
                            saves::backup_file(p, &self.backup_dir, &l.file_name)?;
                            std::fs::remove_file(p)?;
                        }
                        LocalKind::Unit(_) => {
                            if let Some(Plan { kind: Kind::Unit(r), .. }) = cx.plan {
                                self.state.set_sigil_state(&r.state_key(), &[]);
                            }
                        }
                        // Read out of a disk image: nothing of ours to remove, only to stop tracking.
                        LocalKind::Bytes(_) => {}
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
        let params = UploadParams {
            rom_id: cx.rom.rom_id,
            emulator: Some(local.emulator.clone()),
            slot: slot.clone(),
            device_id: Some(self.device_id.to_string()),
            session_id: Some(cx.session_id),
            content_hash: Some(hash.clone()),
            file_name: file_name.clone(),
        };
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
        let stamp = Utc::now().format("%Y%m%dT%H%M%S").to_string();
        let backup = self.backup_dir.join(&stamp).join(&plan.label);

        let name = match &plan.kind {
            Kind::Xbox { image, save_id: title } => {
                if !saves::is_zip(&bytes) {
                    return Err(Error::msg("the server's save for this game isn't a zip of its UDATA folder, so it wasn't applied to xemu"));
                }
                // Keep what is there, in the form that would have been uploaded.
                if let Some(LocalUnit { kind: LocalKind::Bytes(b), file_name, .. }) = cx.local {
                    std::fs::create_dir_all(&backup)?;
                    std::fs::write(backup.join(file_name), b)?;
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
                // Keep what is there, in the form that would have been uploaded.
                if let Some(LocalUnit { kind: LocalKind::Unit(u), file_name, .. }) = cx.local {
                    std::fs::create_dir_all(&backup)?;
                    std::fs::write(backup.join(file_name), &u.data)?;
                }
                let blob = self.state.sigil_state(&route.state_key());
                let restored = unit::restore(route, cx.id, &plan.content_name, &bytes, blob.as_deref())?;
                self.state.set_sigil_state(&route.state_key(), &restored.state);
                self.remember_download(cx, &plan.label, &hash, save_id, server_updated_at);
                upload_name(&cx.rom.stem, &extension_of_unit(&bytes))
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
        let params = UploadParams {
            rom_id: cx.rom.rom_id,
            emulator: Some(plan.label.clone()),
            slot: Some(SLOT.to_string()),
            device_id: Some(self.device_id.to_string()),
            session_id: Some(cx.session_id),
            content_hash: Some(u.content_hash.clone()),
            file_name: file_name.clone(),
        };
        let saved = self.client.replace_save(save_id, &params, u.data.clone()).await?;
        let rec = self.state.saves.entry(save_key(cx.rom.rom_id, &plan.label)).or_default();
        rec.last_hash = Some(saved.content_hash.clone().unwrap_or_else(|| u.content_hash.clone()));
        rec.server_save_id = Some(saved.id);
        rec.server_updated_at = saved.updated_at.clone();
        rec.dirty = false;
        self.state.set_sigil_state(&route.state_key(), &u.state);
        report.downloaded.push(file_name);
        Ok(())
    }

    fn remember_download(&mut self, cx: &Cx<'_>, label: &str, hash: &str, save_id: i64, server_updated_at: Option<String>) {
        let rec = self.state.saves.entry(save_key(cx.rom.rom_id, label)).or_default();
        rec.last_hash = Some(hash.to_string());
        rec.server_save_id = Some(save_id);
        rec.server_updated_at = server_updated_at.or_else(|| Some(saves::iso(SystemTime::now())));
        rec.dirty = false;
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
                let stamp = Utc::now().format("%Y%m%dT%H%M%S");
                let ext = Path::new(&local.file_name).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
                let archive_name = upload_name(&format!("{}.conflict-{stamp}", cx.rom.stem), &ext);
                // 1. preserve the local copy on the server as an archival save (no slot, never paired)
                self.upload(cx, local, None, archive_name, report).await?;
                // 2. then adopt the server's version; download() also backs the local save up on disk
                self.download(cx, server_id, op.server_updated_at.clone(), report).await
            }
            ConflictPolicy::DeviceWins => {
                let bytes = local.bytes()?;
                let hash = unit::content_hash(&bytes);
                let params = UploadParams {
                    rom_id: cx.rom.rom_id,
                    emulator: Some(local.emulator.clone()),
                    slot: Some(local.slot.clone()),
                    device_id: Some(self.device_id.to_string()),
                    session_id: Some(cx.session_id),
                    content_hash: Some(hash.clone()),
                    file_name: local.file_name.clone(),
                };
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
        let rec = self.state.saves.entry(save_key(cx.rom.rom_id, &local.emulator)).or_default();
        rec.last_hash = Some(hash.to_string());
        if save_id.is_some() {
            rec.server_save_id = save_id;
        }
        rec.server_updated_at = server_updated_at.or_else(|| Some(saves::iso(SystemTime::now())));
        rec.dirty = false;
        if let (LocalKind::Unit(u), Some(Plan { kind: Kind::Unit(route), .. })) = (&local.kind, cx.plan) {
            self.state.set_sigil_state(&route.state_key(), &u.state);
        }
    }
}

/// `zip` for a zip unit, else nothing: the extension a restored unit is reported under.
fn extension_of_unit(bytes: &[u8]) -> String {
    if saves::is_zip(bytes) { "zip".into() } else { String::new() }
}
