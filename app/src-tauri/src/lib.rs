//! Tauri backend. Every command is a thin wrapper over `romm-core`; the long-running ones
//! (install, play, sync) run as background jobs and report through `romm://…` events.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use base64::Engine;
use romm_core::achievements::{self, AchievementsView};
use romm_core::api::types::{Me, Platform, Rom};
use romm_core::api::RommClient;
use romm_core::config::{register_device, store_token, AppDirs, Config};
use romm_core::detect::Installed;
use romm_core::diagnostics::{self, LogFile, Watchdog};
use romm_core::emu_settings;
use romm_core::library::{self, BiosStatus};
use romm_core::login::{self, Credentials};
use romm_core::report::TransferReport;
use romm_core::resolve::Resolver;
use romm_core::state::{LocalRom, State};
use romm_core::states::{self, StateOptions, StateReport, StateView};
use romm_core::session::SyncCtx;
use romm_core::sync::{saves_overview, ConflictPolicy, SaveView, SyncReport};
use romm_core::{browse, platforms as plat, profiles, retroarch, session, versions};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State as TauriState};
use tokio::sync::{Mutex, RwLock};

type CmdResult<T> = Result<T, String>;

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

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
    /// RomM answered a refresh with 403: this token lacks `me.write`. Remembered so that a game
    /// closing doesn't fetch the profile just to be refused again. Cleared on sign-in and sign-out.
    ra_refresh_refused: Arc<AtomicBool>,
    /// The whole library, one entry per game, fetched once (see `AppState::library`). The lock is
    /// held while it loads, so everyone who needs the library waits for that one fetch.
    library: Arc<Mutex<Option<LibraryCache>>>,
    diag: Arc<Diag>,
}

/// The diagnostics log (see `romm_core::diagnostics`) and what the watchdog needs to know about the page.
struct Diag {
    log: LogFile,
    started: Instant,
    /// Milliseconds since start of the page's last report, plus one; 0 = none yet.
    last_report: AtomicU64,
    page_visible: AtomicBool,
}

impl Diag {
    fn new(dirs: &AppDirs) -> Self {
        Diag { log: LogFile::new(&dirs.data_dir), started: Instant::now(), last_report: AtomicU64::new(0), page_visible: AtomicBool::new(true) }
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn last_report_ms(&self) -> Option<u64> {
        self.last_report.load(Ordering::Relaxed).checked_sub(1)
    }

    /// A note from the backend.
    fn app(&self, msg: &str) {
        self.log.append(&format!("{} app  {msg}", diagnostics::timestamp()));
    }

    /// A line from the page. `report`: this is one of its periodic reports, which the watchdog listens for.
    fn page(&self, line: &str, report: bool, visible: bool) {
        if report {
            self.last_report.store(self.elapsed_ms() + 1, Ordering::Relaxed);
        }
        self.page_visible.store(visible, Ordering::Relaxed);
        self.log.append(&format!("{} page {line}", diagnostics::timestamp()));
    }
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
        self.ra_refresh_refused.store(false, Ordering::Relaxed);
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
        let began = Instant::now();
        self.diag.app("library loading");
        match client.library(&mut progress).await {
            Ok(roms) => {
                let n = roms.len();
                self.diag.app(&format!("library loaded: {n} games in {} ms", began.elapsed().as_millis()));
                let roms = Arc::new(roms);
                *slot = Some(LibraryCache { at: Instant::now(), roms: roms.clone() });
                let _ = app.emit("romm://library", LibraryEvent { phase: "finished", loaded: n, total: Some(n as i64), error: None });
                Ok(Loaded { roms, refresh_error: None })
            }
            Err(e) => {
                let e = s(e);
                self.diag.app(&format!("library failed after {} ms: {e}", began.elapsed().as_millis()));
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
        let name = self.cfg.read().await.device_name.clone();
        let id = register_device(client, name.as_deref()).await.map_err(s)?;
        self.update_cfg(|c| c.device_id = Some(id.clone())).await?;
        Ok(id)
    }

    async fn state_opts(&self) -> Option<StateOptions> {
        self.cfg.read().await.state_options()
    }
}

fn build_client(dirs: &AppDirs, cfg: &Config) -> Option<RommClient> {
    cfg.client(dirs).ok()
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
    /// The user hid this platform from the game browser. It is still listed here so it can be shown again.
    hidden: bool,
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

/// How big one version of a game is (see `version_sizes`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionSizeDto {
    id: i64,
    size_bytes: u64,
    multi_file: bool,
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
    /// The "Hidden platforms" view: only the games of platforms the user has hidden, which are
    /// listed nowhere else.
    #[serde(default)]
    hidden_only: bool,
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

/// What one kind of sync did, by file name: what the frontend lists.
#[derive(Serialize, Default, Clone)]
struct TransferDto {
    uploaded: Vec<String>,
    downloaded: Vec<String>,
    archived: Vec<String>,
    conflicts: Vec<String>,
    skipped: Vec<String>,
    failed: Vec<String>,
}

impl TransferDto {
    fn add(&mut self, r: &TransferReport) {
        // Every field is named so that one added to the report has to be decided about here; a
        // hand-copied list once left out what was archived.
        let TransferReport { uploaded, downloaded, archived, conflicts, skipped, failed, unchanged: _ } = r;
        self.uploaded.extend(uploaded.iter().cloned());
        self.downloaded.extend(downloaded.iter().cloned());
        self.archived.extend(archived.iter().cloned());
        self.conflicts.extend(conflicts.iter().map(|c| c.file_name.clone()));
        self.skipped.extend(skipped.iter().cloned());
        self.failed.extend(failed.iter().cloned());
    }
}

#[derive(Serialize, Default, Clone)]
struct SyncSummary {
    saves: TransferDto,
    states: TransferDto,
}

impl SyncSummary {
    fn of(saves: &SyncReport, states: &StateReport) -> SyncSummary {
        let mut sum = SyncSummary::default();
        sum.add_saves(saves);
        sum.add_states(states);
        sum
    }

    fn add_saves(&mut self, r: &SyncReport) {
        self.saves.add(r);
    }
    fn add_states(&mut self, r: &StateReport) {
        self.states.add(r);
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

/// What the Emulator settings tab needs to know about one emulator (or one RetroArch core): what can be
/// controlled, and whether it's here.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EmulationScopeDto {
    scope: String,
    emulator_id: String,
    name: String,
    core: Option<String>,
    installed: bool,
    platforms: Vec<String>,
    /// True when these settings edit one of the emulator's own files; false when everything is
    /// passed on the command line and its files are never touched.
    changes_files: bool,
    settings: Vec<SettingDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingDto {
    id: String,
    label: String,
    help: String,
    choices: Vec<SettingChoiceDto>,
}

#[derive(Serialize)]
struct SettingChoiceDto {
    value: String,
    label: String,
}

// -------------------------------------------------------------------------- commands

#[tauri::command]
async fn status(st: TauriState<'_, AppState>) -> CmdResult<StatusDto> {
    Ok(status_of(&st, vec![]).await)
}

#[tauri::command]
async fn login(st: TauriState<'_, AppState>, args: LoginArgs) -> CmdResult<StatusDto> {
    let credentials = Credentials::from_inputs(args.code.as_deref(), args.token.as_deref()).map_err(s)?;
    let connected = login::connect(&args.server, credentials, args.insecure).await.map_err(s)?;
    let client = connected.client.clone();

    store_token(&st.dirs, &connected.token).map_err(s)?;
    let url = client.base_url().to_string();
    st.update_cfg(|c| c.set_server(&url, args.insecure, args.device_name.clone().filter(|n| !n.trim().is_empty()))).await?;
    *st.client.write().await = Some(client.clone());
    st.forget_ra().await;
    st.forget_library().await;
    st.device_id(&client).await?;

    Ok(status_of(&st, connected.scope_warning().into_iter().collect()).await)
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
    let retro = resolver.emu_env(profiles::RETROARCH).ok().and_then(|(env, _)| env.retroarch);
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
                hidden: cfg.hidden_platforms.contains(&p.slug),
                slug: p.slug,
            }
        })
        .collect())
}

#[tauri::command]
async fn games(app: AppHandle, st: TauriState<'_, AppState>, args: GamesArgs) -> CmdResult<GamesPage> {
    // Platforms the user has hidden from the game browser: none of their games are listed, installed or not,
    // except in the "Hidden platforms" view (`hidden_only`).
    let hidden = st.cfg.read().await.hidden_platforms.clone();
    if args.installed_only {
        // Served from the local database so it works offline and needs no paging.
        let db = st.db.lock().await;
        let roms: Vec<&LocalRom> = browse::filter_installed(db.roms.values(), args.search.as_deref().unwrap_or(""), &hidden);
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
    let search = args.search.as_deref().unwrap_or("");
    let hits = if args.hidden_only {
        browse::filter_hidden(&loaded.roms, search, &hidden)
    } else {
        browse::filter(&loaded.roms, args.platform_id, search, &hidden)
    };
    let total = Some(hits.len() as i64);
    let db = st.db.lock().await;
    let items = hits.into_iter().skip(args.offset.max(0) as usize).take(args.limit.max(0) as usize).map(|r| game_dto(r, &db)).collect();
    Ok(GamesPage { items, total })
}

/// The size of each of the given versions of a game. The library list only knows the primary
/// version's, because RomM's `sibling_roms` carries none; the panel asks for the others when it
/// opens. A version the server can't be asked about is simply absent from the answer.
#[tauri::command]
async fn version_sizes(st: TauriState<'_, AppState>, ids: Vec<i64>) -> CmdResult<Vec<VersionSizeDto>> {
    let client = st.client().await?;
    Ok(client.rom_sizes(&ids).await.into_iter().map(|r| VersionSizeDto { id: r.id, size_bytes: r.bytes, multi_file: r.multi_file }).collect())
}

/// The page reporting on itself (see `romm_core::diagnostics`). Fire and forget: nothing here may fail.
#[tauri::command]
fn diag_page(st: TauriState<'_, AppState>, line: String, report: bool, visible: bool) {
    st.diag.page(&line, report, visible);
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

/// After a game closes, ask RomM to re-pull the user's progress from retroachievements.org, so what
/// was just earned shows up without a trip to the Refresh button. RomM re-pulls the whole profile
/// (every game), and an open Achievements panel is told to reload once it has.
///
/// Never speaks to the user, and nothing waits for it. A failure is only logged, because most tokens
/// lack `me.write` and would otherwise hear about it after every game. The first 403 stops further
/// attempts until the next sign-in.
async fn refresh_ra_after_play(app: &AppHandle, st: &AppState) {
    if st.ra_refresh_refused.load(Ordering::Relaxed) {
        return;
    }
    // One refresh at a time. The Refresh button holds the same key; if it is running, it will show
    // its own result.
    let Ok(_busy) = st.busy(RA_REFRESH) else { return };
    let Ok(client) = st.client().await else { return }; // not signed in
    let me = match st.me(&client, false).await {
        Ok(me) => me,
        Err(e) => {
            st.diag.app(&format!("ra refresh after play: couldn't read the profile: {e}"));
            return;
        }
    };
    // Without a username RomM answers 404; there is nothing to pull, so don't ask.
    if me.ra_username.as_deref().is_none_or(|u| u.trim().is_empty()) {
        return;
    }
    match client.refresh_ra(me.id, true).await {
        Ok(()) => {
            *st.ra_me.lock().await = None; // the profile we hold predates the refresh
            let _ = app.emit("romm://achievements", ());
            st.diag.app("ra refresh after play: done");
        }
        Err(e) if e.status() == Some(403) => {
            st.ra_refresh_refused.store(true, Ordering::Relaxed);
            st.diag.app("ra refresh after play: RomM refused (this token lacks me.write); not asking again until the next sign-in");
        }
        Err(e) => st.diag.app(&format!("ra refresh after play: failed: {e}")),
    }
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
    let result = client.refresh_ra(me.id, true).await;
    // What the button learns, the refresh after a game should know too: a 403 means it would only be
    // refused as well, and a success proves the token can refresh after all.
    match &result {
        Ok(()) => st.ra_refresh_refused.store(false, Ordering::Relaxed),
        Err(e) if e.status() == Some(403) => st.ra_refresh_refused.store(true, Ordering::Relaxed),
        Err(_) => {}
    }
    result.map_err(|e| match e.status() {
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
        let (env, cmd, tuning) = (prepared.env, prepared.command, prepared.tuning);
        let backups = st.dirs.backups_dir();
        let state_file = st.dirs.state_file();
        let cx = SyncCtx { client: &client, device_id: &device, backup_dir: &backups, policy, states: opts.as_ref() };
        let run: CmdResult<PlaySummary> = async {
            let pre = {
                let mut db = st.db.lock().await;
                session::pre_play(&cx, &mut db, &state_file, &rom, &env).await.map_err(s)?
            };
            emit_job::<()>(&app, "romm://play", rom_id, "running", None, None);
            // The emulator settings chosen under Emulators & BIOS → Emulator settings, written just before it starts.
            // A problem here is a note for the player, never a reason not to play.
            let tuning_notes = tuning.apply(&st.dirs.emulator_config_backups());
            let run = session::run_emulator(cmd, &env.profile.name).await.map_err(s)?;
            // The emulator has exited, so whatever was earned is on retroachievements.org by now. Ask
            // RomM to pull it while the saves upload; this neither delays the upload nor can fail it.
            {
                let (app, st) = (app.clone(), st.clone());
                tauri::async_runtime::spawn(async move { refresh_ra_after_play(&app, &st).await });
            }
            emit_job::<()>(&app, "romm://play", rom_id, "saving", None, None);
            let out = {
                let mut db = st.db.lock().await;
                session::post_play(&cx, &mut db, &state_file, &rom, &env, pre, &run).await.map_err(s)?
            };
            let (saves, states) = out.reports();
            let mut notes = tuning_notes;
            notes.extend(out.notes());
            Ok(PlaySummary { played_secs: out.elapsed.as_secs(), exit_code: out.exit_code, notes, sync: SyncSummary::of(&saves, &states) })
        }
        .await;
        match run {
            Ok(sum) => emit_job(&app, "romm://play", rom_id, "finished", None, Some(sum)),
            Err(e) => emit_job::<()>(&app, "romm://play", rom_id, "failed", Some(e), None),
        }
    });
    Ok(())
}

async fn sync_one(st: &AppState, client: &RommClient, device: &str, rom: &LocalRom, cfg: &Config, opts: Option<&StateOptions>) -> CmdResult<(SyncReport, StateReport)> {
    let env = Resolver::new(cfg).env_for_sync(&rom.platform_slug).map_err(s)?;
    let mut db = st.db.lock().await;
    let backups = st.dirs.backups_dir();
    let round = SyncCtx { client, device_id: device, backup_dir: &backups, policy: cfg.conflict_policy, states: opts }.sync_game(&mut db, rom, &env, None).await;
    // What was exchanged is recorded whether or not all of it worked, or the next sync would repeat it.
    db.save(&st.dirs.state_file()).map_err(s)?;
    let saves = round.saves.map_err(s)?;
    let mut states = StateReport::default();
    match round.states {
        Some(Ok(r)) => states = r,
        // The saves were synced and that stands: say what went wrong with the states instead of
        // discarding the whole result.
        Some(Err(e)) => states.failed.push(format!("states: {e}")),
        None => {}
    }
    Ok((saves, states))
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
            Ok((saves, states)) => emit_job(&app, "romm://sync", rom_id, "finished", None, Some(SyncSummary::of(&saves, &states))),
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
    summary: SyncSummary,
}

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
        // Totalled as reports and turned into a summary once, so nothing a game's sync reports can
        // be left out of the total (a hand-copied list of fields once left out what was archived).
        let (mut saves, mut states) = (SyncReport::default(), StateReport::default());
        let mut skipped = vec![];
        for rom in &roms {
            match sync_one(&st, &client, &device, rom, &cfg, opts.as_ref()).await {
                Ok((game_saves, game_states)) => {
                    saves.merge(game_saves);
                    states.merge(game_states);
                }
                Err(e) => skipped.push(format!("{}: {e}", rom.name)),
            }
        }
        let summary = SyncSummary::of(&saves, &states);
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
    let resolved = emu_id.as_deref().map(|_| resolver.env_for_platform(&platform_slug));
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
            (states_v, states_error) = states::overview_or_local(client.as_ref(), &db, rom, env).await.map_err(s)?;
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
            let path = inst.as_ref().map(Installed::describe);
            let platforms = plat::handled_by(&p);
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

/// What the Emulator settings tab can offer: every emulator (and RetroArch core) with settings the app can
/// control, and whether it's installed. Static apart from the installed flag; the values are in `settings`.
#[tauri::command]
async fn emulation_schema(st: TauriState<'_, AppState>) -> CmdResult<Vec<EmulationScopeDto>> {
    let cfg = st.cfg.read().await.clone();
    let resolver = Resolver::new(&cfg);
    Ok(emu_settings::scopes()
        .into_iter()
        .map(|sc| {
            let profile = profiles::get(&sc.profile_id);
            let installed = profile.as_ref().is_some_and(|p| resolver.installed(p).is_some());
            let platforms = match (&sc.core, &profile) {
                (Some(core), _) => retroarch::supported_slugs()
                    .filter(|slug| retroarch::cores_for(slug).contains(&core.as_str()))
                    .map(String::from)
                    .collect(),
                (None, Some(p)) => p.platforms.clone(),
                (None, None) => vec![],
            };
            let changes_files = sc.changes_files();
            EmulationScopeDto {
                changes_files,
                scope: sc.scope,
                emulator_id: sc.profile_id,
                name: sc.name,
                core: sc.core,
                installed,
                platforms,
                settings: sc
                    .settings
                    .into_iter()
                    .map(|s| SettingDto {
                        id: s.id,
                        label: s.label,
                        help: s.help,
                        choices: s.choices.into_iter().map(|c| SettingChoiceDto { value: c.value, label: c.label }).collect(),
                    })
                    .collect(),
            }
        })
        .collect())
}

#[tauri::command]
async fn set_emulator_path(st: TauriState<'_, AppState>, id: String, path: Option<String>) -> CmdResult<()> {
    let mut cfg = st.cfg.write().await;
    cfg.set_emulator_path(&id, path.map(PathBuf::from)).map_err(s)?;
    cfg.save(&st.dirs).map_err(s)
}

/// `choice` is a `Choice::value()`: `dolphin`, `retroarch:gambatte`, or None/"" for Automatic.
#[tauri::command]
async fn set_platform_choice(st: TauriState<'_, AppState>, slug: String, choice: Option<String>) -> CmdResult<()> {
    let mut guard = st.cfg.write().await;
    let cfg = &mut *guard;
    plat::set_choice(&mut cfg.platform_emulators, &mut cfg.retroarch_cores, &slug, choice.as_deref())?;
    cfg.save(&st.dirs).map_err(s)
}

/// Hides a platform, and all its games, from the game browser (or shows it again). Only what is
/// listed changes: installed games stay installed and their saves still sync.
#[tauri::command]
async fn set_platform_hidden(st: TauriState<'_, AppState>, slug: String, hidden: bool) -> CmdResult<()> {
    let mut cfg = st.cfg.write().await;
    if hidden {
        cfg.hidden_platforms.insert(slug);
    } else {
        cfg.hidden_platforms.remove(&slug);
    }
    cfg.save(&st.dirs).map_err(s)
}

#[tauri::command]
async fn install_bios(st: TauriState<'_, AppState>, platform_slug: String) -> CmdResult<Vec<BiosDto>> {
    let client = st.client().await?;
    let cfg = st.cfg.read().await.clone();
    let (_, res) = library::install_platform_bios(&client, &Resolver::new(&cfg), &platform_slug, &mut |_, _| {}).await.map_err(s)?;
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

/// What's been chosen under Emulators & BIOS → Emulator settings: scope → setting → value (see
/// `emu_settings`). Kept apart from `settings`, since it is edited on a different page and the two
/// must not overwrite each other with a stale copy.
#[tauri::command]
async fn emulator_settings(st: TauriState<'_, AppState>) -> CmdResult<emu_settings::Values> {
    Ok(st.cfg.read().await.emulator_settings.clone())
}

/// Saves the emulator settings, forgetting anything the catalogue no longer has.
#[tauri::command]
async fn save_emulator_settings(st: TauriState<'_, AppState>, values: emu_settings::Values) -> CmdResult<()> {
    st.update_cfg(|c| c.emulator_settings = emu_settings::sanitize(values)).await?;
    Ok(())
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
            let diag = Arc::new(Diag::new(&dirs));
            diag.app(&format!(
                "start romm-desktop {} on {}/{}, web view {}",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH,
                tauri::webview_version().unwrap_or_else(|e| format!("unknown ({e})")),
            ));
            // If the page stops reporting while it is on screen, say so in the log.
            let watched = diag.clone();
            tauri::async_runtime::spawn(async move {
                let mut watchdog = Watchdog::new(Duration::from_secs(10));
                let mut tick = tokio::time::interval(Duration::from_secs(2));
                loop {
                    tick.tick().await;
                    let visible = watched.page_visible.load(Ordering::Relaxed);
                    if let Some(line) = watchdog.check(watched.elapsed_ms(), watched.last_report_ms(), visible) {
                        watched.app(&line);
                    }
                }
            });
            app.manage(AppState {
                dirs,
                cfg: Arc::new(RwLock::new(cfg)),
                db: Arc::new(Mutex::new(db)),
                client: Arc::new(RwLock::new(client)),
                busy: Arc::new(StdMutex::new(HashSet::new())),
                ra_me: Arc::new(Mutex::new(None)),
                ra_refresh_refused: Arc::new(AtomicBool::new(false)),
                library: Arc::new(Mutex::new(None)),
                diag,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            emulation_schema,
            emulator_settings,
            save_emulator_settings,
            set_platform_hidden,
            status,
            login,
            logout,
            platforms,
            games,
            load_library,
            version_sizes,
            diag_page,
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

#[cfg(test)]
mod tests {
    use super::*;
    use romm_core::sync::PendingConflict;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// One game's sync, as `sync_one` hands it to `sync_game` and `sync_all`.
    fn game(n: &str) -> (SyncReport, StateReport) {
        let tag = |what: &str| format!("{n}.{what}");
        let conflict = |what: &str| PendingConflict { rom_id: 1, file_name: tag(what), server_updated_at: None, reason: None };
        (
            SyncReport {
                uploaded: vec![tag("up")],
                downloaded: vec![tag("down")],
                archived: vec![tag("archived")],
                conflicts: vec![conflict("conflict")],
                skipped: vec![],
                failed: vec![tag("failed")],
                unchanged: 1,
            },
            StateReport {
                uploaded: vec![tag("sup")],
                downloaded: vec![tag("sdown")],
                archived: vec![tag("sarchived")],
                conflicts: vec![conflict("sconflict")],
                skipped: vec![tag("sskipped")],
                failed: vec![tag("sfailed")],
                unchanged: 1,
            },
        )
    }

    /// "Sync all" used to copy eight of the summary's eleven fields by hand and so never said that a
    /// local copy had been archived on the server, or a state skipped for its size, however many
    /// games it happened to. The total is now made the way `sync_all` makes it.
    #[test]
    fn a_total_over_several_games_reports_everything_each_game_reported() {
        let (mut saves, mut states) = (SyncReport::default(), StateReport::default());
        for n in ["A", "B"] {
            let (s, st) = game(n);
            saves.merge(s);
            states.merge(st);
        }
        let sum = SyncSummary::of(&saves, &states);

        assert_eq!(sum.saves.uploaded, names(&["A.up", "B.up"]));
        assert_eq!(sum.saves.downloaded, names(&["A.down", "B.down"]));
        assert_eq!(sum.saves.archived, names(&["A.archived", "B.archived"]));
        assert_eq!(sum.saves.conflicts, names(&["A.conflict", "B.conflict"]));
        assert_eq!(sum.saves.failed, names(&["A.failed", "B.failed"]));
        assert_eq!(sum.states.uploaded, names(&["A.sup", "B.sup"]));
        assert_eq!(sum.states.downloaded, names(&["A.sdown", "B.sdown"]));
        assert_eq!(sum.states.archived, names(&["A.sarchived", "B.sarchived"]));
        assert_eq!(sum.states.conflicts, names(&["A.sconflict", "B.sconflict"]));
        assert_eq!(sum.states.skipped, names(&["A.sskipped", "B.sskipped"]));
        assert_eq!(sum.states.failed, names(&["A.sfailed", "B.sfailed"]));
    }

    #[test]
    fn one_games_summary_is_the_same_as_that_game_alone_in_a_total() {
        let (saves, states) = game("A");
        let alone = serde_json::to_value(SyncSummary::of(&saves, &states)).unwrap();
        let (mut total_saves, mut total_states) = (SyncReport::default(), StateReport::default());
        let (s, st) = game("A");
        total_saves.merge(s);
        total_states.merge(st);
        assert_eq!(alone, serde_json::to_value(SyncSummary::of(&total_saves, &total_states)).unwrap());
    }

    /// The names the frontend's `SyncSummary` type (app/src/types.ts) reads. Renaming one here
    /// without there would make it silently `undefined` in the UI.
    #[test]
    fn the_summary_is_sent_under_the_names_the_frontend_reads() {
        fn keys(v: &serde_json::Value) -> Vec<&str> {
            let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
            k.sort_unstable();
            k
        }
        let v = serde_json::to_value(SyncSummary::default()).unwrap();
        assert_eq!(keys(&v), ["saves", "states"]);
        for lane in ["saves", "states"] {
            assert_eq!(keys(&v[lane]), ["archived", "conflicts", "downloaded", "failed", "skipped", "uploaded"], "{lane}");
        }
    }
}
