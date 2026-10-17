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
use romm_core::{library, profiles, retroarch, session};

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
enum Cmd {
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
        Ok(RommClient::new(url, Some(token), self.cfg.accept_invalid_certs)?)
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut app = App::load()?;

    match cli.cmd {
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
                bail!("RomM has no BIOS files for {} (add them under the platform's bios folder and rescan)", plat.title());
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
                                println!("    note: RetroArch sorts saves into subfolders; romm will still find them");
                            }
                        }
                    }
                    Err(_) => println!("✗ {:<12} not found (set it with `romm emulator-set {} <path>`)", p.name, p.id),
                }
                let claims: Vec<_> = p.platforms.iter().map(String::as_str).collect();
                if !claims.is_empty() {
                    println!("    plays: {}", claims.join(", "));
                }
            }
            let cores: Vec<_> = retroarch::DEFAULT_CORES.iter().map(|(s, _)| *s).collect();
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
            println!("Saved. Run `romm emulators` to check.");
        }

        Cmd::Play { rom_id, on_conflict, no_states } => {
            let c = app.client()?;
            let device = app.device_id(&c).await?;
            let mut state = app.state()?;
            let rom = state
                .roms
                .get(&rom_id)
                .cloned()
                .ok_or_else(|| anyhow!("game {rom_id} isn't installed; run `romm install {rom_id}`"))?;
            let prepared = app.resolver().prepare(&rom)?;
            let (env, cmd) = (prepared.env, prepared.command);

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
                println!("Couldn't upload saves ({e}). They're tracked and will upload on the next `romm sync`.");
            } else if let Some(r) = &out.push {
                print_report(r);
            }
            if let Some(r) = &out.state_push {
                print_state_report(r);
            }
            if let Some(e) = &out.state_error {
                println!("Save states weren't synced ({e}). They'll be picked up on the next `romm sync`.");
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
                let Ok(emu_id) = app.emulator_for(&rom.platform_slug) else { continue };
                let env = match app.emu_env(&emu_id) {
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
            let (env, _) = app.emu_env(&app.emulator_for(&rom.platform_slug)?)?;
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
