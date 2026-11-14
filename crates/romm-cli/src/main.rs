//! `romm`: command-line front end for romm-core. The future Tauri UI calls the same core functions.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use romm_core::api::RommClient;
use romm_core::config::{register_device, store_token, AppDirs, Config};
use romm_core::detect::Installed;
use romm_core::login::{self, Credentials};
use romm_core::report::TransferReport;
use romm_core::resolve::{Resolver, Unsyncable};
use romm_core::session::SyncCtx;
use romm_core::state::State;
use romm_core::sync::{ConflictPolicy, EmuEnv};
use romm_core::states::{self, StateOptions, StateStatus};
use romm_core::{library, platforms, profiles, session, unit, xbox};

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
}

impl App {
    fn load() -> Result<App> {
        let dirs = AppDirs::discover();
        let cfg = Config::load(&dirs)?;
        Ok(App { dirs, cfg })
    }

    fn client(&self) -> Result<RommClient> {
        self.cfg.client(&self.dirs).map_err(|e| anyhow!("{e}; run `romm login --server <url> --code <code>`"))
    }

    async fn device_id(&mut self, client: &RommClient) -> Result<String> {
        if let Some(id) = &self.cfg.device_id {
            return Ok(id.clone());
        }
        let id = register_device(client, self.cfg.device_name.as_deref()).await?;
        self.cfg.device_id = Some(id.clone());
        self.cfg.save(&self.dirs)?;
        Ok(id)
    }

    fn state(&self) -> Result<State> {
        Ok(State::load(&self.dirs.state_file())?)
    }

    fn resolver(&self) -> Resolver<'_> {
        Resolver::new(&self.cfg)
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
        if skip { None } else { self.cfg.state_options() }
    }

    fn policy(&self, flag: Option<OnConflict>) -> ConflictPolicy {
        flag.map(Into::into).unwrap_or(self.cfg.conflict_policy)
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

/// Which kind of sync a report is about; it decides the wording.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Saves,
    States,
}

fn print_report(r: &TransferReport, kind: Kind) {
    let (what, noun) = match kind {
        Kind::Saves => ("", "copy"),
        Kind::States => ("state ", "state"),
    };
    for f in &r.uploaded {
        println!("  ↑ {what}uploaded   {f}");
    }
    for f in &r.downloaded {
        println!("  ↓ {what}downloaded {f}");
    }
    for f in &r.archived {
        println!("  ⎘ archived your local {noun} on the server as {f}");
    }
    for c in &r.conflicts {
        println!("  ! {what}conflict   {} ({})", c.file_name, c.reason.as_deref().unwrap_or("both sides changed"));
    }
    for f in &r.skipped {
        println!("  – {what}skipped (too large) {f}");
    }
    for f in &r.failed {
        println!("  ✗ {what}failed     {f}");
    }
    if kind == Kind::Saves && r.uploaded.is_empty() && r.downloaded.is_empty() && r.conflicts.is_empty() && r.failed.is_empty() {
        println!("  saves are up to date");
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
            let credentials = Credentials::from_inputs(code.as_deref(), token.as_deref())?;
            let connected = login::connect(&server, credentials, insecure).await?;
            let client = &connected.client;
            app.cfg.set_server(client.base_url(), insecure, device_name);
            store_token(&app.dirs, &connected.token)?;
            app.cfg.save(&app.dirs)?;
            if let Some(w) = connected.scope_warning() {
                eprintln!("warning: {w}");
            }
            let id = app.device_id(client).await?;
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
            let mut p = progress_printer("Downloading BIOS".into());
            let (dir, res) = library::install_platform_bios(&c, &app.resolver(), &platform, &mut p).await?;
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
                        println!("✓ {:<12} {}\n    data: {}", p.name, inst.describe(), env.data_root.display());
                        if let Some(l) = &env.retroarch {
                            if l.sort_saves_by_core || l.sort_saves_by_content {
                                println!("    note: RetroArch sorts saves into subfolders");
                            }
                        }
                    }
                    Err(_) => println!("✗ {:<12} not found, set it with `romm emulator-set {} <path>`", p.name, p.id),
                }
                let claims = platforms::handled_by(&p);
                if !claims.is_empty() {
                    println!("    plays: {}", claims.join(", "));
                }
            }
        }

        Cmd::EmulatorSet { id, path } => {
            app.cfg.set_emulator_path(&id, Some(path))?;
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
            let (env, cmd, tuning) = (prepared.env, prepared.command, prepared.tuning);

            println!("Syncing saves, then launching {} in {}…", rom.name, env.profile.name);
            let st_opts = app.state_opts(no_states);
            let backups = app.dirs.backups_dir();
            let cx = SyncCtx { client: &c, device_id: &device, backup_dir: &backups, policy: app.policy(on_conflict), states: st_opts.as_ref() };
            // Settings chosen in the desktop app (Emulators & BIOS → Emulator settings) apply here
            // too, written just before the emulator starts, as the app does.
            let out = session::play(&cx, &mut state, &app.dirs.state_file(), &rom, &env, cmd, || {
                for note in tuning.apply(&app.dirs.emulator_config_backups()) {
                    println!("({note})");
                }
            })
            .await?;

            if let Some(note) = out.pull_note() {
                println!("{note}");
            } else if let Some(r) = &out.pull {
                print_report(r, Kind::Saves);
            }
            if let Some(r) = &out.state_pull {
                print_report(r, Kind::States);
            }
            println!("Played for {}s", out.elapsed.as_secs());
            if let Some(note) = out.push_note() {
                println!("{note}");
            } else if let Some(r) = &out.push {
                print_report(r, Kind::Saves);
            }
            if let Some(r) = &out.state_push {
                print_report(r, Kind::States);
            }
            if let Some(note) = out.states_note() {
                println!("{note}");
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
            let (backups, state_opts) = (app.dirs.backups_dir(), app.state_opts(no_states));
            let mut any_failed = false;
            for rom in roms {
                let env = match Resolver::new(&app.cfg).env_for_sync(&rom.platform_slug) {
                    Ok(env) => env,
                    // Nothing here keeps this platform's saves, so there is nothing to sync or to say.
                    Err(Unsyncable::NoEmulator(_)) => continue,
                    Err(e) => {
                        println!("{}: skipped ({e})", rom.name);
                        continue;
                    }
                };
                println!("{}", rom.name);
                let ctx = SyncCtx { client: &c, device_id: &device, backup_dir: &backups, policy: app.policy(on_conflict), states: state_opts.as_ref() };
                let round = ctx.sync_game(&mut state, &rom, &env, None).await;
                match &round.saves {
                    Ok(r) => {
                        any_failed |= !r.failed.is_empty();
                        print_report(r, Kind::Saves)
                    }
                    Err(e) => {
                        any_failed = true;
                        println!("  ✗ {e}");
                    }
                }
                match &round.states {
                    Some(Ok(r)) => {
                        any_failed |= !r.failed.is_empty();
                        print_report(r, Kind::States)
                    }
                    Some(Err(e)) => {
                        any_failed = true;
                        println!("  ✗ states: {e}");
                    }
                    None => {}
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
            let (views, unreachable) = states::overview_or_local(Some(&c), &state, &rom, &env).await?;
            if let Some(why) = unreachable {
                println!("(server unreachable: {why}; showing local files only)");
            }
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
