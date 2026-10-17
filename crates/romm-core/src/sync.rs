//! Save sync against RomM's device-sync protocol: negotiate → apply operations →
//! complete the session.

use std::path::PathBuf;
use std::time::SystemTime;

use chrono::Utc;

use crate::api::types::*;
use crate::api::RommClient;
use crate::error::{Error, Result};
use crate::profiles::{Profile, SaveMode};
use crate::retroarch;
use crate::saves::{self, SLOT};
use crate::state::{save_key, LocalRom, SaveRecord, State};

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
}

#[derive(Debug, Clone)]
pub enum UnitKind {
    /// A single file saved as-is (compatible with RomM's own web player).
    Raw(PathBuf),
    /// Files under `data_root`, zipped.
    Bundle { files: Vec<String> },
}

#[derive(Debug, Clone)]
pub struct LocalUnit {
    pub emulator: String,
    pub slot: String,
    pub file_name: String,
    pub kind: UnitKind,
}

impl EmuEnv {
    pub fn save_mode(&self) -> Option<SaveMode> {
        self.profile.saves.as_ref().map(|s| s.mode)
    }

    /// Folders to snapshot around a play session (diff mode).
    pub fn watch_roots(&self) -> Vec<String> {
        match &self.profile.saves {
            Some(r) if r.mode == SaveMode::Diff => r.roots.clone(),
            _ => vec![],
        }
    }

    /// Stem mode: (path, exists). When no save exists yet, the path is where
    /// RetroArch would create it.
    pub fn stem_target(&self, rom: &LocalRom) -> Option<(PathBuf, bool)> {
        let rule = self.profile.saves.as_ref()?;
        let layout = self.retroarch.as_ref()?;
        let ext = rule.extensions.first()?;
        match retroarch::find_save(layout, &rom.stem, ext) {
            Some(p) => Some((p, true)),
            None => Some((layout.saves.join(format!("{}.{ext}", rom.stem)), false)),
        }
    }

    /// Saves that currently exist locally for this ROM.
    pub fn save_units(&self, rom: &LocalRom, state: &State) -> Vec<LocalUnit> {
        let emu = self.profile.id.clone();
        match self.save_mode() {
            Some(SaveMode::Stem) => match self.stem_target(rom) {
                Some((path, true)) => {
                    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    vec![LocalUnit { emulator: emu, slot: SLOT.into(), file_name, kind: UnitKind::Raw(path) }]
                }
                _ => vec![],
            },
            Some(SaveMode::Diff) => {
                let files: Vec<String> = state
                    .saves
                    .get(&save_key(rom.rom_id, &emu))
                    .map(|r| r.files.iter().filter(|f| self.data_root.join(f).is_file()).cloned().collect())
                    .unwrap_or_default();
                if files.is_empty() {
                    return vec![];
                }
                vec![LocalUnit {
                    emulator: emu,
                    slot: SLOT.into(),
                    file_name: format!("{}.zip", rom.stem),
                    kind: UnitKind::Bundle { files },
                }]
            }
            None => vec![],
        }
    }
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
    /// e.g. "2 files" for shared-folder bundles
    pub detail: Option<String>,
    pub status: SaveStatus,
}

/// Local view of a game's saves and whether each differs from the last sync. No network.
pub fn saves_overview(env: &EmuEnv, rom: &LocalRom, db: &State) -> Result<Vec<SaveView>> {
    let mut out = Vec::new();
    for u in env.save_units(rom, db) {
        let rec = db.saves.get(&save_key(rom.rom_id, &u.emulator));
        let (size, modified, detail, status) = match &u.kind {
            UnitKind::Raw(p) => {
                let meta = std::fs::metadata(p)?;
                let hash = saves::md5_file(p)?;
                let st = match rec.and_then(|r| r.last_hash.as_deref()) {
                    None => SaveStatus::NeverSynced,
                    Some(h) if h == hash => SaveStatus::Synced,
                    Some(_) => SaveStatus::Modified,
                };
                (meta.len(), meta.modified().ok().map(saves::iso), None, st)
            }
            UnitKind::Bundle { files } => {
                let fp = saves::fingerprint(&env.data_root, files)?;
                let size = files.iter().filter_map(|f| std::fs::metadata(env.data_root.join(f)).ok()).map(|m| m.len()).sum();
                let st = match rec {
                    Some(r) if r.last_hash.is_some() && r.fingerprint.as_deref() == Some(fp.as_str()) => SaveStatus::Synced,
                    Some(r) if r.last_hash.is_some() => SaveStatus::Modified,
                    _ => SaveStatus::NeverSynced,
                };
                let when = saves::latest_mtime(&env.data_root, files);
                (size, Some(saves::iso(when)), Some(format!("{} file{}", files.len(), if files.len() == 1 { "" } else { "s" })), st)
            }
        };
        out.push(SaveView { name: u.file_name, size, modified, detail, status });
    }
    Ok(out)
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

impl<'a> Syncer<'a> {
    /// Hash/size/mtime of a local unit, exactly as it would be uploaded.
    fn describe(&self, rom: &LocalRom, env: &EmuEnv, unit: &LocalUnit) -> Result<(String, u64, SystemTime)> {
        match &unit.kind {
            UnitKind::Raw(p) => {
                let meta = std::fs::metadata(p)?;
                Ok((saves::md5_file(p)?, meta.len(), meta.modified()?))
            }
            UnitKind::Bundle { files } => {
                let bytes = saves::pack_bundle(&unit.emulator, &env.data_root, files)?;
                let fp = saves::fingerprint(&env.data_root, files)?;
                // If nothing changed since the last sync, report the hash we last
                // exchanged with the server rather than a freshly packed one, so a
                // zip-library upgrade can never fake a local change.
                let hash = match self.state.saves.get(&save_key(rom.rom_id, &unit.emulator)) {
                    Some(r) if r.fingerprint.as_deref() == Some(fp.as_str()) && r.last_hash.is_some() => {
                        r.last_hash.clone().unwrap()
                    }
                    _ => saves::md5_bytes(&bytes),
                };
                Ok((hash, bytes.len() as u64, saves::latest_mtime(&env.data_root, files)))
            }
        }
    }

    fn unit_bytes(&self, env: &EmuEnv, unit: &LocalUnit) -> Result<Vec<u8>> {
        match &unit.kind {
            UnitKind::Raw(p) => Ok(std::fs::read(p)?),
            UnitKind::Bundle { files } => saves::pack_bundle(&unit.emulator, &env.data_root, files),
        }
    }

    pub async fn sync_rom(&mut self, rom: &LocalRom, env: &EmuEnv, play: Option<PlaySession>) -> Result<SyncReport> {
        let emu = env.profile.id.clone();
        let units = env.save_units(rom, self.state);

        let mut saves_list = Vec::new();
        for u in &units {
            let (hash, size, mtime) = self.describe(rom, env, u)?;
            saves_list.push(NegotiateSave {
                rom_id: rom.rom_id,
                file_name: u.file_name.clone(),
                slot: Some(u.slot.clone()),
                emulator: Some(emu.clone()),
                content_hash: hash,
                updated_at: saves::iso(mtime),
                file_size_bytes: size,
            });
        }
        let resp = self
            .client
            .negotiate(&NegotiateRequest {
                device_id: self.device_id.to_string(),
                saves: saves_list,
                rom_ids: vec![rom.rom_id],
                restore_unlisted: false,
                emulators: Some(vec![emu.clone()]),
            })
            .await?;

        let mut report = SyncReport::default();
        let (mut ok, mut bad) = (0u32, 0u32);
        for op in &resp.operations {
            match self.apply(op, rom, env, resp.session_id, &units, &mut report).await {
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

    async fn apply(
        &mut self,
        op: &Operation,
        rom: &LocalRom,
        env: &EmuEnv,
        session_id: i64,
        units: &[LocalUnit],
        report: &mut SyncReport,
    ) -> Result<()> {
        let unit = units.iter().find(|u| Some(&u.slot) == op.slot.as_ref()).or_else(|| units.first());
        match op.action {
            Action::NoOp | Action::Unknown => {
                report.unchanged += 1;
                Ok(())
            }
            Action::Upload => {
                let unit = unit.ok_or_else(|| Error::msg("server asked for an upload but nothing is saved locally"))?;
                self.upload(rom, env, session_id, unit, Some(SLOT.to_string()), unit.file_name.clone(), report).await
            }
            Action::Download => {
                let id = op.save_id.ok_or_else(|| Error::msg("download operation without save_id"))?;
                self.download(rom, env, session_id, id, unit, report).await
            }
            Action::Conflict => self.resolve_conflict(op, rom, env, session_id, unit, report).await,
            Action::Delete => {
                // The slot was emptied on the server. Raw saves are backed up then
                // removed; shared memory cards are never deleted, only untracked.
                if let Some(u) = unit {
                    match &u.kind {
                        UnitKind::Raw(p) => {
                            saves::backup_file(p, &self.backup_dir, &u.file_name)?;
                            std::fs::remove_file(p)?;
                        }
                        UnitKind::Bundle { .. } => {
                            self.state.saves.remove(&save_key(rom.rom_id, &u.emulator));
                        }
                    }
                }
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn upload(
        &mut self,
        rom: &LocalRom,
        env: &EmuEnv,
        session_id: i64,
        unit: &LocalUnit,
        slot: Option<String>,
        file_name: String,
        report: &mut SyncReport,
    ) -> Result<()> {
        let bytes = self.unit_bytes(env, unit)?;
        let hash = saves::md5_bytes(&bytes);
        let params = UploadParams {
            rom_id: rom.rom_id,
            emulator: Some(unit.emulator.clone()),
            slot: slot.clone(),
            device_id: Some(self.device_id.to_string()),
            session_id: Some(session_id),
            content_hash: Some(hash.clone()),
            file_name: file_name.clone(),
        };
        match self.client.upload_save(&params, bytes).await {
            Ok(saved) => {
                if slot.is_some() {
                    self.record_synced(rom, env, unit, &hash, Some(saved.id))?;
                    report.uploaded.push(file_name);
                } else {
                    report.archived.push(file_name);
                }
                Ok(())
            }
            // The server refuses to overwrite a slot another device filled.
            Err(e) if e.status() == Some(409) => {
                report.conflicts.push(PendingConflict {
                    rom_id: rom.rom_id,
                    file_name,
                    server_updated_at: None,
                    reason: Some("server holds a newer save for this slot".into()),
                });
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    async fn download(
        &mut self,
        rom: &LocalRom,
        env: &EmuEnv,
        session_id: i64,
        save_id: i64,
        _unit: Option<&LocalUnit>,
        report: &mut SyncReport,
    ) -> Result<()> {
        let bytes = self.client.download_save(save_id, self.device_id, session_id).await?;
        let hash = saves::md5_bytes(&bytes);
        let emu = &env.profile.id;
        let stamp = Utc::now().format("%Y%m%dT%H%M%S").to_string();
        let backup = self.backup_dir.join(&stamp).join(emu);

        let name = match env.save_mode() {
            Some(SaveMode::Stem) => {
                let (path, exists) = env.stem_target(rom).ok_or_else(|| Error::msg("no save location for this emulator"))?;
                let fname = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if exists {
                    saves::backup_file(&path, &backup, &fname)?;
                }
                saves::write_atomic(&path, &bytes)?;
                fname
            }
            Some(SaveMode::Diff) => {
                if !saves::is_zip(&bytes) {
                    return Err(Error::msg(
                        "server save isn't a romm-desktop bundle (it was probably uploaded by another client)",
                    ));
                }
                let written = saves::unpack_bundle(&bytes, &env.data_root, &backup)?;
                let rec = self.state.saves.entry(save_key(rom.rom_id, emu)).or_default();
                let mut files: Vec<String> = rec.files.iter().cloned().chain(written).collect();
                files.sort();
                files.dedup();
                rec.files = files;
                format!("{}.zip", rom.stem)
            }
            None => return Err(Error::msg("emulator has no save rule")),
        };
        // Baseline for the next negotiate.
        let fp = match env.save_mode() {
            Some(SaveMode::Diff) => {
                let files = self.state.saves.get(&save_key(rom.rom_id, emu)).map(|r| r.files.clone()).unwrap_or_default();
                Some(saves::fingerprint(&env.data_root, &files)?)
            }
            _ => None,
        };
        let rec = self.state.saves.entry(save_key(rom.rom_id, emu)).or_insert_with(SaveRecord::default);
        rec.last_hash = Some(hash);
        rec.fingerprint = fp;
        rec.server_save_id = Some(save_id);
        report.downloaded.push(name);
        Ok(())
    }

    async fn resolve_conflict(
        &mut self,
        op: &Operation,
        rom: &LocalRom,
        env: &EmuEnv,
        session_id: i64,
        unit: Option<&LocalUnit>,
        report: &mut SyncReport,
    ) -> Result<()> {
        let server_id = op.save_id.ok_or_else(|| Error::msg("conflict without save_id"))?;
        let Some(unit) = unit else {
            // Nothing local to protect: just take the server's.
            return self.download(rom, env, session_id, server_id, None, report).await;
        };
        match self.policy {
            ConflictPolicy::Ask => {
                report.conflicts.push(PendingConflict {
                    rom_id: rom.rom_id,
                    file_name: unit.file_name.clone(),
                    server_updated_at: op.server_updated_at.clone(),
                    reason: op.reason.clone(),
                });
                Ok(())
            }
            ConflictPolicy::ServerWins => self.download(rom, env, session_id, server_id, Some(unit), report).await,
            ConflictPolicy::KeepBoth => {
                let stamp = Utc::now().format("%Y%m%dT%H%M%S");
                let archive_name = match &unit.kind {
                    UnitKind::Raw(p) => {
                        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        let ext = p.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        format!("{stem}.conflict-{stamp}.{ext}")
                    }
                    UnitKind::Bundle { .. } => format!("{}.conflict-{stamp}.zip", rom.stem),
                };
                // 1. preserve the local copy on the server as an archival save (no slot, never paired)
                self.upload(rom, env, session_id, unit, None, archive_name, report).await?;
                // 2. then adopt the server's version; download() also backs the local file up on disk
                self.download(rom, env, session_id, server_id, Some(unit), report).await
            }
            ConflictPolicy::DeviceWins => {
                let bytes = self.unit_bytes(env, unit)?;
                let hash = saves::md5_bytes(&bytes);
                let params = UploadParams {
                    rom_id: rom.rom_id,
                    emulator: Some(unit.emulator.clone()),
                    slot: Some(unit.slot.clone()),
                    device_id: Some(self.device_id.to_string()),
                    session_id: Some(session_id),
                    content_hash: Some(hash.clone()),
                    file_name: unit.file_name.clone(),
                };
                let saved = self.client.replace_save(server_id, &params, bytes).await?;
                self.record_synced(rom, env, unit, &hash, Some(saved.id))?;
                report.uploaded.push(unit.file_name.clone());
                Ok(())
            }
        }
    }

    fn record_synced(&mut self, rom: &LocalRom, env: &EmuEnv, unit: &LocalUnit, hash: &str, save_id: Option<i64>) -> Result<()> {
        let fp = match &unit.kind {
            UnitKind::Bundle { files } => Some(saves::fingerprint(&env.data_root, files)?),
            UnitKind::Raw(_) => None,
        };
        let rec = self.state.saves.entry(save_key(rom.rom_id, &unit.emulator)).or_default();
        rec.last_hash = Some(hash.to_string());
        rec.fingerprint = fp;
        rec.server_save_id = save_id;
        Ok(())
    }
}
