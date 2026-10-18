//! Tauri backend. Every command is a thin wrapper over `romm-core`; the long-running ones
//! (install, play, sync) run as background jobs and report through `romm://…` events.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use base64::Engine;
use romm_core::achievements::{self, AchievementsView};
use romm_core::api::types::{DeviceRegistration, Me, Platform, Rom};
use romm_core::api::{RommClient, CLIENT_SLUG};
use romm_core::config::{default_device_name, hostname, load_token, store_token, AppDirs, Config};
use romm_core::library::{self, BiosStatus};
use romm_core::resolve::Resolver;
use romm_core::state::{LocalRom, State};
use romm_core::states::{self, StateOptions, StateReport, StateView};
use romm_core::sync::{saves_overview, ConflictPolicy, SaveView, SyncReport, Syncer};
use romm_core::{browse, platforms as plat, profiles, retroarch, session, versions};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State as TauriState};
use tokio::sync::{Mutex, RwLock};

type CmdResult<T> = Result<T, String>;

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Scopes a token needs for everything the app does.
const REQUIRED_SCOPES: &[&str] = &[
    "me.read", "platforms.read", "roms.read", "firmware.read", "assets.read", "assets.write", "devices.read", "devices.write",
];

#[derive(Clone)]
pub struct AppState {
    dirs: AppDirs,
    cfg: Arc<RwLock<Config>>,
    /// Held only for the short sync phases, never while an emulator runs.
    db: Arc<Mutex<State>>,
    client: Arc<RwLock<Option<RommClient>>>,
    busy: Arc<StdMutex<HashSet<i64>>>,
    /// `GET /api/users/me`, kept briefly: it carries the user's whole RetroAchievements progression
    /// (megabytes for a heavy RA user) and every game panel needs it.
    ra_me: Arc<Mutex<Option<(Instant, Me)>>>,
    /// The whole library, one entry per game, fetched once (see `AppState::library`). The lock is
    /// held while it loads, so everyone who needs the library waits for that one fetch.
    library: Arc<Mutex<Option<LibraryCache>>>,
}

struct LibraryCache {
    at: Instant,
    roms: Arc<Vec<Rom>>,
}

/// What `AppState::library` hands back.
struct Loaded {
    roms: Arc<Vec<Rom>>,
    /// Set when a refresh failed and `roms` is the previous copy: better than nothing, but the
    /// caller may want to say so.
    refresh_error: Option<String>,
}

/// Marks a game as "something is happening to it" so the UI can't start two jobs at once.
struct Busy {
    set: Arc<StdMutex<HashSet<i64>>>,
    id: i64,
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.id);
    }
}

/// Job key for "sync everything".
const SYNC_ALL: i64 = -1;
/// Job key for "refresh RetroAchievements" (per user, not per game).
const RA_REFRESH: i64 = -2;
/// How long a fetched profile is reused when opening game panels.
const RA_ME_TTL: Duration = Duration::from_secs(5 * 60);
/// How old the library may be when a list is opened from the top (switching platform, searching,
/// going back to it). Scrolling further down a list never refetches: the order must not move.
const LIBRARY_TTL: Duration = Duration::from_secs(10 * 60);

impl AppState {
    fn busy(&self, id: i64) -> CmdResult<Busy> {
        let mut set = self.busy.lock().unwrap();
        if !set.insert(id) {
            return Err("something is already running for this game".into());
        }
        Ok(Busy { set: self.busy.clone(), id })
    }

    async fn client(&self) -> CmdResult<RommClient> {
        self.client.read().await.clone().ok_or_else(|| "not connected to a RomM server".to_string())
    }

    /// The signed-in user, from the short-lived cache unless `force`.
    async fn me(&self, client: &RommClient, force: bool) -> CmdResult<Me> {
        if !force {
            if let Some((at, me)) = &*self.ra_me.lock().await {
                if at.elapsed() < RA_ME_TTL {
                    return Ok(me.clone());
                }
            }
        }
        let me = client.me().await.map_err(s)?;
        *self.ra_me.lock().await = Some((Instant::now(), me.clone()));
        Ok(me)
    }

    /// Drop everything RetroAchievements-related that belongs to the current account
    /// (on login and logout, so one user's progress is never shown to the next).
    async fn forget_ra(&self) {
        *self.ra_me.lock().await = None;
        let _ = std::fs::remove_dir_all(self.dirs.data_dir.join("achievements"));
    }

    /// Forget the library (on login and logout: it belongs to one account's server).
    async fn forget_library(&self) {
        *self.library.lock().await = None;
    }

    /// The whole library, grouped one entry per game, fetched on first use and then reused for
    /// `max_age` (always, when None; never, when zero, which is how a manual refresh forces it).
    /// Concurrent callers share one fetch. Progress goes out as `romm://library` events.
    ///
    /// If a refresh fails while an older copy exists, the older copy is returned with the error.
    async fn library(&self, app: &AppHandle, max_age: Option<Duration>) -> CmdResult<Loaded> {
        let mut slot = self.library.lock().await;
        if let Some(c) = &*slot {
            if max_age.is_none_or(|m| c.at.elapsed() < m) {
                return Ok(Loaded { roms: c.roms.clone(), refresh_error: None });
            }
        }
        let client = self.client().await?;
        let a = app.clone();
        let mut progress = move |loaded: usize, total: Option<i64>| {
            let _ = a.emit("romm://library", LibraryEvent { phase: "loading", loaded, total, error: None });
        };
        let _ = app.emit("romm://library", LibraryEvent { phase: "loading", loaded: 0, total: None, error: None });
        match client.library(&mut progress).await {
            Ok(roms) => {
                let n = roms.len();
                let roms = Arc::new(roms);
                *slot = Some(LibraryCache { at: Instant::now(), roms: roms.clone() });
                let _ = app.emit("romm://library", LibraryEvent { phase: "finished", loaded: n, total: Some(n as i64), error: None });
                Ok(Loaded { roms, refresh_error: None })
            }
            Err(e) => {
                let e = s(e);
                let _ = app.emit("romm://library", LibraryEvent { phase: "failed", loaded: 0, total: None, error: Some(e.clone()) });
                match &*slot {
                    Some(c) => Ok(Loaded { roms: c.roms.clone(), refresh_error: Some(e) }),
                    None => Err(e),
                }
            }
        }
    }

    async fn update_cfg(&self, f: impl FnOnce(&mut Config)) -> CmdResult<()> {
        let mut cfg = self.cfg.write().await;
        f(&mut cfg);
        cfg.save(&self.dirs).map_err(s)
    }

    async fn device_id(&self, client: &RommClient) -> CmdResult<String> {
        if let Some(id) = self.cfg.read().await.device_id.clone() {
            return Ok(id);
        }
        let name = self.cfg.read().await.device_name.clone().unwrap_or_else(default_device_name);
        let d = client
            .register_device(&DeviceRegistration {
                name,
                platform: std::env::consts::OS.to_string(),
                client: CLIENT_SLUG.to_string(),
                client_version: env!("CARGO_PKG_VERSION").to_string(),
                hostname: hostname(),
                sync_mode: "api".to_string(),
            })
            .await
            .map_err(|e| format!("registering this device with RomM failed (does the token have the devices.write scope?): {e}"))?;
        self.update_cfg(|c| c.device_id = Some(d.device_id.clone())).await?;
        Ok(d.device_id)
    }

    async fn state_opts(&self) -> Option<StateOptions> {
        let c = self.cfg.read().await;
        c.sync_states.then(|| StateOptions { max_bytes: c.max_state_mb * 1024 * 1024 })
    }
}

fn build_client(dirs: &AppDirs, cfg: &Config) -> Option<RommClient> {
    let url = cfg.server_url.as_deref()?;
    let token = load_token(dirs)?;
    RommClient::new(url, Some(token), cfg.accept_invalid_certs).ok()
}

// ----------------------------------------------------------------------------- DTOs

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusDto {
    logged_in: bool,
    server_url: Option<String>,
    device_name: Option<String>,
    version: &'static str,
    warnings: Vec<String>,
}

async fn status_of(st: &AppState, warnings: Vec<String>) -> StatusDto {
    let cfg = st.cfg.read().await;
    StatusDto {
        logged_in: st.client.read().await.is_some(),
        server_url: cfg.server_url.clone(),
        device_name: cfg.device_name.clone(),
        version: env!("CARGO_PKG_VERSION"),
        warnings,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginArgs {
    server: String,
    code: Option<String>,
    token: Option<String>,
    insecure: bool,
    device_name: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlatformDto {
    id: i64,
    slug: String,
    name: String,
    rom_count: i64,
    firmware_count: usize,
    emulator: Option<String>,
    emulator_name: Option<String>,
    emulator_installed: bool,
    /// Everything that can play this platform (standalone emulators, RetroArch cores).
    choices: Vec<EmulatorChoiceDto>,
    /// The saved choice's `value`, or "" for Automatic.
    selected: String,
    /// What Automatic resolves to; None when nothing can play the platform.
    automatic_label: Option<String>,
    /// A saved emulator that can't play this platform and is being ignored.
    ignored_override: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EmulatorChoiceDto {
    value: String,
    label: String,
    installed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GameDto {
    id: i64,
    name: String,
    platform_id: i64,
    platform_slug: String,
    installed: bool,
    size_bytes: u64,
    multi_file: bool,
    cover_path: Option<String>,
    /// Every copy of this game (regions, revisions...) when there is more than one, the primary
    /// one first. `id` above is the primary. Empty when there is nothing to choose.
    versions: Vec<VersionDto>,
    /// Set on the Installed list when two installed entries are the same game, to tell them apart.
    version_label: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionDto {
    /// The ROM id of this copy: install, play and sync it like any other game.
    id: i64,
    label: String,
    /// The copy RomM chose to represent the game.
    is_default: bool,
    installed: bool,
}

/// Progress of loading the whole library (`romm://library`).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LibraryEvent {
    /// "loading" | "finished" | "failed"
    phase: &'static str,
    loaded: usize,
    total: Option<i64>,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GamesPage {
    items: Vec<GameDto>,
    total: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GamesArgs {
    platform_id: Option<i64>,
    search: Option<String>,
    offset: i64,
    limit: i64,
    #[serde(default)]
    installed_only: bool,
}

fn game_dto(r: &Rom, db: &State) -> GameDto {
    let versions: Vec<VersionDto> = versions::versions(r)
        .into_iter()
        .map(|v| VersionDto { installed: db.roms.contains_key(&v.id), id: v.id, label: v.label, is_default: v.is_default })
        .collect();
    // A game counts as installed when any of its copies is.
    let installed = if versions.is_empty() { db.roms.contains_key(&r.id) } else { versions.iter().any(|v| v.installed) };
    GameDto {
        id: r.id,
        name: r.title().to_string(),
        platform_id: r.platform_id,
        platform_slug: r.platform_slug.clone(),
        installed,
        size_bytes: r.fs_size_bytes,
        multi_file: r.has_multiple_files,
        cover_path: r.path_cover_small.clone(),
        versions,
        version_label: None,
    }
}

#[derive(Serialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct SyncSummary {
    uploaded: Vec<String>,
    downloaded: Vec<String>,
    archived: Vec<String>,
    conflicts: Vec<String>,
    failed: Vec<String>,
    state_uploaded: Vec<String>,
    state_downloaded: Vec<String>,
    state_archived: Vec<String>,
    state_conflicts: Vec<String>,
    state_skipped: Vec<String>,
    state_failed: Vec<String>,
}

impl SyncSummary {
    fn add_saves(&mut self, r: &SyncReport) {
        self.uploaded.extend(r.uploaded.iter().cloned());
        self.downloaded.extend(r.downloaded.iter().cloned());
        self.archived.extend(r.archived.iter().cloned());
        self.conflicts.extend(r.conflicts.iter().map(|c| c.file_name.clone()));
        self.failed.extend(r.failed.iter().cloned());
    }
    fn add_states(&mut self, r: &StateReport) {
        self.state_uploaded.extend(r.uploaded.iter().cloned());
        self.state_downloaded.extend(r.downloaded.iter().cloned());
        self.state_archived.extend(r.archived.iter().cloned());
        self.state_conflicts.extend(r.conflicts.iter().cloned());
        self.state_skipped.extend(r.skipped.iter().cloned());
        self.state_failed.extend(r.failed.iter().cloned());
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PlaySummary {
    played_secs: u64,
    exit_code: Option<i32>,
    /// Plain-language notes: offline, couldn't sync, etc.
    notes: Vec<String>,
    sync: SyncSummary,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
    rom_id: i64,
    done: u64,
    total: Option<u64>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct JobEvent<T: Serialize + Clone> {
    rom_id: i64,
    /// "started" | "syncing" | "running" | "saving" | "finished" | "failed"
    phase: &'static str,
    error: Option<String>,
    result: Option<T>,
}

fn emit_job<T: Serialize + Clone>(app: &AppHandle, topic: &str, rom_id: i64, phase: &'static str, error: Option<String>, result: Option<T>) {
    let _ = app.emit(topic, JobEvent { rom_id, phase, error, result });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EmulatorRef {
    id: String,
    name: String,
    installed: bool,
    /// Why it can't be used right now, in plain words.
    problem: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DetailsDto {
    installed: bool,
    install_path: Option<String>,
    emulator: Option<EmulatorRef>,
    saves: Vec<SaveView>,
    states: Vec<StateView>,
    /// Set when the server couldn't be reached and `states` shows local files only.
    states_error: Option<String>,
    states_enabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EmulatorDto {
    id: String,
    name: String,
    installed: bool,
    flatpak: bool,
    path: Option<String>,
    data_dir: Option<String>,
    override_path: Option<String>,
    platforms: Vec<String>,
    problem: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BiosDto {
    file: String,
    status: &'static str,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsDto {
    rom_dir: String,
    sync_states: bool,
    max_state_mb: u64,
    conflict_policy: ConflictPolicy,
    device_name: Option<String>,
    accept_invalid_certs: bool,
    platform_emulators: std::collections::BTreeMap<String, String>,
}

// -------------------------------------------------------------------------- commands

#[tauri::command]
async fn status(st: TauriState<'_, AppState>) -> CmdResult<StatusDto> {
    Ok(status_of(&st, vec![]).await)
}

#[tauri::command]
async fn login(st: TauriState<'_, AppState>, args: LoginArgs) -> CmdResult<StatusDto> {
    let (raw, scopes) = match (args.code.as_deref().filter(|c| !c.trim().is_empty()), args.token.as_deref().filter(|t| !t.trim().is_empty())) {
        (Some(code), None) => {
            let t = RommClient::exchange_pair_code(&args.server, code, args.insecure)
                .await
                .map_err(|e| match e.status() {
                    Some(404) => "That pairing code wasn't recognised. Codes are single-use and expire after 5 minutes, so create a fresh one in RomM (Profile → Client API Tokens → Pair).".to_string(),
                    Some(401) | Some(403) => format!("The server refused the request: {e}"),
                    _ => format!("Couldn't reach the server or pair with it: {e}"),
                })?;
            (t.raw_token, Some(t.scopes))
        }
        (None, Some(t)) => (t.trim().to_string(), None),
        _ => return Err("Enter either a pairing code or a token.".into()),
    };
    let client = RommClient::new(&args.server, Some(raw.clone()), args.insecure).map_err(s)?;
    client.platforms().await.map_err(|e| format!("The server accepted the token but listing platforms failed: {e}"))?;

    store_token(&st.dirs, &raw).map_err(s)?;
    let url = client.base_url().to_string();
    st.update_cfg(|c| {
        c.server_url = Some(url);
        c.accept_invalid_certs = args.insecure;
        c.device_name = args.device_name.clone().filter(|n| !n.trim().is_empty());
        c.device_id = None;
    })
    .await?;
    *st.client.write().await = Some(client.clone());
    st.forget_ra().await;
    st.forget_library().await;
    st.device_id(&client).await?;

    let mut warnings = vec![];
    if let Some(scopes) = scopes {
        let missing: Vec<&str> = REQUIRED_SCOPES.iter().copied().filter(|r| !scopes.iter().any(|x| x == r)).collect();
        if !missing.is_empty() {
            warnings.push(format!(
                "This token is missing scopes the app needs ({}). Create a new token with them in RomM → Profile → Client API Tokens.",
                missing.join(", ")
            ));
        }
    }
    Ok(status_of(&st, warnings).await)
}

#[tauri::command]
async fn logout(st: TauriState<'_, AppState>) -> CmdResult<StatusDto> {
    let _ = std::fs::remove_file(st.dirs.token_file());
    *st.client.write().await = None;
    st.forget_ra().await;
    st.forget_library().await;
    st.update_cfg(|c| {
        c.server_url = None;
        c.device_id = None;
    })
    .await?;
    Ok(status_of(&st, vec![]).await)
}

#[tauri::command]
async fn platforms(st: TauriState<'_, AppState>) -> CmdResult<Vec<PlatformDto>> {
    let client = st.client().await?;
    let list: Vec<Platform> = client.platforms().await.map_err(s)?;
    let cfg = st.cfg.read().await.clone();
    let resolver = Resolver::new(&cfg);
    // Detect each emulator once, not once per platform.
    let installed: std::collections::HashMap<String, bool> =
        profiles::builtin().into_iter().map(|p| { let ok = resolver.installed(&p).is_some(); (p.id, ok) }).collect();
    // RetroArch cores count as installed only if the core file is actually present.
    let retro = resolver.emu_env("retroarch").ok().and_then(|(env, _)| env.retroarch);
    let choice_installed = |c: &plat::Choice| match &c.core {
        Some(core) => retro.as_ref().is_some_and(|l| retroarch::find_core(l, core, resolver.os).is_some()),
        None => installed.get(&c.emulator).copied().unwrap_or(false),
    };
    // The server counts files; the list shows games (copies of one game are a single entry), so
    // once the library is in memory its counts are used. `try_lock`: while it is still loading,
    // the sidebar shouldn't wait for it, and the server's counts will do until it's done.
    let games_per_platform = st.library.try_lock().ok().and_then(|g| g.as_ref().map(|c| browse::count_by_platform(&c.roms)));
    Ok(list
        .into_iter()
        .map(|p| {
            let emu = resolver.emulator_for(&p.slug).ok();
            let profile = emu.as_deref().and_then(profiles::get);
            let offered = plat::choices_for(&p.slug);
            let saved = plat::saved(&p.slug, &cfg.platform_emulators, &cfg.retroarch_cores);
            PlatformDto {
                id: p.id,
                name: p.title().to_string(),
                rom_count: games_per_platform.as_ref().map_or(p.rom_count, |m| m.get(&p.id).copied().unwrap_or(0)),
                firmware_count: p.firmware.len(),
                emulator_installed: profile.as_ref().is_some_and(|pr| resolver.installed(pr).is_some()),
                emulator_name: profile.map(|pr| pr.name),
                emulator: emu,
                automatic_label: offered.first().map(plat::Choice::label),
                choices: offered
                    .iter()
                    .map(|c| EmulatorChoiceDto { value: c.value(), label: c.label(), installed: choice_installed(c) })
                    .collect(),
                selected: saved.choice.map(|c| c.value()).unwrap_or_default(),
                ignored_override: saved.ignored.map(|id| profiles::get(&id).map(|pr| pr.name).unwrap_or(id)),
                slug: p.slug,
            }
        })
        .collect())
}

#[tauri::command]
async fn games(app: AppHandle, st: TauriState<'_, AppState>, args: GamesArgs) -> CmdResult<GamesPage> {
    if args.installed_only {
        // Served from the local database so it works offline and needs no paging.
        let db = st.db.lock().await;
        let needle = args.search.clone().unwrap_or_default().to_lowercase();
        let roms: Vec<&LocalRom> = db.roms.values().filter(|r| needle.is_empty() || r.name.to_lowercase().contains(&needle)).collect();
        // Two installed copies of one game (USA and Japan) would otherwise be identical cards.
        let file_names: Vec<String> = roms.iter().map(|r| r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        let entries: Vec<(&str, &str, &str)> = roms.iter().zip(&file_names).map(|(r, f)| (r.platform_slug.as_str(), r.name.as_str(), f.as_str())).collect();
        let labels = versions::installed_labels(&entries);
        let items: Vec<GameDto> = roms
            .iter()
            .zip(labels)
            .map(|(r, version_label)| GameDto {
                id: r.rom_id,
                name: r.name.clone(),
                platform_id: 0,
                platform_slug: r.platform_slug.clone(),
                installed: true,
                size_bytes: 0,
                multi_file: false,
                cover_path: r.cover_path.clone(),
                versions: vec![],
                version_label,
            })
            .collect();
        let total = Some(items.len() as i64);
        return Ok(GamesPage { items, total });
    }
    // The whole library is held in memory and sliced here. A list opened from the top may refresh
    // it if it has gone stale; a request for further down must see the same list as the first.
    let loaded = st.library(&app, (args.offset == 0).then_some(LIBRARY_TTL)).await?;
    let hits = browse::filter(&loaded.roms, args.platform_id, args.search.as_deref().unwrap_or(""));
    let total = Some(hits.len() as i64);
    let db = st.db.lock().await;
    let items = hits.into_iter().skip(args.offset.max(0) as usize).take(args.limit.max(0) as usize).map(|r| game_dto(r, &db)).collect();
    Ok(GamesPage { items, total })
}

/// Loads the library (if it isn't already) so the first list opens instantly, and so the
/// sidebar can count games rather than files. `force` refetches; a failed refresh is reported
/// and leaves the previous copy in use. Returns the number of games.
#[tauri::command]
async fn load_library(app: AppHandle, st: TauriState<'_, AppState>, force: Option<bool>) -> CmdResult<usize> {
    let max_age = if force.unwrap_or(false) { Duration::ZERO } else { LIBRARY_TTL };
    let loaded = st.library(&app, Some(max_age)).await?;
    match loaded.refresh_error {
        Some(e) if force.unwrap_or(false) => Err(e),
        _ => Ok(loaded.roms.len()),
    }
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "webp" => "image/webp",
        "png" => "image/png",
        "gif" => "image/gif",
        _ => "image/jpeg",
    }
}

fn data_url(bytes: &[u8], ext: &str) -> String {
    format!("data:{};base64,{}", mime_for(ext), base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Cover art as a data URL (cached on disk). `None` means "show a placeholder".
#[tauri::command]
async fn cover(st: TauriState<'_, AppState>, rom_id: i64, path: String) -> CmdResult<Option<String>> {
    let clean = path.split('?').next().unwrap_or(&path).to_string();
    let ext = clean.rsplit('.').next().unwrap_or("jpg").to_ascii_lowercase();
    let ext = if ["webp", "png", "gif", "jpg", "jpeg"].contains(&ext.as_str()) { ext } else { "jpg".to_string() };
    let dir = st.dirs.data_dir.join("covers");
    let mut hash = 0xcbf29ce484222325u64; // FNV-1a over the path so a changed cover invalidates the cache
    for b in path.bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let file = dir.join(format!("{rom_id}-{hash:016x}.{ext}"));
    if let Ok(bytes) = std::fs::read(&file) {
        return Ok(Some(data_url(&bytes, &ext)));
    }
    let Ok(client) = st.client().await else { return Ok(None) };
    match client.fetch_asset(&path).await {
        Ok((bytes, _)) if !bytes.is_empty() => {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&file, &bytes);
            Ok(Some(data_url(&bytes, &ext)))
        }
        _ => Ok(None),
    }
}

// ------------------------------------------------------------------ RetroAchievements
//
// Badge images need no command of their own: they are ordinary server assets, so the UI loads
// them through `cover` (authenticated fetch + on-disk cache keyed by path).

fn ra_cache_file(dirs: &AppDirs, rom_id: i64) -> PathBuf {
    dirs.data_dir.join("achievements").join(format!("{rom_id}.json"))
}

/// Live data when the server answers; otherwise the copy from the last successful fetch, marked
/// `stale` (the same promise the Installed view makes: it still works offline).
async fn fetch_achievements(st: &AppState, rom_id: i64, force: bool) -> CmdResult<AchievementsView> {
    let live: CmdResult<AchievementsView> = async {
        let client = st.client().await?;
        let (rom, me) = tokio::join!(client.rom_ra(rom_id), st.me(&client, force));
        let mut view = achievements::build(&rom.map_err(s)?, &me?);
        view.fetched_at = Some(achievements::now_rfc3339());
        Ok(view)
    }
    .await;

    let file = ra_cache_file(&st.dirs, rom_id);
    match live {
        Ok(view) => {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(json) = serde_json::to_vec(&view) {
                let _ = std::fs::write(&file, json);
            }
            Ok(view)
        }
        Err(e) => match std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<AchievementsView>(&b).ok()) {
            Some(mut cached) => {
                cached.stale = true;
                Ok(cached)
            }
            None => Err(e),
        },
    }
}

/// A game's achievements merged with what the user has earned.
#[tauri::command]
async fn achievements(st: TauriState<'_, AppState>, rom_id: i64, force: Option<bool>) -> CmdResult<AchievementsView> {
    fetch_achievements(&st, rom_id, force.unwrap_or(false)).await
}

/// Ask RomM to re-pull the user's progress from retroachievements.org, then reload this game.
/// RomM only does that on a schedule (daily, and only if the admin enabled it) or on request, so
/// without this the panel shows whatever RomM last fetched. Needs the `me.write` scope, which
/// the app deliberately doesn't ask for by default (it also allows editing the profile).
#[tauri::command]
async fn refresh_achievements(st: TauriState<'_, AppState>, rom_id: i64) -> CmdResult<AchievementsView> {
    let _busy = st.busy(RA_REFRESH)?;
    let client = st.client().await?;
    let me = st.me(&client, false).await?;
    client.refresh_ra(me.id, true).await.map_err(|e| match e.status() {
        Some(403) => "RomM refused: refreshing needs the me.write scope, which this token doesn't have. \
                      Pair a new token with it (RomM → Profile → Client API Tokens), or refresh from RomM's own profile page."
            .to_string(),
        Some(404) if e.to_string().contains("RetroAchievements username") => {
            "RomM doesn't know your RetroAchievements username. Add it under Profile in RomM, then try again.".to_string()
        }
        _ => s(e),
    })?;
    *st.ra_me.lock().await = None;
    fetch_achievements(&st, rom_id, true).await
}

#[tauri::command]
async fn install_game(app: AppHandle, st: TauriState<'_, AppState>, rom_id: i64) -> CmdResult<()> {
    let st = st.inner().clone();
    let busy = st.busy(rom_id)?;
    let client = st.client().await?;
    let rom_dir = st.cfg.read().await.rom_dir();
    tauri::async_runtime::spawn(async move {
        let _busy = busy;
        emit_job::<()>(&app, "romm://install", rom_id, "started", None, None);
        let res: CmdResult<()> = async {
            let rom = client.rom(rom_id).await.map_err(s)?;
            let mut last = Instant::now() - Duration::from_secs(1);
            let a = app.clone();
            let mut progress = move |done: u64, total: Option<u64>| {
                if last.elapsed() >= Duration::from_millis(120) || Some(done) == total {
                    last = Instant::now();
                    let _ = a.emit("romm://progress", ProgressEvent { rom_id, done, total });
                }
            };
            let local = library::install_rom(&client, &rom, &rom_dir, &mut progress).await.map_err(s)?;
            let mut db = st.db.lock().await;
            db.roms.insert(rom_id, local);
            db.save(&st.dirs.state_file()).map_err(s)
        }
        .await;
        match res {
            Ok(()) => emit_job::<()>(&app, "romm://install", rom_id, "finished", None, None),
            Err(e) => emit_job::<()>(&app, "romm://install", rom_id, "failed", Some(e), None),
        }
    });
    Ok(())
}

/// Remove the downloaded game. Saves and states on disk and on the server are never touched.
#[tauri::command]
async fn uninstall_game(st: TauriState<'_, AppState>, rom_id: i64) -> CmdResult<()> {
    let _busy = st.busy(rom_id)?;
    let mut db = st.db.lock().await;
    if let Some(rom) = db.roms.remove(&rom_id) {
        if rom.path.is_dir() {
            std::fs::remove_dir_all(&rom.path).map_err(s)?;
        } else if rom.path.is_file() {
            std::fs::remove_file(&rom.path).map_err(s)?;
        }
    }
    db.save(&st.dirs.state_file()).map_err(s)
}

fn local_rom(db: &State, rom_id: i64) -> CmdResult<LocalRom> {
    db.roms.get(&rom_id).cloned().ok_or_else(|| "this game isn't installed".to_string())
}

#[tauri::command]
async fn play_game(app: AppHandle, st: TauriState<'_, AppState>, rom_id: i64) -> CmdResult<()> {
    let st = st.inner().clone();
    let busy = st.busy(rom_id)?;
    let client = st.client().await?;
    let device = st.device_id(&client).await?;
    let rom = local_rom(&*st.db.lock().await, rom_id)?;
    let cfg = st.cfg.read().await.clone();
    // Fail fast, before any "syncing" UI, if the emulator can't be launched.
    let prepared = Resolver::new(&cfg).prepare(&rom).map_err(s)?;
    let policy = cfg.conflict_policy;
    let opts = st.state_opts().await;

    tauri::async_runtime::spawn(async move {
        let _busy = busy;
        emit_job::<()>(&app, "romm://play", rom_id, "syncing", None, None);
        let (env, cmd) = (prepared.env, prepared.command);
        let backups = st.dirs.backups_dir();
        let state_file = st.dirs.state_file();
        let run: CmdResult<PlaySummary> = async {
            let pre = {
                let mut db = st.db.lock().await;
                session::pre_play(&client, &device, &mut db, &state_file, backups.clone(), &rom, &env, policy, opts.as_ref()).await.map_err(s)?
            };
            emit_job::<()>(&app, "romm://play", rom_id, "running", None, None);
            let run = session::run_emulator(cmd, &env.profile.name).await.map_err(s)?;
            emit_job::<()>(&app, "romm://play", rom_id, "saving", None, None);
            let out = {
                let mut db = st.db.lock().await;
                session::post_play(&client, &device, &mut db, &state_file, backups, &rom, &env, policy, opts.as_ref(), pre, &run).await.map_err(s)?
            };
            let mut sync = SyncSummary::default();
            let mut notes = vec![];
            for r in [&out.pull, &out.push].into_iter().flatten() {
                sync.add_saves(r);
            }
            for r in [&out.state_pull, &out.state_push].into_iter().flatten() {
                sync.add_states(r);
            }
            if let Some(e) = &out.pull_error {
                notes.push(format!("Couldn't sync before launch (offline?): {e}"));
            }
            if let Some(e) = &out.push_error {
                notes.push(format!("Couldn't upload saves: {e}. They're tracked and will upload on the next sync."));
            }
            if let Some(e) = &out.state_error {
                notes.push(format!("Save states weren't synced: {e}"));
            }
            Ok(PlaySummary { played_secs: out.elapsed.as_secs(), exit_code: out.exit_code, notes, sync })
        }
        .await;
        match run {
            Ok(sum) => emit_job(&app, "romm://play", rom_id, "finished", None, Some(sum)),
            Err(e) => emit_job::<()>(&app, "romm://play", rom_id, "failed", Some(e), None),
        }
    });
    Ok(())
}

async fn sync_one(st: &AppState, client: &RommClient, device: &str, rom: &LocalRom, cfg: &Config, opts: Option<&StateOptions>) -> CmdResult<SyncSummary> {
    let (env, _) = Resolver::new(cfg).emu_env(&Resolver::new(cfg).emulator_for(&rom.platform_slug).map_err(s)?).map_err(s)?;
    let mut sum = SyncSummary::default();
    let mut db = st.db.lock().await;
    let backups = st.dirs.backups_dir();
    let mut syncer = Syncer { client, device_id: device, state: &mut db, backup_dir: backups.clone(), policy: cfg.conflict_policy };
    let r = syncer.sync_rom(rom, &env, None).await.map_err(s)?;
    sum.add_saves(&r);
    if let Some(o) = opts {
        let r = states::sync_states(client, &mut db, &backups, rom, &env, cfg.conflict_policy, o).await.map_err(s)?;
        sum.add_states(&r);
    }
    db.save(&st.dirs.state_file()).map_err(s)?;
    Ok(sum)
}

#[tauri::command]
async fn sync_game(app: AppHandle, st: TauriState<'_, AppState>, rom_id: i64) -> CmdResult<()> {
    let st = st.inner().clone();
    let busy = st.busy(rom_id)?;
    let client = st.client().await?;
    let device = st.device_id(&client).await?;
    let rom = local_rom(&*st.db.lock().await, rom_id)?;
    let cfg = st.cfg.read().await.clone();
    let opts = st.state_opts().await;
    tauri::async_runtime::spawn(async move {
        let _busy = busy;
        emit_job::<()>(&app, "romm://sync", rom_id, "syncing", None, None);
        match sync_one(&st, &client, &device, &rom, &cfg, opts.as_ref()).await {
            Ok(sum) => emit_job(&app, "romm://sync", rom_id, "finished", None, Some(sum)),
            Err(e) => emit_job::<()>(&app, "romm://sync", rom_id, "failed", Some(e), None),
        }
    });
    Ok(())
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SyncAllResult {
    games: usize,
    skipped: Vec<String>,
    summary: SyncSummaryOut,
}
type SyncSummaryOut = serde_json::Value;

#[tauri::command]
async fn sync_all(app: AppHandle, st: TauriState<'_, AppState>) -> CmdResult<()> {
    let st = st.inner().clone();
    let busy = st.busy(SYNC_ALL)?;
    let client = st.client().await?;
    let device = st.device_id(&client).await?;
    let cfg = st.cfg.read().await.clone();
    let opts = st.state_opts().await;
    let roms: Vec<LocalRom> = st.db.lock().await.roms.values().cloned().collect();
    tauri::async_runtime::spawn(async move {
        let _busy = busy;
        emit_job::<()>(&app, "romm://sync-all", SYNC_ALL, "syncing", None, None);
        let mut total = SyncSummary::default();
        let mut skipped = vec![];
        for rom in &roms {
            match sync_one(&st, &client, &device, rom, &cfg, opts.as_ref()).await {
                Ok(sum) => {
                    total.uploaded.extend(sum.uploaded);
                    total.downloaded.extend(sum.downloaded);
                    total.conflicts.extend(sum.conflicts);
                    total.failed.extend(sum.failed);
                    total.state_uploaded.extend(sum.state_uploaded);
                    total.state_downloaded.extend(sum.state_downloaded);
                    total.state_conflicts.extend(sum.state_conflicts);
                    total.state_failed.extend(sum.state_failed);
                }
                Err(e) => skipped.push(format!("{}: {e}", rom.name)),
            }
        }
        let summary = serde_json::to_value(&total).unwrap_or_default();
        emit_job(&app, "romm://sync-all", SYNC_ALL, "finished", None, Some(SyncAllResult { games: roms.len(), skipped, summary }));
    });
    Ok(())
}

#[tauri::command]
async fn game_details(st: TauriState<'_, AppState>, rom_id: i64, platform_slug: String) -> CmdResult<DetailsDto> {
    let cfg = st.cfg.read().await.clone();
    let resolver = Resolver::new(&cfg);
    let db = st.db.lock().await;
    let local = db.roms.get(&rom_id).cloned();
    let states_enabled = cfg.sync_states;

    let emu_id = resolver.emulator_for(&platform_slug).ok();
    let profile = emu_id.as_deref().and_then(profiles::get);
    let resolved = emu_id.as_deref().map(|id| resolver.emu_env(id));
    let emulator = match (&profile, &resolved) {
        (Some(p), Some(Ok(_))) => Some(EmulatorRef { id: p.id.clone(), name: p.name.clone(), installed: true, problem: None }),
        (Some(p), Some(Err(e))) => Some(EmulatorRef { id: p.id.clone(), name: p.name.clone(), installed: false, problem: Some(e.to_string()) }),
        _ => None,
    };
    let (mut saves, mut states_v, mut states_error) = (vec![], vec![], None);
    if let (Some(rom), Some(Ok((env, _)))) = (&local, &resolved) {
        saves = saves_overview(env, rom, &db).map_err(s)?;
        if states_enabled {
            let client = st.client.read().await.clone();
            match states::overview(client.as_ref(), &db, rom, env).await {
                Ok(v) => states_v = v,
                Err(e) => {
                    states_error = Some(e.to_string());
                    states_v = states::overview(None, &db, rom, env).await.map_err(s)?;
                }
            }
        }
    }
    Ok(DetailsDto {
        installed: local.is_some(),
        install_path: local.as_ref().map(|r| r.path.display().to_string()),
        emulator,
        saves,
        states: states_v,
        states_error,
        states_enabled,
    })
}

#[tauri::command]
async fn emulators(st: TauriState<'_, AppState>) -> CmdResult<Vec<EmulatorDto>> {
    let cfg = st.cfg.read().await.clone();
    let resolver = Resolver::new(&cfg);
    Ok(profiles::builtin()
        .into_iter()
        .map(|p| {
            let (inst, problem, data) = match resolver.emu_env(&p.id) {
                Ok((env, inst)) => (Some(inst), None, Some(env.data_root.display().to_string())),
                Err(e) => (None, Some(e.to_string()), None),
            };
            let path = inst.as_ref().map(|i| match &i.kind {
                romm_core::detect::InstallKind::Native(x) => x.display().to_string(),
                romm_core::detect::InstallKind::Flatpak(id) => format!("flatpak run {id}"),
            });
            let platforms = if p.id == "retroarch" {
                retroarch::supported_slugs().map(String::from).collect()
            } else {
                p.platforms.clone()
            };
            EmulatorDto {
                override_path: cfg.emulator_paths.get(&p.id).map(|x| x.display().to_string()),
                flatpak: inst.as_ref().is_some_and(|i| i.is_flatpak()),
                installed: inst.is_some(),
                id: p.id,
                name: p.name,
                path,
                data_dir: data,
                platforms,
                problem,
            }
        })
        .collect())
}

#[tauri::command]
async fn set_emulator_path(st: TauriState<'_, AppState>, id: String, path: Option<String>) -> CmdResult<()> {
    if profiles::get(&id).is_none() {
        return Err(format!("unknown emulator '{id}'"));
    }
    if let Some(p) = &path {
        if !PathBuf::from(p).is_file() {
            return Err(format!("{p} is not a file"));
        }
    }
    st.update_cfg(|c| match path {
        Some(p) => {
            c.emulator_paths.insert(id, PathBuf::from(p));
        }
        None => {
            c.emulator_paths.remove(&id);
        }
    })
    .await
}

/// `choice` is a `Choice::value()`: `dolphin`, `retroarch:gambatte`, or None/"" for Automatic.
#[tauri::command]
async fn set_platform_choice(st: TauriState<'_, AppState>, slug: String, choice: Option<String>) -> CmdResult<()> {
    let mut guard = st.cfg.write().await;
    let cfg = &mut *guard;
    plat::set_choice(&mut cfg.platform_emulators, &mut cfg.retroarch_cores, &slug, choice.as_deref())?;
    cfg.save(&st.dirs).map_err(s)
}

#[tauri::command]
async fn install_bios(st: TauriState<'_, AppState>, platform_slug: String) -> CmdResult<Vec<BiosDto>> {
    let client = st.client().await?;
    let plat = client
        .platforms()
        .await
        .map_err(s)?
        .into_iter()
        .find(|p| p.slug == platform_slug)
        .ok_or_else(|| format!("no platform '{platform_slug}' on the server"))?;
    if plat.firmware.is_empty() {
        return Err(format!("RomM has no BIOS files for {}. Add them to the platform's bios folder on the server and rescan.", plat.title()));
    }
    let cfg = st.cfg.read().await.clone();
    let resolver = Resolver::new(&cfg);
    let (env, _) = resolver.emu_env(&resolver.emulator_for(&platform_slug).map_err(s)?).map_err(s)?;
    let rule = env.profile.bios.as_ref().ok_or_else(|| format!("{} has no known BIOS folder", env.profile.name))?;
    let dir = match &env.retroarch {
        Some(l) => l.system.clone(),
        None => env.data_root.join(&rule.dir),
    };
    let res = library::install_bios(&client, &plat, &dir, &mut |_, _| {}).await.map_err(s)?;
    Ok(res
        .into_iter()
        .map(|r| BiosDto {
            file: r.file,
            status: match r.status {
                BiosStatus::Installed => "installed",
                BiosStatus::AlreadyPresent => "already present",
                BiosStatus::InstalledUnverified => "installed (no checksum to verify)",
            },
        })
        .collect())
}

#[tauri::command]
async fn settings(st: TauriState<'_, AppState>) -> CmdResult<SettingsDto> {
    let c = st.cfg.read().await;
    Ok(SettingsDto {
        rom_dir: c.rom_dir().display().to_string(),
        sync_states: c.sync_states,
        max_state_mb: c.max_state_mb,
        conflict_policy: c.conflict_policy,
        device_name: c.device_name.clone(),
        accept_invalid_certs: c.accept_invalid_certs,
        platform_emulators: c.platform_emulators.clone(),
    })
}

#[tauri::command]
async fn save_settings(st: TauriState<'_, AppState>, settings: SettingsDto) -> CmdResult<()> {
    if settings.max_state_mb == 0 {
        return Err("The state size limit must be at least 1 MB.".into());
    }
    let dir = PathBuf::from(&settings.rom_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Can't use {} as the ROM folder: {e}", dir.display()))?;
    st.update_cfg(|c| {
        c.rom_dir = Some(dir);
        c.sync_states = settings.sync_states;
        c.max_state_mb = settings.max_state_mb;
        c.conflict_policy = settings.conflict_policy;
        c.device_name = settings.device_name.filter(|n| !n.trim().is_empty());
        c.accept_invalid_certs = settings.accept_invalid_certs;
    })
    .await?;
    // A changed certificate policy needs a fresh HTTP client.
    let cfg = st.cfg.read().await.clone();
    *st.client.write().await = build_client(&st.dirs, &cfg);
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            use tauri::Manager;
            let dirs = AppDirs::discover();
            let cfg = Config::load(&dirs)?;
            let db = State::load(&dirs.state_file())?;
            let client = build_client(&dirs, &cfg);
            app.manage(AppState {
                dirs,
                cfg: Arc::new(RwLock::new(cfg)),
                db: Arc::new(Mutex::new(db)),
                client: Arc::new(RwLock::new(client)),
                busy: Arc::new(StdMutex::new(HashSet::new())),
                ra_me: Arc::new(Mutex::new(None)),
                library: Arc::new(Mutex::new(None)),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status,
            login,
            logout,
            platforms,
            games,
            load_library,
            cover,
            achievements,
            refresh_achievements,
            install_game,
            uninstall_game,
            play_game,
            sync_game,
            sync_all,
            game_details,
            emulators,
            set_emulator_path,
            set_platform_choice,
            install_bios,
            settings,
            save_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running RomM Desktop");
}
