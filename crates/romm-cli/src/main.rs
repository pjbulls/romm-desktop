//! `romm`: command-line front end for romm-core. The future Tauri UI calls the same core functions.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use romm_core::api::types::DeviceRegistration;
use romm_core::api::{RommClient, CLIENT_SLUG};
use romm_core::config::{default_device_name, hostname, load_token, store_token, AppDirs, Config};
use romm_core::detect::{self, Installed};
use romm_core::paths::{Os, PathCtx};
use romm_core::resolve::Resolver;
use romm_core::state::State;
use romm_core::sync::{ConflictPolicy, EmuEnv, SyncReport, Syncer};
use romm_core::states::{self, StateOptions, StateReport, StateStatus};
use romm_core::{library, profiles, retroarch, session, unit, xbox};

/// Scopes a token needs for everything this app does.
const REQUIRED_SCOPES: &[&str] = &[
    "me.read",
    "platforms.read",
    "roms.read",
    "firmware.read",
    "assets.read",
    "assets.write",
    "devices.read",
    "devices.write",
];

#[derive(Parser)]
#[command(name = "romm", version, about = "Sync ROMs, BIOS files and saves with RomM and launch them in your emulators")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum OnConflict {
    /// Archive your local save on the server, then take the server's (default)
    KeepBoth,
    /// Take the server's save (local is backed up)
    Server,
    /// Overwrite the server's save with yours
    Device,
    /// Don't resolve; just report
    Ask,
}

impl From<OnConflict> for ConflictPolicy {
    fn from(o: OnConflict) -> Self {
        match o {
            OnConflict::KeepBoth => ConflictPolicy::KeepBoth,
            OnConflict::Server => ConflictPolicy::ServerWins,
            OnConflict::Device => ConflictPolicy::DeviceWins,
            OnConflict::Ask => ConflictPolicy::Ask,
        }
    }
}

#[derive(Subcommand)]
enum XboxCmd {
    /// List the title folders (E:\\UDATA\\<id>) on an xemu hard-disk image
    Titles {
        /// The xemu HDD image (a .qcow2). Close xemu first: the image is read without any lock
        image: PathBuf,
    },
    /// Write one title's save as Argosy's zip (<ID>/...), the same bytes RomM would store
    Export {
        image: PathBuf,
        /// Eight hex digits, e.g. 4D530004
        title: String,
        /// Where to write the zip (default: <ID>.zip here)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Say whether an image can be read and written right now (is xemu holding it open?)
    Status { image: PathBuf },
    /// Put a save zip (<ID>/..., as `export` writes) into the image, replacing that game's UDATA folder.
    /// All or nothing: a failure, or a crash, leaves the image exactly as it was. Close xemu first,
    /// and keep a copy of the image if it matters: this changes it
    Import {
        image: PathBuf,
        /// The save zip
        zip: PathBuf,
        /// The title id it is for (default: the zip's root folder)
        #[arg(long)]
        title: Option<String>,
    },
}

#[derive(Subcommand)]
enum Cmd {
    /// Inspect saves inside an xemu disk image, or put one back
    Xbox {
        #[command(subcommand)]
        cmd: XboxCmd,
    },
    /// Connect to a RomM server (use the 8-digit pairing code from RomM's web UI, or a token)
    Login {
        #[arg(long)]
        server: String,
        /// Pairing code from Profile → Client API Tokens → Pair
        #[arg(long, conflicts_with = "token")]
        code: Option<String>,
        /// An existing rmm_… Client API Token
        #[arg(long)]
        token: Option<String>,
        /// Accept self-signed HTTPS certificates
        #[arg(long)]
        insecure: bool,
        /// Name shown in RomM's device list
        #[arg(long)]
        device_name: Option<String>,
    },
    /// List platforms on the server
    Platforms,
    /// List games
    Games {
        /// Platform slug, e.g. psx, ps2, psp, ngc, wii, snes
        #[arg(long)]
        platform: Option<String>,
        #[arg(long)]
        search: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Download a game
    Install { rom_id: i64 },
    /// Download BIOS/firmware for a platform into the right emulator folder
    Bios { platform: String },
    /// Show detected emulators, their data folders, and which platforms they handle
    Emulators,
    /// Tell romm where an emulator's executable is
    EmulatorSet { id: String, path: PathBuf },
    /// Pull saves, launch the game, push saves afterwards
    Play {
        rom_id: i64,
        #[arg(long, value_enum)]
        on_conflict: Option<OnConflict>,
        /// Skip save states this time
        #[arg(long)]
        no_states: bool,
    },
    /// Sync saves and save states for installed games without launching anything
    Sync {
        rom_id: Option<i64>,
        #[arg(long, value_enum)]
        on_conflict: Option<OnConflict>,
        /// Skip save states this time
        #[arg(long)]
        no_states: bool,
    },
    /// Show a game's save states and how they compare with the server
    States { rom_id: i64 },
}

struct App {
    dirs: AppDirs,
    cfg: Config,
    os: Os,
    pctx: PathCtx,
}

impl App {
    fn load() -> Result<App> {
        let dirs = AppDirs::discover();
        let cfg = Config::load(&dirs)?;
        Ok(App { dirs, cfg, os: Os::current(), pctx: PathCtx::from_env() })
    }

    fn client(&self) -> Result<RommClient> {
        let url = self.cfg.server_url.as_deref().ok_or_else(|| anyhow!("not logged in; run `romm login --server <url> --code <code>`"))?;
        let token = load_token(&self.dirs).ok_or_else(|| anyhow!("no saved token; run `romm login` again"))?;
        Ok(RommClient::new(url, Some(token), self.cfg.accept_invalid_certs)?.with_process_lock(self.dirs.api_lock()))
    }

    async fn device_id(&mut self, client: &RommClient) -> Result<String> {
        if let Some(id) = &self.cfg.device_id {
            return Ok(id.clone());
        }
        let name = self.cfg.device_name.clone().unwrap_or_else(default_device_name);
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
            .context("registering this device with RomM (does the token have the devices.write scope?)")?;
        self.cfg.device_id = Some(d.device_id.clone());
        self.cfg.save(&self.dirs)?;
        Ok(d.device_id)
    }

    fn state(&self) -> Result<State> {
        Ok(State::load(&self.dirs.state_file())?)
    }

    fn resolver(&self) -> Resolver<'_> {
        Resolver { cfg: &self.cfg, os: self.os, pctx: self.pctx.clone() }
    }

    fn emu_env(&self, emu_id: &str) -> Result<(EmuEnv, Installed)> {
        Ok(self.resolver().emu_env(emu_id)?)
    }

    /// The environment for the emulator that plays `slug`, with the RetroArch core filled in: what a
    /// game's save is depends on it.
    fn env_for_platform(&self, slug: &str) -> Result<(EmuEnv, Installed)> {
        Ok(self.resolver().env_for_platform(slug)?)
    }

    fn state_opts(&self, skip: bool) -> Option<StateOptions> {
        (self.cfg.sync_states && !skip).then(|| StateOptions { max_bytes: self.cfg.max_state_mb * 1024 * 1024 })
    }

    fn policy(&self, flag: Option<OnConflict>) -> ConflictPolicy {
        flag.map(Into::into).unwrap_or(self.cfg.conflict_policy)
    }

    fn emulator_for(&self, slug: &str) -> Result<String> {
        Ok(self.resolver().emulator_for(slug)?)
    }
}

fn progress_printer(label: String) -> impl FnMut(u64, Option<u64>) {
    let mut last_pct = u64::MAX;
    move |done, total| {
        let pct = total.filter(|t| *t > 0).map(|t| done * 100 / t);
        if pct != Some(last_pct) {
            last_pct = pct.unwrap_or(0);
            match pct {
                Some(p) => eprint!("\r{label}: {p:>3}%"),
                None => eprint!("\r{label}: {} KiB", done / 1024),
            }
            let _ = std::io::stderr().flush();
        }
    }
}

fn print_report(r: &SyncReport) {
    for f in &r.uploaded {
        println!("  ↑ uploaded   {f}");
    }
    for f in &r.downloaded {
        println!("  ↓ downloaded {f}");
    }
    for f in &r.archived {
        println!("  ⎘ archived your local copy on the server as {f}");
    }
    for c in &r.conflicts {
        println!("  ! conflict   {} ({})", c.file_name, c.reason.as_deref().unwrap_or("both sides changed"));
    }
    for f in &r.failed {
        println!("  ✗ failed     {f}");
    }
    if r.uploaded.is_empty() && r.downloaded.is_empty() && r.conflicts.is_empty() && r.failed.is_empty() {
        println!("  saves are up to date");
    }
}

fn print_state_report(r: &StateReport) {
    for f in &r.uploaded {
        println!("  ↑ state uploaded   {f}");
    }
    for f in &r.downloaded {
        println!("  ↓ state downloaded {f}");
    }
    for f in &r.archived {
        println!("  ⎘ archived your local state on the server as {f}");
    }
    for f in &r.conflicts {
        println!("  ! state conflict   {f}");
    }
    for f in &r.skipped {
        println!("  – state skipped (too large) {f}");
    }
    for f in &r.failed {
        println!("  ✗ state failed     {f}");
    }
}

fn xbox_cmd(cmd: XboxCmd) -> Result<()> {
    if let XboxCmd::Import { image, zip, title } = &cmd {
        let bytes = std::fs::read(zip).with_context(|| format!("reading {}", zip.display()))?;
        let id = match title {
            Some(t) => t.to_ascii_uppercase(),
            None => xbox::zip_title_id(&bytes).ok_or_else(|| anyhow!("can't tell which game this zip is for, provide --title"))?,
        };
        let s = xbox::import_save(image, &id, &bytes).with_context(|| format!("importing into {}", image.display()))?;
        println!("{id}: {} file(s), {} folder(s), {} bytes written", s.files, s.dirs, s.bytes);
        return Ok(());
    }
    if let XboxCmd::Status { image } = &cmd {
        println!("image: {}", image.display());
        match xbox::image_in_use(image) {
            Some(why) => println!("open elsewhere:  YES, {why}"),
            None => println!("open elsewhere:  no"),
        }
        match xbox::check_writable(image) {
            Ok(()) => println!("can be written:  yes"),
            Err(e) => println!("can be written:  NO, {e}"),
        }
        println!("interrupted write pending: {}", if xbox::qcow2::journal_path(image).exists() { "YES, it is undone the next time the image is opened" } else { "no" });
        return Ok(());
    }
    let image = match &cmd {
        XboxCmd::Titles { image } | XboxCmd::Export { image, .. } => image,
        XboxCmd::Import { .. } | XboxCmd::Status { .. } => unreachable!("handled above"),
    };
    if let Some(why) = xbox::image_in_use(image) {
        eprintln!("warning: the image is in use ({why}). Close xemu first");
    }
    let mut hdd = xbox::FileHdd::open(image).with_context(|| format!("reading {}", image.display()))?;
    if hdd.device().is_dirty() {
        eprintln!("warning: QEMU marked this image dirty (not closed cleanly, or xemu is running it)");
    }
    match cmd {
        XboxCmd::Titles { .. } => {
            let titles = hdd.titles()?;
            for t in &titles {
                match hdd.export_save(&t.save_id) {
                    Ok(Some(save)) => {
                        let age = save.modified.map_or("no usable date".to_string(), |m| format!("saved {}", chrono::DateTime::<chrono::Local>::from(m).format("%Y-%m-%d %H:%M")));
                        println!("{}  UDATA\\{}  (zip {} bytes, {age})", t.save_id, t.dir_name, save.zip.len());
                    }
                    Ok(None) => println!("{}  UDATA\\{}  (no save files)", t.save_id, t.dir_name),
                    Err(e) => println!("{}  UDATA\\{}  (unreadable: {e})", t.save_id, t.dir_name),
                }
            }
            println!("{} title folder(s)", titles.len());
        }
        XboxCmd::Export { title, output, .. } => {
            let id = title.to_ascii_uppercase();
            let zip = hdd.export_zip(&id)?.ok_or_else(|| anyhow!("{id} has no save files on this disk"))?;
            let out = output.unwrap_or_else(|| PathBuf::from(format!("{id}.zip")));
            std::fs::write(&out, &zip).with_context(|| format!("writing {}", out.display()))?;
            println!("{}  {} bytes  content_hash {}", out.display(), zip.len(), unit::content_hash(&zip));
        }
        XboxCmd::Import { .. } | XboxCmd::Status { .. } => unreachable!("handled above"),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Cmd::Xbox { cmd } = cli.cmd {
        return xbox_cmd(cmd);
    }
    let mut app = App::load()?;

    match cli.cmd {
        Cmd::Xbox { .. } => unreachable!("handled before the app config is loaded"),
        Cmd::Login { server, code, token, insecure, device_name } => {
            let (raw, scopes) = match (code, token) {
                (Some(code), None) => {
                    let t = RommClient::exchange_pair_code(&server, &code, insecure)
                        .await
                        .context("pairing failed (codes are single-use and expire after 5 minutes)")?;
                    (t.raw_token, Some(t.scopes))
                }
                (None, Some(t)) => (t, None),
                _ => bail!("give either --code or --token"),
            };
            let client = RommClient::new(&server, Some(raw.clone()), insecure)?;
            client.platforms().await.context("the token was accepted but listing platforms failed")?;
            app.cfg.server_url = Some(client.base_url().to_string());
            app.cfg.accept_invalid_certs = insecure;
            app.cfg.device_name = device_name;
            app.cfg.device_id = None;
            store_token(&app.dirs, &raw)?;
            app.cfg.save(&app.dirs)?;
            if let Some(scopes) = scopes {
                let missing: Vec<_> = REQUIRED_SCOPES.iter().filter(|s| !scopes.iter().any(|x| x == *s)).collect();
                if !missing.is_empty() {
                    eprintln!("warning: this token lacks scopes romm needs: {missing:?}\n         create a new token with them in RomM → Profile → Client API Tokens");
                }
            }
            let id = app.device_id(&client).await?;
            println!("Connected to {} (device {id})", client.base_url());
        }

        Cmd::Platforms => {
            let c = app.client()?;
            for p in c.platforms().await? {
                println!("{:<24} {:<28} {:>5} games  {:>2} BIOS files", p.slug, p.title(), p.rom_count, p.firmware.len());
            }
        }

        Cmd::Games { platform, search, limit } => {
            let c = app.client()?;
            let state = app.state()?;
            let pid = match &platform {
                Some(slug) => Some(
                    c.platforms()
                        .await?
                        .into_iter()
                        .find(|p| &p.slug == slug)
                        .ok_or_else(|| anyhow!("no platform with slug '{slug}' (see `romm platforms`)"))?
                        .id,
                ),
                None => None,
            };
            let page = c.roms(pid, search.as_deref(), limit, 0).await?;
            for r in &page.items {
                let mark = if state.roms.contains_key(&r.id) { "✓" } else { " " };
                println!("{mark} {:>6}  {:<12} {}", r.id, r.platform_slug, r.title());
            }
            if let Some(t) = page.total {
                println!("({} of {t})", page.items.len());
            }
        }

        Cmd::Install { rom_id } => {
            let c = app.client()?;
            let rom = c.rom(rom_id).await?;
            let mut p = progress_printer(format!("Downloading {}", rom.title()));
            let local = library::install_rom(&c, &rom, &app.cfg.rom_dir(), &mut p).await?;
            eprintln!();
            let mut state = app.state()?;
            println!("Installed to {}", local.path.display());
            state.roms.insert(rom_id, local);
            state.save(&app.dirs.state_file())?;
        }

        Cmd::Bios { platform } => {
            let c = app.client()?;
            let plat = c
                .platforms()
                .await?
                .into_iter()
                .find(|p| p.slug == platform)
                .ok_or_else(|| anyhow!("no platform with slug '{platform}'"))?;
            if plat.firmware.is_empty() {
                bail!("RomM has no BIOS files for {}", plat.title());
            }
            let emu_id = app.emulator_for(&platform)?;
            let (env, _) = app.emu_env(&emu_id)?;
            let rel = env.profile.bios.as_ref().ok_or_else(|| anyhow!("{} has no known BIOS folder", env.profile.name))?.dir.clone();
            let dir = match &env.retroarch {
                Some(l) => l.system.clone(),
                None => env.data_root.join(rel),
            };
            let mut p = progress_printer("Downloading BIOS".into());
            let res = library::install_bios(&c, &plat, &dir, &mut p).await?;
            eprintln!();
            for r in res {
                println!("  {:?}: {}", r.status, r.file);
            }
            println!("BIOS folder: {}", dir.display());
        }

        Cmd::Emulators => {
            for p in profiles::builtin() {
                match app.emu_env(&p.id) {
                    Ok((env, inst)) => {
                        let how = match &inst.kind {
                            detect::InstallKind::Native(x) => x.display().to_string(),
                            detect::InstallKind::Flatpak(id) => format!("flatpak {id}"),
                        };
                        println!("✓ {:<12} {how}\n    data: {}", p.name, env.data_root.display());
                        if let Some(l) = &env.retroarch {
                            if l.sort_saves_by_core || l.sort_saves_by_content {
                                println!("    note: RetroArch sorts saves into subfolders");
                            }
                        }
                    }
                    Err(_) => println!("✗ {:<12} not found, set it with `romm emulator-set {} <path>`", p.name, p.id),
                }
                let claims: Vec<_> = p.platforms.iter().map(String::as_str).collect();
                if !claims.is_empty() {
                    println!("    plays: {}", claims.join(", "));
                }
            }
            let cores: Vec<_> = retroarch::supported_slugs().collect();
            println!("RetroArch default platforms: {}", cores.join(", "));
        }

        Cmd::EmulatorSet { id, path } => {
            if profiles::get(&id).is_none() {
                bail!("unknown emulator '{id}' (known: {})", profiles::builtin().iter().map(|p| p.id.clone()).collect::<Vec<_>>().join(", "));
            }
            if !path.is_file() {
                bail!("{} is not a file", path.display());
            }
            app.cfg.emulator_paths.insert(id.clone(), path);
            app.cfg.save(&app.dirs)?;
            println!("Saved, run `romm emulators` to check.");
        }

        Cmd::Play { rom_id, on_conflict, no_states } => {
            let c = app.client()?;
            let device = app.device_id(&c).await?;
            let mut state = app.state()?;
            let rom = state
                .roms
                .get(&rom_id)
                .cloned()
                .ok_or_else(|| anyhow!("game {rom_id} isn't installed, run `romm install {rom_id}`"))?;
            let prepared = app.resolver().prepare(&rom)?;
            let (env, cmd) = (prepared.env, prepared.command);
            // Settings chosen in the desktop app (Settings → Emulation) apply here too.
            for note in prepared.tuning.apply(&app.dirs.emulator_config_backups()) {
                println!("({note})");
            }

            println!("Syncing saves, then launching {} in {}…", rom.name, env.profile.name);
            let st_opts = app.state_opts(no_states);
            let out = session::play(&c, &device, &mut state, &app.dirs.state_file(), app.dirs.backups_dir(), &rom, &env, cmd, app.policy(on_conflict), st_opts.as_ref()).await?;

            if let Some(e) = &out.pull_error {
                println!("(couldn't sync before launch — offline? {e})");
            } else if let Some(r) = &out.pull {
                print_report(r);
            }
            if let Some(r) = &out.state_pull {
                print_state_report(r);
            }
            println!("Played for {}s", out.elapsed.as_secs());
            if let Some(e) = &out.push_error {
                println!("Couldn't sync saves ({e}), will try again on the next `romm sync`.");
            } else if let Some(r) = &out.push {
                print_report(r);
            }
            if let Some(r) = &out.state_push {
                print_state_report(r);
            }
            if let Some(e) = &out.state_error {
                println!("Couldn't syunc save states ({e}), will try again on the next `romm sync`.");
            }
        }

        Cmd::Sync { rom_id, on_conflict, no_states } => {
            let c = app.client()?;
            let device = app.device_id(&c).await?;
            let mut state = app.state()?;
            let roms: Vec<_> = state.roms.values().filter(|r| rom_id.is_none_or(|id| id == r.rom_id)).cloned().collect();
            if roms.is_empty() {
                bail!("nothing installed to sync");
            }
            let mut any_failed = false;
            for rom in roms {
                if app.emulator_for(&rom.platform_slug).is_err() {
                    continue;
                }
                let env = match app.env_for_platform(&rom.platform_slug) {
                    Ok((e, _)) => e,
                    Err(e) => {
                        println!("{}: skipped ({e})", rom.name);
                        continue;
                    }
                };
                println!("{}", rom.name);
                let policy = app.policy(on_conflict);
                let mut s = Syncer { client: &c, device_id: &device, state: &mut state, backup_dir: app.dirs.backups_dir(), policy };
                match s.sync_rom(&rom, &env, None).await {
                    Ok(r) => {
                        any_failed |= !r.failed.is_empty();
                        print_report(&r)
                    }
                    Err(e) => {
                        any_failed = true;
                        println!("  ✗ {e}");
                    }
                }
                if let Some(opts) = app.state_opts(no_states) {
                    match states::sync_states(&c, &mut state, &app.dirs.backups_dir(), &rom, &env, policy, &opts).await {
                        Ok(r) => {
                            any_failed |= !r.failed.is_empty();
                            print_state_report(&r)
                        }
                        Err(e) => {
                            any_failed = true;
                            println!("  ✗ states: {e}");
                        }
                    }
                }
                state.save(&app.dirs.state_file())?;
            }
            if any_failed {
                std::process::exit(1);
            }
        }

        Cmd::States { rom_id } => {
            let c = app.client()?;
            let state = app.state()?;
            let rom = state.roms.get(&rom_id).cloned().ok_or_else(|| anyhow!("game {rom_id} isn't installed"))?;
            let (env, _) = app.env_for_platform(&rom.platform_slug)?;
            let views = match states::overview(Some(&c), &state, &rom, &env).await {
                Ok(v) => v,
                Err(e) => {
                    println!("(server unreachable: {e}; showing local files only)");
                    states::overview(None, &state, &rom, &env).await?
                }
            };
            if views.is_empty() {
                println!("No save states for {} yet.", rom.name);
            }
            for v in views {
                let label = match v.status {
                    StateStatus::InSync => "in sync",
                    StateStatus::LocalNewer => "local is newer",
                    StateStatus::ServerNewer => "server is newer",
                    StateStatus::LocalOnly => "local only",
                    StateStatus::ServerOnly => "server only",
                    StateStatus::Conflict => "CONFLICT",
                    StateStatus::Unverified => "not compared yet",
                };
                println!("  {:<40} {label}", v.name);
            }
        }
    }
    Ok(())
}
