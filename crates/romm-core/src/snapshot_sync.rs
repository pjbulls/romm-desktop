//! Save sync for a RomM with snapshots (5.5 and later). A child of `sync`, which has the legacy engine.
//!
//! From 5.5 a game's saves are not slots any more but *channels*: a line of snapshots with a current
//! one, keyed to one of the ROM's files. A client lists the channel, decides on its own device what
//! to do, then pushes a new snapshot or downloads the current one. Argosy does exactly that for every
//! game ("Save sync against RomM's snapshot API for every game RomM knows"), and this follows it.
//!
//! Why the older engine (`negotiate`, slots) can't just carry on, though 5.5 still answers it:
//!
//! - A unit Sigil builds is pushed as a `neutral` save, and `negotiate` never offers a neutral save to
//!   a client (`slot_heads` leaves them out), so a PSP save Argosy pushed would never reach this app.
//! - Hardcore is a flag of the channel, set by a snapshot's manifest. A slot upload to a hardcore
//!   channel is left out of it ("A hardcore channel... stays legacy"), and the server knows nothing of
//!   the marker Argosy used to append (`hardcore`), so that mechanism has no meaning here.
//!
//! Which engine runs is the server's say, by its heartbeat (`RommClient::capabilities`); a server
//! that doesn't answer or isn't new enough gets the older one, as before.

use super::*;
use crate::report::PendingConflict;
use crate::state::{channel_key, ChannelRecord, DEFAULT_CHANNEL};

// ---------------------------------------------------------------------------------------------
// The decision, ported from Argosy's `SnapshotDecision`
// ---------------------------------------------------------------------------------------------

/// A save as the two hashes RomM keeps of it. `identity` is of the saves themselves, unmoved by
/// timestamps and placement; `content` is of the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hashes {
    pub content: String,
    pub identity: String,
}

impl Hashes {
    /// The same save, even when it was packed again: the identity hash is of the saves alone. An
    /// identity that is empty says nothing and never matches.
    pub fn same_save_as(&self, other: &Hashes) -> bool {
        (!self.identity.is_empty() && self.identity == other.identity) || (!self.content.is_empty() && self.content == other.content)
    }
}

/// A snapshot, and the save in it when it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Point {
    pub id: i64,
    pub save: Option<Hashes>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    Nothing,
    /// Write a snapshot on top of `expected` (what the push believes is the channel's current; `None`:
    /// an empty channel), built on `parent`.
    Push { expected: Option<i64>, parent: Option<i64> },
    Download(i64),
    /// The save on disk already is the channel's current; only note that this device holds it.
    Adopt(i64),
    /// Both this device and the channel moved on from what the device held.
    Conflict(i64),
}

/// What to do for one channel, from the snapshot this device last pushed or applied (`held`), the
/// channel's `current`, and the save on disk now (`local`). A missing local save never reads as a
/// change, so a cleared save folder is refilled and not pushed as empty.
pub(crate) fn decide(held: Option<&Point>, current: Option<&Point>, local: Option<&Hashes>) -> Action {
    let Some(current) = current else {
        return if local.is_some() { Action::Push { expected: None, parent: held.map(|h| h.id) } } else { Action::Nothing };
    };
    let Some(held) = held else {
        return match local {
            None => Action::Download(current.id),
            Some(l) if current.save.as_ref().is_some_and(|c| l.same_save_as(c)) => Action::Adopt(current.id),
            Some(_) => Action::Conflict(current.id),
        };
    };
    let dirty = local.is_some_and(|l| held.save.as_ref().is_none_or(|h| !l.same_save_as(h)));
    if local.is_none() && held.id == current.id && current.save.is_some() {
        Action::Download(current.id)
    } else if held.id == current.id {
        if dirty {
            Action::Push { expected: Some(current.id), parent: Some(held.id) }
        } else {
            Action::Nothing
        }
    } else if dirty {
        Action::Conflict(current.id)
    } else {
        Action::Download(current.id)
    }
}

/// Stands in for a snapshot id this device held but can no longer name. Real ids start at 1, so it is
/// never the channel's current, and it is never sent to the server (see `sync_channel`).
pub(crate) const NO_SNAPSHOT: i64 = 0;

/// What a device that has only ever synced this game the older way can be taken to hold.
///
/// Such a device has no channel record (`decide` would call it a device that holds nothing, and any
/// difference from the server a conflict), but it does know the save it last exchanged: `last_synced`,
/// the content hash the older engine kept. That is enough to tell which side has moved since:
///
/// - the channel's current is still that save: the device holds the current, so what changed here
///   is a change to push, not a conflict;
/// - the channel's current is something else: the device holds an earlier snapshot it can't name
///   ([`NO_SNAPSHOT`]), so the channel moved on, and a clean device follows it while one that changed
///   too has a real conflict.
///
/// `None` (so `decide` goes on as for a device that holds nothing) when there is nothing to go by: no
/// last-synced hash, an empty channel, or a local save that already is the channel's current, which
/// `decide` adopts.
pub(crate) fn seed_held(last_synced: &str, current: Option<&Point>, local: Option<&Hashes>) -> Option<Point> {
    let current = current?;
    if last_synced.is_empty() {
        return None;
    }
    if let (Some(l), Some(c)) = (local, current.save.as_ref()) {
        if l.same_save_as(c) {
            return None;
        }
    }
    match &current.save {
        Some(c) if c.content == last_synced => Some(current.clone()),
        // Only the content hash was kept, so the identity is unknown, and an empty one matches nothing.
        _ => Some(Point { id: NO_SNAPSHOT, save: Some(Hashes { content: last_synced.to_string(), identity: String::new() }) }),
    }
}

// ---------------------------------------------------------------------------------------------
// What this module needs to know about a game, from the server and from this machine
// ---------------------------------------------------------------------------------------------

/// The ROM file a game's channels key to: the file this app launches when RomM lists it among the game
/// files, else the one RomM keys the game's slots by: a disc set's loader (`.cue`...) before its
/// tracks, then the first game file by name (`legacy.sync_file` on the server).
fn channel_file<'f>(files: &'f [RomFile], launched: Option<&str>) -> Option<&'f RomFile> {
    let games = |f: &&RomFile| f.category.as_deref().is_none_or(|c| c.eq_ignore_ascii_case("game")) && !f.missing_from_fs;
    if let Some(name) = launched {
        if let Some(f) = files.iter().filter(games).find(|f| f.file_name == name) {
            return Some(f);
        }
    }
    let mut candidates: Vec<&RomFile> = files.iter().filter(games).collect();
    candidates.sort_by_key(|f| f.file_name.to_lowercase());
    let is_loader = |f: &&RomFile| matches!(f.file_name.rsplit('.').next().map(str::to_lowercase).as_deref(), Some("cue" | "gdi" | "ccd" | "mds" | "toc"));
    candidates.iter().copied().find(is_loader).or_else(|| candidates.first().copied())
}

/// How a save that is not a Sigil unit travels: a plain file is one; a zip is flat when no member
/// sits in a folder, else a folder's worth. Argosy sorts native saves the same way.
fn native_shape(bytes: &[u8]) -> &'static str {
    if !saves::is_zip(bytes) {
        return "SINGLE";
    }
    let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else { return "SINGLE" };
    let in_folder = (0..zip.len()).any(|i| zip.by_index_raw(i).is_ok_and(|f| saves::entry_label(&f).trim_end_matches('/').contains('/')));
    if in_folder { "FOLDER" } else { "MULTI" }
}

/// The hashes of a save on the server, when it names them.
fn save_hashes(s: &SnapshotSave) -> Option<Hashes> {
    let content = s.content_hash.clone().filter(|h| !h.is_empty())?;
    Some(Hashes { identity: s.identity_hash.clone().filter(|h| !h.is_empty()).unwrap_or_else(|| content.clone()), content })
}

fn local_hashes(l: &LocalUnit) -> Hashes {
    match &l.kind {
        LocalKind::Unit(u) => Hashes { content: u.content_hash.clone(), identity: u.identity_hash.clone() },
        _ => Hashes { content: l.hash.clone(), identity: l.hash.clone() },
    }
}

/// How RomM knows a plain (not zip) save that an older engine uploaded as a hardcore one: with
/// Argosy's marker after it, and, since RomM hashes anything but a zip whole, under the hash of that.
/// A zip is hashed by its entries, marker or not, so it has no such form.
fn marked_form(l: &LocalUnit) -> Option<Hashes> {
    let data = l.bytes().ok()?;
    if saves::is_zip(&data) {
        return None;
    }
    let h = unit::content_hash(&hardcore::mark(&data));
    Some(Hashes { identity: h.clone(), content: h })
}

/// The hashes to measure a local save by, and whether it is known only in its marked form.
///
/// The emulator's file never carries the marker, but the server's copy of a hardcore save an older
/// engine uploaded does, and that copy is hashed with it. Measured plainly such a save looks changed
/// (and would go up again, as a casual one, with no marker and no flag for RomM to guard). `known` is
/// every content hash that names the save on the server or on record here: when the plain hash is
/// one the marked form is not looked for, and when only the marked form is, that is the save, and
/// proof that it is a hardcore one.
pub(crate) fn known_form(plain: Hashes, marked: impl FnOnce() -> Option<Hashes>, known: &[&str]) -> (Hashes, bool) {
    if known.contains(&plain.content.as_str()) {
        return (plain, false);
    }
    match marked() {
        Some(m) if known.contains(&m.content.as_str()) => (m, true),
        _ => (plain, false),
    }
}

fn newest_first(c: &Channel) -> String {
    c.current.as_ref().and_then(|s| s.created_at.clone()).unwrap_or_default()
}

/// The channel as the server and this device have it.
struct View {
    file_id: i64,
    channel_id: String,
    /// The server has no such channel yet; the first push creates it, naming it.
    is_new: bool,
    current: Option<Snapshot>,
    held: Option<ChannelRecord>,
}

/// Why a game can't just be pushed or pulled.
#[derive(Clone, Copy)]
enum Why {
    /// The channel moved on while this device did too.
    Moved,
    /// A casual save would replace a hardcore one.
    Hardcore,
}

impl Why {
    fn text(self) -> &'static str {
        match self {
            Why::Moved => "the server's save moved on from the one this device built on",
            Why::Hardcore => "the server's save is a hardcore one, and this one wasn't played in hardcore",
        }
    }
}

enum Pushed {
    Done(Box<Snapshot>),
    /// Refused because the channel moved on; the server kept the push as a branch.
    Moved(Box<PushConflict>),
    /// Refused because it would replace a hardcore save with a casual one.
    Downgrade,
}

impl<'a> Syncer<'a> {
    /// `sync_rom` against a server with snapshots: the same game, the same save, and the play time
    /// reported on its own. See the module docs for why this is not the `negotiate` flow.
    pub(super) async fn sync_rom_snapshots(&mut self, rom: &LocalRom, env: &EmuEnv, play: Option<PlaySession>) -> Result<SyncReport> {
        let id = identity_for(self.state, rom);
        let plan = route::plan(env, rom, &id);
        let mut report = SyncReport::default();

        if let Some(p @ Plan { kind: Kind::Unit(route), .. }) = &plan {
            if unit::has_unowned_saves(&route.platform) {
                self.claim_unowned(rom, &id, p, route, play.is_some());
            }
        }
        if let Some(p) = &plan {
            // A save Sigil refuses to read is no save to sync, and no reason to lose the play time.
            match gather_from_state(self.state, rom, &id, p) {
                Err(e) => report.failed.push(format!("{}: {e}", name_base(rom, plan.as_ref()))),
                Ok(local) => {
                    let cx = Cx { rom, env, id: &id, plan: plan.as_ref(), local: local.as_ref(), session_id: 0 };
                    if let Err(e) = self.sync_channel(&cx, p, play.is_some(), &mut report).await {
                        report.failed.push(format!("{}: {e}", name_base(rom, plan.as_ref())));
                    }
                }
            }
        }
        if let Some(p) = play {
            if let Err(e) = self.client.ingest_play_sessions(self.device_id, &[p]).await {
                report.failed.push(format!("reporting play time: {e}"));
            }
        }
        Ok(report)
    }

    async fn sync_channel(&mut self, cx: &Cx<'_>, plan: &Plan, in_session: bool, report: &mut SyncReport) -> Result<()> {
        let rom_id = cx.rom.rom_id;
        let server_rom = self.client.rom(rom_id).await?;
        let launched = cx.rom.launch_path.file_name().map(|n| n.to_string_lossy().into_owned());
        let file = channel_file(&server_rom.files, launched.as_deref())
            .ok_or_else(|| Error::msg("RomM lists no game file for this ROM, so its saves have no channel"))?;

        let channels = self.client.channels(&[file.id]).await?;
        let stored = self.state.channels.get(&channel_key(rom_id)).filter(|c| c.rom_file_id == file.id).cloned();
        let found = stored
            .as_ref()
            .and_then(|s| channels.iter().find(|c| c.id == s.channel_id))
            .or_else(|| channels.iter().filter(|c| c.is_own && c.label.eq_ignore_ascii_case(DEFAULT_CHANNEL)).max_by_key(|c| newest_first(c)));
        let view = View {
            file_id: file.id,
            channel_id: found.map_or_else(|| uuid::Uuid::new_v4().to_string(), |c| c.id.clone()),
            is_new: found.is_none(),
            current: found.and_then(|c| c.current.clone()),
            held: stored.filter(|s| found.is_some_and(|c| c.id == s.channel_id)),
        };

        let local = cx.local;
        let local_h = local.map(local_hashes);
        let held = view.held.as_ref().and_then(|h| {
            let content = h.held_content_hash.clone()?;
            Some(Point { id: h.held_snapshot_id?, save: Some(Hashes { identity: h.held_identity_hash.clone().unwrap_or_else(|| content.clone()), content }) })
        });
        // A held snapshot with no hashes on record is still a held snapshot.
        let held = held.or_else(|| view.held.as_ref().and_then(|h| h.held_snapshot_id).map(|id| Point { id, save: None }));
        let current = view.current.as_ref().map(|s| Point { id: s.id, save: s.save.as_ref().and_then(save_hashes) });
        let last_synced = self.state.last_synced_hash(rom_id, &plan.label).map(str::to_owned);
        // An older engine uploaded a hardcore save that is a plain file with the marker after it, and
        // RomM hashes it so; the emulator's file has no marker. Match them, or the same save would
        // look changed and go up again without the marker (see `known_form`).
        let (local_h, local_marked) = match (local, local_h) {
            (Some(l), Some(plain)) => {
                let known: Vec<&str> = [current.as_ref(), held.as_ref()]
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p.save.as_ref().map(|h| h.content.as_str()))
                    .chain(last_synced.as_deref())
                    .collect();
                let (h, marked) = known_form(plain, || marked_form(l), &known);
                (Some(h), marked)
            }
            (_, h) => (h, false),
        };
        // A game this install has synced only the older way has no channel record yet, though it knows
        // what it last exchanged: without that, its first sync would call every change here, or on the
        // server, a conflict. Once a sync writes the channel record, this is no longer asked.
        let held = held.or_else(|| {
            if self.state.channels.contains_key(&channel_key(rom_id)) {
                return None;
            }
            seed_held(last_synced.as_deref()?, current.as_ref(), local_h.as_ref())
        });

        let action = decide(held.as_ref(), current.as_ref(), local_h.as_ref());
        tracing::debug!("snapshot sync: rom {rom_id} channel {} held {:?} current {:?} -> {action:?}", view.channel_id, held.as_ref().map(|h| h.id), current.as_ref().map(|c| c.id));

        let dirty = local_h.as_ref().is_some_and(|l| held.as_ref().and_then(|h| h.save.as_ref()).is_none_or(|h| !l.same_save_as(h)));
        let hardcore = self.push_is_hardcore(rom_id, &plan.label, cx.env, &view, dirty, in_session);
        // `NO_SNAPSHOT` is no snapshot the server knows; naming it would only draw a 404 for the parent.
        let held_id = held.as_ref().map(|h| h.id).filter(|id| *id != NO_SNAPSHOT);

        match action {
            Action::Nothing => {
                if local.is_some() || current.is_some() || held.is_some() {
                    report.unchanged += 1;
                }
                self.commit_sigil(plan, local);
            }
            Action::Adopt(snapshot_id) => {
                if let Some(cur) = &view.current {
                    self.record(cx, plan, &view, cur, current.as_ref().and_then(|c| c.save.clone()), local_marked);
                }
                // Tell the server this device holds it. Nothing is lost if that fails: the next sync
                // finds the same hashes and adopts again.
                if let Err(e) = self.client.snapshot(snapshot_id, self.device_id, true).await {
                    tracing::warn!("couldn't tell the server this device holds snapshot {snapshot_id}: {e}");
                }
                self.commit_sigil(plan, local);
                report.unchanged += 1;
            }
            Action::Download(snapshot_id) => self.apply_snapshot(cx, plan, &view, snapshot_id, report).await?,
            Action::Push { expected, parent } => {
                let local = local.ok_or_else(|| Error::msg("nothing to push"))?;
                match self.push_local(cx, plan, &view, local, expected, parent, hardcore, false).await? {
                    Pushed::Done(snap) => self.pushed(cx, plan, &view, local, &snap, report),
                    // The channel moved between listing it and writing to it; the server kept the push
                    // as a branch, so nothing is lost and only the choice of which save to play is left.
                    Pushed::Moved(c) => self.settle(cx, plan, &view, c.current.as_ref().or(view.current.as_ref()), local, held_id, hardcore, Why::Moved, true, report).await?,
                    Pushed::Downgrade => self.settle(cx, plan, &view, view.current.as_ref(), local, held_id, hardcore, Why::Hardcore, false, report).await?,
                }
            }
            Action::Conflict(_) => {
                let local = local.ok_or_else(|| Error::msg("a conflict needs a local save"))?;
                self.settle(cx, plan, &view, view.current.as_ref(), local, held_id, hardcore, Why::Moved, false, report).await?;
            }
        }
        Ok(())
    }

    /// Whether what this game pushes counts as hardcore: the session that made the changes was one, as
    /// the emulator's settings say when it ends (see `hardcore`). Changes found outside a session count
    /// only if the session that made them was, as remembered; a casual save is never passed off as
    /// hardcore. Nothing changed: the held save's own kind.
    ///
    /// What was made of the save before is remembered by whichever engine last exchanged it
    /// (`State::hardcore_memory`), so it counts here even on the first snapshot sync of a game the
    /// older engine had been syncing.
    fn push_is_hardcore(&mut self, rom_id: i64, emulator: &str, env: &EmuEnv, view: &View, dirty: bool, in_session: bool) -> bool {
        let key = channel_key(rom_id);
        let (was, pending) = self.state.hardcore_memory(rom_id, emulator);
        if !dirty {
            return was;
        }
        if !in_session {
            return pending;
        }
        let hardcore = hardcore::session_is_hardcore(env);
        // Remembered at once, so that a push that fails doesn't lose it. A casual session adds nothing.
        if hardcore || self.state.channels.contains_key(&key) {
            self.state.channels.entry(key).or_insert_with(|| ChannelRecord { channel_id: view.channel_id.clone(), rom_file_id: view.file_id, ..Default::default() });
        }
        self.state.note_pending(rom_id, emulator, hardcore);
        hardcore
    }

    /// Writes this device's save into the channel as a new snapshot.
    #[allow(clippy::too_many_arguments)]
    async fn push_local(
        &mut self,
        cx: &Cx<'_>,
        plan: &Plan,
        view: &View,
        local: &LocalUnit,
        expected: Option<i64>,
        parent: Option<i64>,
        hardcore: bool,
        approve_downgrade: bool,
    ) -> Result<Pushed> {
        let bytes = local.bytes()?;
        let (shape, format) = match &local.kind {
            LocalKind::Unit(u) => (
                match u.shape {
                    unit::Shape::Single => "SINGLE",
                    unit::Shape::Multi => "MULTI",
                    unit::Shape::Folder => "FOLDER",
                },
                "neutral",
            ),
            _ => (native_shape(&bytes), "native"),
        };
        let content = local_hashes(local).content;
        let (emulator, core) = if cx.env.profile.id == "retroarch" { ("retroarch".to_string(), cx.env.core.clone()) } else { (plan.label.clone(), None) };
        let mut manifest = Manifest {
            rom_file_id: view.file_id,
            channel_id: view.channel_id.clone(),
            label: view.is_new.then(|| DEFAULT_CHANNEL.to_string()),
            expected_current_id: expected,
            parent_snapshot_id: parent,
            save: ManifestSave { hash: content.clone(), shape: shape.into(), format: format.into() },
            is_hardcore: hardcore,
            approve_hardcore_downgrade: approve_downgrade,
            emulator: Some(emulator.chars().take(50).collect()),
            emulator_version: None,
            core: core.map(|c| c.chars().take(50).collect()),
            core_version: None,
        };
        // The server keeps what it has been sent: a push whose save it already holds carries only the manifest.
        let mut send_save = view.current.as_ref().and_then(|s| s.save.as_ref()).and_then(|s| s.content_hash.as_deref()) != Some(content.as_str());
        for _ in 0..4 {
            let part = send_save.then(|| (local.file_name.clone(), bytes.clone()));
            match self.client.push_snapshot(self.device_id, &manifest, part).await? {
                PushOutcome::Written { snapshot, .. } => return Ok(Pushed::Done(snapshot)),
                PushOutcome::Conflict(c) => return Ok(Pushed::Moved(c)),
                PushOutcome::HardcoreDowngrade => return Ok(Pushed::Downgrade),
                // It asked for the save after all (its copy of it was pruned, say).
                PushOutcome::Missing(parts) if !send_save && parts.iter().any(|p| p == "save") => send_save = true,
                PushOutcome::Missing(parts) => return Err(Error::msg(format!("the server asked for content this push can't send: {}", parts.join(", ")))),
                // The snapshot this builds on is gone (pruned); the push stands without naming it.
                PushOutcome::ParentMissing if manifest.parent_snapshot_id.is_some() => manifest.parent_snapshot_id = None,
                PushOutcome::ParentMissing => return Err(Error::msg("the server doesn't know the snapshot this builds on")),
            }
        }
        Err(Error::msg("the server kept asking for more before it would take the save"))
    }

    /// A push landed: this device now holds it.
    fn pushed(&mut self, cx: &Cx<'_>, plan: &Plan, view: &View, local: &LocalUnit, snap: &Snapshot, report: &mut SyncReport) {
        self.record(cx, plan, view, snap, Some(local_hashes(local)), false);
        self.commit_sigil(plan, Some(local));
        report.uploaded.push(local.file_name.clone());
    }

    /// Takes the server's snapshot as this device's save.
    async fn apply_snapshot(&mut self, cx: &Cx<'_>, plan: &Plan, view: &View, snapshot_id: i64, report: &mut SyncReport) -> Result<()> {
        precheck(plan)?;
        // Reading it with `hold` is also what tells the server this device has it now.
        let snap = self.client.snapshot(snapshot_id, self.device_id, true).await?;
        let Some(save) = &snap.save else {
            // States only: there is no save to place, but the device has caught up.
            self.record(cx, plan, view, &snap, None, false);
            return Ok(());
        };
        let bytes = self.client.download_snapshot_save(&save.download_path).await?;
        let placed = self.place(cx, plan, &bytes)?;
        // A marker on what came down is proof it is a hardcore save, whatever the snapshot's flag says:
        // RomM never reads it, so a save an older engine uploaded as hardcore is flagged casual there.
        self.record(cx, plan, view, &snap, save_hashes(save), placed.marked);
        report.downloaded.push(placed.name);
        Ok(())
    }

    /// Both sides have a save the other lacks, or a casual save would replace a hardcore one. The
    /// policy decides, as it does for the older sync; the server's way of keeping both is a branch.
    /// `branched`: the server already stored this device's save as one (a push it refused did).
    #[allow(clippy::too_many_arguments)]
    async fn settle(
        &mut self,
        cx: &Cx<'_>,
        plan: &Plan,
        view: &View,
        current: Option<&Snapshot>,
        local: &LocalUnit,
        held_id: Option<i64>,
        hardcore: bool,
        why: Why,
        branched: bool,
        report: &mut SyncReport,
    ) -> Result<()> {
        let Some(cur) = current else { return Err(Error::msg("the channel has no current save to settle against")) };
        // Taking the server's save writes to xemu's disk; find out it can be before archiving anything.
        if matches!(self.policy, ConflictPolicy::KeepBoth | ConflictPolicy::ServerWins) {
            precheck(plan)?;
        }
        match self.policy {
            ConflictPolicy::Ask => {
                report.conflicts.push(PendingConflict {
                    rom_id: cx.rom.rom_id,
                    file_name: local.file_name.clone(),
                    server_updated_at: cur.created_at.clone(),
                    reason: Some(why.text().to_string()),
                });
                Ok(())
            }
            ConflictPolicy::ServerWins => self.apply_snapshot(cx, plan, view, cur.id, report).await,
            ConflictPolicy::KeepBoth => {
                if !branched {
                    // A push that doesn't build on the channel's current is kept as a branch, and
                    // doesn't need the hardcore check. Built on what this device held when that is
                    // not the current, else on an empty channel, which a channel with a current isn't.
                    let expected = held_id.filter(|h| *h != cur.id);
                    match self.push_local(cx, plan, view, local, expected, held_id, hardcore, false).await? {
                        Pushed::Done(snap) => {
                            // The channel had not moved after all: nothing to keep apart.
                            self.pushed(cx, plan, view, local, &snap, report);
                            return Ok(());
                        }
                        Pushed::Moved(_) => {}
                        Pushed::Downgrade => return Err(Error::msg("the server refused to keep this device's save as a branch")),
                    }
                }
                let ext = Path::new(&local.file_name).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
                report.archived.push(upload_name(&saves::conflict_name(&name_base(cx.rom, cx.plan)), &ext));
                self.apply_snapshot(cx, plan, view, cur.id, report).await
            }
            ConflictPolicy::DeviceWins => {
                // On top of the current, and approving the downgrade: this is the one choice that says so.
                match self.push_local(cx, plan, view, local, Some(cur.id), held_id, hardcore, true).await? {
                    Pushed::Done(snap) => {
                        self.pushed(cx, plan, view, local, &snap, report);
                        Ok(())
                    }
                    Pushed::Moved(c) => {
                        report.conflicts.push(PendingConflict {
                            rom_id: cx.rom.rom_id,
                            file_name: local.file_name.clone(),
                            server_updated_at: c.current.and_then(|s| s.created_at),
                            reason: Some(Why::Moved.text().to_string()),
                        });
                        Ok(())
                    }
                    Pushed::Downgrade => Err(Error::msg("the server refused the downgrade it was asked to approve")),
                }
            }
        }
    }

    /// What this device holds of the channel now, and the per-save record the saves list reads (which
    /// the older engine reads too, so it is told whether the save is a hardcore one: `marked` is proof
    /// that it is, besides the snapshot's own flag).
    fn record(&mut self, cx: &Cx<'_>, plan: &Plan, view: &View, snap: &Snapshot, hashes: Option<Hashes>, marked: bool) {
        let rom_id = cx.rom.rom_id;
        let hardcore = snap.is_hardcore || marked;
        self.state.channels.insert(
            channel_key(rom_id),
            ChannelRecord {
                channel_id: view.channel_id.clone(),
                rom_file_id: view.file_id,
                held_snapshot_id: Some(snap.id),
                held_content_hash: hashes.as_ref().map(|h| h.content.clone()),
                held_identity_hash: hashes.as_ref().map(|h| h.identity.clone()),
                held_hardcore: hardcore,
                pending_hardcore: false,
            },
        );
        if let Some(h) = &hashes {
            self.state.save_record(rom_id, &plan.label).mark_synced(&h.content, snap.save.as_ref().map(|s| s.id), snap.created_at.clone());
            self.state.note_exchanged(rom_id, &plan.label, hardcore);
        }
    }

    /// Sigil's state moves on only once a save has reached the server or been matched with it (see
    /// `record_synced`), or a failed push would lose its change marker.
    fn commit_sigil(&mut self, plan: &Plan, local: Option<&LocalUnit>) {
        if let (Kind::Unit(route), Some(LocalUnit { kind: LocalKind::Unit(u), .. })) = (&plan.kind, local) {
            self.state.set_sigil_state(&route.state_key(), &u.state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(c: &str) -> Hashes {
        Hashes { content: c.into(), identity: format!("id-{c}") }
    }
    fn pt(id: i64, c: &str) -> Point {
        Point { id, save: Some(h(c)) }
    }

    // -- the decision table (Argosy's SnapshotDecision) ----------------------------------------

    #[test]
    fn an_empty_channel_takes_the_local_save_and_nothing_else() {
        assert_eq!(decide(None, None, Some(&h("a"))), Action::Push { expected: None, parent: None });
        assert_eq!(decide(None, None, None), Action::Nothing);
        // A channel emptied on the server is refilled from a device that held a snapshot of it.
        assert_eq!(decide(Some(&pt(3, "a")), None, Some(&h("a"))), Action::Push { expected: None, parent: Some(3) });
    }

    #[test]
    fn a_device_that_holds_nothing_downloads_adopts_or_conflicts() {
        let cur = pt(5, "x");
        assert_eq!(decide(None, Some(&cur), None), Action::Download(5));
        assert_eq!(decide(None, Some(&cur), Some(&h("x"))), Action::Adopt(5), "same save: no transfer");
        assert_eq!(decide(None, Some(&cur), Some(&h("y"))), Action::Conflict(5), "a different save on each side");
    }

    #[test]
    fn a_save_found_equal_by_identity_alone_is_the_same_save() {
        // Packed again, so the bytes differ; the saves are the same.
        let cur = Point { id: 5, save: Some(Hashes { content: "c1".into(), identity: "same".into() }) };
        let local = Hashes { content: "c2".into(), identity: "same".into() };
        assert_eq!(decide(None, Some(&cur), Some(&local)), Action::Adopt(5));
    }

    #[test]
    fn an_empty_identity_matches_nothing() {
        let a = Hashes { content: "c1".into(), identity: String::new() };
        let b = Hashes { content: "c2".into(), identity: String::new() };
        assert!(!a.same_save_as(&b));
    }

    // -- a device that has only synced this game the older way ----------------------------------

    #[test]
    fn nothing_is_inferred_without_a_last_synced_save_or_a_current() {
        let cur = pt(5, "x");
        assert_eq!(seed_held("", Some(&cur), Some(&h("y"))), None, "the older engine never exchanged anything");
        assert_eq!(seed_held("x", None, Some(&h("y"))), None, "an empty channel has nothing to compare with");
    }

    #[test]
    fn a_local_save_that_already_is_the_current_is_left_to_be_adopted() {
        // Even though the server has moved on from what was last synced: the two sides agree.
        let cur = pt(6, "new");
        assert_eq!(seed_held("old", Some(&cur), Some(&h("new"))), None);
        assert_eq!(decide(None, Some(&cur), Some(&h("new"))), Action::Adopt(6));
    }

    #[test]
    fn a_channel_still_on_the_last_synced_save_is_held_as_the_current() {
        let cur = pt(5, "old");
        assert_eq!(seed_held("old", Some(&cur), Some(&h("mine"))), Some(cur.clone()));
        // Its own hashes, including the identity the older engine never kept.
        assert_eq!(seed_held("old", Some(&cur), None), Some(cur));
    }

    #[test]
    fn a_channel_that_moved_on_is_held_as_an_earlier_snapshot_that_cant_be_named() {
        let held = seed_held("old", Some(&pt(6, "new")), Some(&h("mine"))).unwrap();
        assert_eq!(held.id, NO_SNAPSHOT);
        let kept = held.save.unwrap();
        assert_eq!((kept.content.as_str(), kept.identity.as_str()), ("old", ""));
        // A current with no save of its own (only states) moved on as well.
        let states_only = Point { id: 7, save: None };
        assert_eq!(seed_held("old", Some(&states_only), Some(&h("mine"))).unwrap().id, NO_SNAPSHOT);
    }

    #[test]
    fn the_stand_in_id_is_never_a_real_snapshot() {
        // Real ids are 1 and up, so a seeded device is never taken to hold the current.
        assert_eq!(NO_SNAPSHOT, 0);
        let held = seed_held("old", Some(&pt(1, "new")), Some(&h("mine"))).unwrap();
        assert_ne!(held.id, 1);
    }

    #[test]
    fn what_the_seed_changes_in_the_first_snapshot_sync_of_an_upgraded_device() {
        let seeded = |last: &str, cur: &Point, local: Option<&Hashes>| seed_held(last, Some(cur), local);

        // The channel is as it was last synced. Changed here: push it on top (not a conflict). Not changed:
        // the two sides are the same save, so it is adopted (no transfer, and the device now holds it).
        let cur = pt(5, "old");
        let mine = h("mine");
        assert_eq!(decide(seeded("old", &cur, Some(&mine)).as_ref(), Some(&cur), Some(&mine)), Action::Push { expected: Some(5), parent: Some(5) });
        assert_eq!(decide(seeded("old", &cur, Some(&h("old"))).as_ref(), Some(&cur), Some(&h("old"))), Action::Adopt(5));

        // The channel has moved on. Not changed here: follow it. Changed here too: a real conflict.
        let cur = pt(6, "new");
        assert_eq!(decide(seeded("old", &cur, Some(&h("old"))).as_ref(), Some(&cur), Some(&h("old"))), Action::Download(6));
        assert_eq!(decide(seeded("old", &cur, Some(&mine)).as_ref(), Some(&cur), Some(&mine)), Action::Conflict(6));
        // No local save at all: take the channel's.
        assert_eq!(decide(seeded("old", &cur, None).as_ref(), Some(&cur), None), Action::Download(6));

        // Without the seed, every one of those differences was a conflict (or an adoption).
        assert_eq!(decide(None, Some(&pt(5, "old")), Some(&mine)), Action::Conflict(5));
        assert_eq!(decide(None, Some(&cur), Some(&h("old"))), Action::Conflict(6));
    }

    // -- a plain hardcore save an older engine uploaded with Argosy's marker -----------------------

    fn hs(c: &str) -> Hashes {
        Hashes { content: c.into(), identity: c.into() }
    }

    fn local_bytes(data: &[u8]) -> LocalUnit {
        LocalUnit {
            emulator: "x".into(),
            slot: "default".into(),
            file_name: "g.srm".into(),
            kind: LocalKind::Bytes(data.to_vec()),
            size: data.len() as u64,
            modified: std::time::SystemTime::UNIX_EPOCH,
            hash: unit::content_hash(data),
            changed: false,
            marked: false,
        }
    }

    #[test]
    fn a_save_known_by_its_own_hash_is_measured_by_it_and_the_marked_form_is_never_made() {
        let (h, marked) = known_form(hs("plain"), || panic!("the marked form isn't needed"), &["plain", "other"]);
        assert_eq!((h.content.as_str(), marked), ("plain", false));
    }

    #[test]
    fn a_save_known_only_with_its_marker_is_that_save_and_a_hardcore_one() {
        let (h, marked) = known_form(hs("plain"), || Some(hs("with-marker")), &["with-marker"]);
        assert_eq!((h.content.as_str(), marked), ("with-marker", true));
    }

    #[test]
    fn a_save_known_by_neither_form_stays_as_it_is() {
        let (h, marked) = known_form(hs("plain"), || Some(hs("with-marker")), &["something-else"]);
        assert_eq!((h.content.as_str(), marked), ("plain", false));
        // A zip has no marked form, so there is nothing to match.
        let (h, marked) = known_form(hs("plain"), || None, &["with-marker"]);
        assert_eq!((h.content.as_str(), marked), ("plain", false));
    }

    #[test]
    fn the_marked_form_is_the_hash_romm_gives_the_save_with_the_marker_after_it() {
        let l = local_bytes(b"SAVE");
        let m = marked_form(&l).unwrap();
        assert_eq!(m.content, unit::content_hash(&hardcore::mark(b"SAVE")));
        assert_ne!(m.content, l.hash, "the marker is part of what a plain file's hash covers");
        assert_eq!(m.identity, m.content);
    }

    #[test]
    fn a_zip_has_no_marked_form() {
        // RomM hashes a zip by its entries, so a marker after it changes nothing.
        let z = zip_of(&["a.sav", "b.rtc"]);
        assert!(marked_form(&local_bytes(&z)).is_none());
        assert_eq!(unit::content_hash(&hardcore::mark(&z)), unit::content_hash(&z));
    }

    #[test]
    fn holding_the_current_pushes_only_what_changed() {
        let held = pt(5, "a");
        let cur = pt(5, "a");
        assert_eq!(decide(Some(&held), Some(&cur), Some(&h("a"))), Action::Nothing);
        assert_eq!(decide(Some(&held), Some(&cur), Some(&h("b"))), Action::Push { expected: Some(5), parent: Some(5) });
    }

    #[test]
    fn a_cleared_save_folder_is_refilled_not_pushed_as_empty() {
        assert_eq!(decide(Some(&pt(5, "a")), Some(&pt(5, "a")), None), Action::Download(5));
        assert_eq!(decide(Some(&pt(4, "a")), Some(&pt(5, "b")), None), Action::Download(5));
    }

    #[test]
    fn a_channel_that_moved_on_is_downloaded_unless_this_device_changed_too() {
        let held = pt(4, "a");
        let cur = pt(5, "b");
        assert_eq!(decide(Some(&held), Some(&cur), Some(&h("a"))), Action::Download(5), "unchanged here: follow the server");
        assert_eq!(decide(Some(&held), Some(&cur), Some(&h("c"))), Action::Conflict(5), "changed here too");
    }

    #[test]
    fn a_held_snapshot_with_no_hashes_on_record_counts_the_local_save_as_changed() {
        let held = Point { id: 5, save: None };
        assert_eq!(decide(Some(&held), Some(&pt(5, "a")), Some(&h("a"))), Action::Push { expected: Some(5), parent: Some(5) });
    }

    // -- which ROM file the channel keys to ---------------------------------------------------

    fn file(id: i64, name: &str, category: Option<&str>, missing: bool) -> RomFile {
        RomFile { id, rom_id: 1, file_name: name.into(), file_size_bytes: 0, md5_hash: None, is_top_level: true, category: category.map(Into::into), missing_from_fs: missing }
    }

    #[test]
    fn the_file_launched_is_the_one_when_romm_lists_it() {
        let files = [file(1, "Game (Disc 1).cue", None, false), file(2, "Game (Disc 1).bin", None, false)];
        assert_eq!(channel_file(&files, Some("Game (Disc 1).bin")).map(|f| f.id), Some(2));
    }

    #[test]
    fn without_a_match_a_disc_sets_loader_comes_before_its_tracks() {
        let files = [file(1, "a track.bin", None, false), file(2, "Game.cue", None, false), file(3, "Another.bin", None, false)];
        assert_eq!(channel_file(&files, None).map(|f| f.id), Some(2));
        assert_eq!(channel_file(&files, Some("not a file")).map(|f| f.id), Some(2));
    }

    #[test]
    fn without_a_loader_the_first_game_file_by_name_is_it() {
        let files = [file(1, "b.iso", None, false), file(2, "A.iso", None, false)];
        assert_eq!(channel_file(&files, None).map(|f| f.id), Some(2));
    }

    #[test]
    fn only_game_files_still_on_disk_can_carry_a_channel() {
        let files = [file(1, "Update.cue", Some("update"), false), file(2, "Gone.iso", Some("game"), true), file(3, "Game.iso", Some("GAME"), false), file(4, "Manual.pdf", Some("manual"), false)];
        assert_eq!(channel_file(&files, None).map(|f| f.id), Some(3));
        assert_eq!(channel_file(&files, Some("Gone.iso")).map(|f| f.id), Some(3), "a launched file the server lost doesn't count");
        assert!(channel_file(&[file(1, "x.pdf", Some("manual"), false)], None).is_none());
        assert!(channel_file(&[], None).is_none());
    }

    // -- how a save that isn't a Sigil unit is described --------------------------------------

    fn zip_of(names: &[&str]) -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            for n in names {
                w.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
                w.write_all(b"x").unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn a_plain_file_is_single_a_flat_zip_multi_and_a_zip_of_folders_a_folder() {
        assert_eq!(native_shape(b"SRAM"), "SINGLE");
        assert_eq!(native_shape(&zip_of(&["a.bin", "b.bin"])), "MULTI");
        assert_eq!(native_shape(&zip_of(&["SAVE1/a.bin", "SAVE1/b.bin"])), "FOLDER");
        assert_eq!(native_shape(&zip_of(&["a.bin", "SAVE1/b.bin"])), "FOLDER");
    }

    #[test]
    fn what_the_server_names_of_a_save_is_what_decides_on() {
        let s = |c: Option<&str>, i: Option<&str>| SnapshotSave { id: 1, file_name: String::new(), file_size_bytes: 0, content_hash: c.map(Into::into), identity_hash: i.map(Into::into), shape: None, format: None, download_path: String::new() };
        assert_eq!(save_hashes(&s(Some("c"), Some("i"))), Some(Hashes { content: "c".into(), identity: "i".into() }));
        assert_eq!(save_hashes(&s(Some("c"), None)), Some(Hashes { content: "c".into(), identity: "c".into() }), "no identity: the content stands for it");
        assert_eq!(save_hashes(&s(None, Some("i"))), None, "no content hash, nothing to compare");
    }
}
