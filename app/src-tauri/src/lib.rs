//! Tauri backend. Every command is a thin wrapper over `romm-core`; the long-running ones
//! (install, play, sync) run as background jobs and report through `romm://…` events.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use base64::Engine;
use romm_core::api::types::{DeviceRegistration, Platform, Rom};
use romm_core::api::{RommClient, CLIENT_SLUG};
use romm_core::config::{default_device_name, hostname, load_token, store_token, AppDirs, Config};
use romm_core::library::{self, BiosStatus};
use romm_core::resolve::Resolver;
use romm_core::state::{LocalRom, State};
use romm_core::states::{self, StateOptions, StateReport, StateView};
use romm_core::sync::{saves_overview, ConflictPolicy, SaveView, SyncReport, Syncer};
use romm_core::{profiles, retroarch, session};
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

fn game_dto(r: &Rom, installed: bool) -> GameDto {
    GameDto {
        id: r.id,
        name: r.title().to_string(),
        platform_id: r.platform_id,
        platform_slug: r.platform_slug.clone(),
        installed,
        size_bytes: r.fs_size_bytes,
        multi_file: r.has_multiple_files,
        cover_path: r.path_cover_small.clone(),
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
    Ok(list
        .into_iter()
        .map(|p| {
            let emu = resolver.emulator_for(&p.slug).ok();
            let profile = emu.as_deref().and_then(profiles::get);
            PlatformDto {
                id: p.id,
                name: p.title().to_string(),
                rom_count: p.rom_count,
                firmware_count: p.firmware.len(),
                emulator_installed: profile.as_ref().is_some_and(|pr| resolver.installed(pr).is_some()),
                emulator_name: profile.map(|pr| pr.name),
                emulator: emu,
                slug: p.slug,
            }
        })
        .collect())
}

#[tauri::command]
async fn games(st: TauriState<'_, AppState>, args: GamesArgs) -> CmdResult<GamesPage> {
    if args.installed_only {
        // Served from the local database so it works offline and needs no paging.
        let db = st.db.lock().await;
        let needle = args.search.clone().unwrap_or_default().to_lowercase();
        let items: Vec<GameDto> = db
            .roms
            .values()
            .filter(|r| needle.is_empty() || r.name.to_lowercase().contains(&needle))
            .map(|r| GameDto {
                id: r.rom_id,
                name: r.name.clone(),
                platform_id: 0,
                platform_slug: r.platform_slug.clone(),
                installed: true,
                size_bytes: 0,
                multi_file: false,
                cover_path: r.cover_path.clone(),
            })
            .collect();
        let total = Some(items.len() as i64);
        return Ok(GamesPage { items, total });
    }
    let client = st.client().await?;
    let page = client.roms(args.platform_id, args.search.as_deref(), args.limit, args.offset).await.map_err(s)?;
    let db = st.db.lock().await;
    Ok(GamesPage { items: page.items.iter().map(|r| game_dto(r, db.roms.contains_key(&r.id))).collect(), total: page.total })
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
                retroarch::DEFAULT_CORES.iter().map(|(slug, _)| slug.to_string()).collect()
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

#[tauri::command]
async fn set_platform_emulator(st: TauriState<'_, AppState>, slug: String, emulator: Option<String>) -> CmdResult<()> {
    st.update_cfg(|c| match emulator {
        Some(e) => {
            c.platform_emulators.insert(slug, e);
        }
        None => {
            c.platform_emulators.remove(&slug);
        }
    })
    .await
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
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status,
            login,
            logout,
            platforms,
            games,
            cover,
            install_game,
            uninstall_game,
            play_game,
            sync_game,
            sync_all,
            game_details,
            emulators,
            set_emulator_path,
            set_platform_emulator,
            install_bios,
            settings,
            save_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running RomM Desktop");
}
