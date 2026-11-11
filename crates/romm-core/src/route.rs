//! Which of an emulator's files are a game's save, and how they travel to RomM.
//!
//! This is Argosy's routing, so a save uploaded from here is one Argosy can restore and the other
//! way round (`SigilSaveHandler`'s layout tables in the Argosy sources):
//!
//! * Standalone emulators each have a named Sigil layout (`ppsspp_standalone`, `dolphin_standalone`,
//!   `duckstation`, `pcsx2_standalone`, `azahar`) and the save is a *unit* cut out of the
//!   emulator's files by game id.
//! * RetroArch cores that Sigil has a layout for (Game Boy, N64, Dreamcast, Mega-CD) travel as
//!   units too. Every other core keeps one file per game, `<stem>.srm` (or `.sav`), which travels
//!   as it is.
//! * The `emulator` label RomM records is the libretro core's name for RetroArch, and the
//!   emulator's own id otherwise (`dolphin` for both GameCube and Wii). Argosy compares it with
//!   the emulator it is about to launch to decide whether a save needs converting.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::paths::PathCtx;
use crate::profiles::{SaveMode, RETROARCH};
use crate::retroarch;
use crate::state::LocalRom;
use crate::sync::EmuEnv;
use crate::unit::{self, Identity, Route};
use crate::xbox;
use crate::xenia;

#[derive(Debug, Clone)]
pub enum Kind {
    /// Sigil decides what the save is, from the emulator's files.
    Unit(Route),
    /// One file, as the core wrote it.
    Raw { path: PathBuf, exists: bool },
    /// The `E:\UDATA\<title id>` folder inside an xemu hard-disk image, read out as a zip.
    /// Read-only: nothing is ever written back to the image.
    Xbox {
        image: PathBuf,
        /// Eight hex digits, from the ROM's XBE; empty when the ROM could not be read.
        save_id: String,
    },
    /// A folder the emulator keeps per game, zipped the way Argosy zips it (Xenia: `xenia.rs`).
    /// `title_id` is empty when the ROM could not be identified, which `sync::gather` reports.
    Folder { content: PathBuf, title_id: String },
}

#[derive(Debug, Clone)]
pub struct Plan {
    /// RomM's `emulator` for saves uploaded from this setup.
    pub label: String,
    pub kind: Kind,
    /// File name of what the emulator loads; Sigil names RetroArch-style saves after its stem.
    pub content_name: String,
}

/// Libretro cores Argosy routes through Sigil when they run under RetroArch, by Sigil platform.
/// Cores not listed here (snes9x, genesis_plus_gx on Mega Drive, pcsx_rearmed, ...) travel raw.
const RETROARCH_UNIT_CORES: &[(&str, &[&str])] = &[
    ("gb", &["gambatte", "mgba", "vbam", "sameboy", "tgbdual"]),
    ("gbc", &["gambatte", "mgba", "vbam", "sameboy", "tgbdual"]),
    ("n64", &["mupen64plus_next", "parallel_n64"]),
    ("dreamcast", &["flycast"]),
    ("segacd", &["genesis_plus_gx"]),
    ("saturn", &["mednafen_saturn", "kronos", "yabause", "yabasanshiro"]),
];

pub fn is_unit_core(sigil_platform: &str, core: &str) -> bool {
    RETROARCH_UNIT_CORES.iter().any(|(p, cores)| *p == sigil_platform && cores.contains(&core))
}

/// The plan for `rom` under `env`, or `None` when this emulator has no save rule or no way to run
/// the platform.
pub fn plan(env: &EmuEnv, rom: &LocalRom, id: &Identity) -> Option<Plan> {
    env.profile.saves.as_ref()?;
    let content_name = rom
        .launch_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| rom.stem.clone());
    if env.profile.id == RETROARCH {
        return retroarch_plan(env, rom, id, content_name);
    }
    if env.profile.saves.as_ref().is_some_and(|r| r.mode == SaveMode::Hdd) {
        return xbox_plan(env, rom, id, content_name);
    }
    if env.profile.saves.as_ref().is_some_and(|r| r.mode == SaveMode::Folder) {
        return xenia_plan(env, rom, id, content_name);
    }
    let route = standalone_route(env, rom)?;
    Some(Plan { label: env.profile.id.clone(), kind: Kind::Unit(route), content_name })
}

// ---------------------------------------------------------------------------------------------
// Emulators with a virtual hard disk (xemu)
// ---------------------------------------------------------------------------------------------

/// The save of an original Xbox game is its `UDATA` folder on the emulator's disk image. Which
/// folder is the game's comes from the title id in its XBE, which Sigil reads (`save_id`, the
/// 8-digit hex the console names the folder after).
fn xbox_plan(env: &EmuEnv, rom: &LocalRom, id: &Identity, content_name: String) -> Option<Plan> {
    if unit::platform_slug(&rom.platform_slug) != "xbox" {
        return None;
    }
    let kind = Kind::Xbox { image: xbox::xemu_hdd_path(&env.data_root), save_id: id.save_id.to_ascii_uppercase() };
    Some(Plan { label: env.profile.id.clone(), kind, content_name })
}

// ---------------------------------------------------------------------------------------------
// Emulators that keep a folder per game (Xenia)
// ---------------------------------------------------------------------------------------------

/// The saves of an Xbox 360 game are its `00000001` folder under Xenia's content root. Which folder
/// is the game's comes from the title id in its `default.xex`, which Sigil reads; it is empty when
/// the ROM could not be read, and `sync::gather` then says why.
fn xenia_plan(env: &EmuEnv, rom: &LocalRom, id: &Identity, content_name: String) -> Option<Plan> {
    if unit::platform_slug(&rom.platform_slug) != "xbox360" {
        return None;
    }
    let kind = Kind::Folder { content: xenia::content_root(&env.data_root), title_id: xenia::title_id_of(id).unwrap_or_default() };
    Some(Plan { label: env.profile.id.clone(), kind, content_name })
}

// ---------------------------------------------------------------------------------------------
// Standalone emulators
// ---------------------------------------------------------------------------------------------

fn standalone_route(env: &EmuEnv, rom: &LocalRom) -> Option<Route> {
    let platform = unit::platform_slug(&rom.platform_slug);
    let root = env.data_root.clone();
    let (layout, options) = match (env.profile.id.as_str(), platform.as_str()) {
        ("ppsspp", "psp") => ("ppsspp_standalone", vec![]),
        ("dolphin", "gamecube" | "wii") => ("dolphin_standalone", dolphin_options(&root)),
        ("duckstation", "psx") => ("duckstation", duckstation_options(&root)),
        ("pcsx2", "ps2") => ("pcsx2_standalone", pcsx2_options(&root)),
        ("azahar", "3ds") => ("azahar", vec![]),
        _ => return None,
    };
    Some(Route { layout: layout.into(), platform, root, options })
}

/// `key = value` lines of one `[section]` of an INI file; empty when the file or section is absent.
pub fn ini_section(text: &str, section: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut inside = false;
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            inside = name == section;
        } else if inside {
            if let Some((k, v)) = line.split_once('=') {
                out.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    out
}

/// The values of `keys` in `section`, keeping only ones the layout understands. `allowed` of `None`
/// accepts any non-empty value (a card's file name).
fn settings(text: &str, section: &str, keys: &[(&str, Option<&[&str]>)]) -> Vec<(String, String)> {
    let map = ini_section(text, section);
    keys.iter()
        .filter_map(|(k, allowed)| {
            let v = map.get(*k)?.clone();
            let ok = !v.is_empty() && allowed.is_none_or(|a| a.contains(&v.as_str()));
            ok.then(|| (k.to_string(), v))
        })
        .collect()
}

fn read(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

const CARD_TYPES: &[&str] = &["PerGameTitle", "PerGame", "PerGameFileTitle", "Shared"];

/// DuckStation keeps its settings in the data folder.
fn duckstation_options(root: &Path) -> Vec<(String, String)> {
    let Some(text) = read(&root.join("settings.ini")) else { return vec![] };
    settings(&text, "MemoryCards", &[("Card1Type", Some(CARD_TYPES)), ("Card2Type", Some(CARD_TYPES))])
}

/// PCSX2 names the cards in `inis/PCSX2.ini`; without these the layout assumes `Mcd001.ps2`.
fn pcsx2_options(root: &Path) -> Vec<(String, String)> {
    let Some(text) = read(&root.join("inis/PCSX2.ini")) else { return vec![] };
    settings(&text, "MemoryCards", &[("Slot1_Filename", None), ("Slot2_Filename", None)])
}

/// Dolphin's `Dolphin.ini` is in `Config/` of its user folder, or, on a Linux install that splits
/// config from data (XDG, Flatpak), in the config folder beside the data one.
fn dolphin_options(root: &Path) -> Vec<(String, String)> {
    let mut candidates = vec![root.join("Config/Dolphin.ini")];
    if let (Some(parent), Some(name)) = (root.parent(), root.file_name()) {
        // .../data/dolphin-emu  ->  .../config/dolphin-emu   (Flatpak)
        if parent.file_name().is_some_and(|n| n == "data") {
            if let Some(app) = parent.parent() {
                candidates.push(app.join("config").join(name).join("Dolphin.ini"));
            }
        }
    }
    if let Some(cfg) = PathCtx::from_env().xdg_config {
        candidates.push(cfg.join("dolphin-emu/Dolphin.ini"));
    }
    let Some(text) = candidates.iter().find_map(|p| read(p)) else { return vec![] };
    settings(
        &text,
        "Core",
        &[("SlotA", Some(&["8", "1"])), ("MemoryCardSize", Some(&["-1", "4", "3", "2", "1", "0"]))],
    )
}

// ---------------------------------------------------------------------------------------------
// RetroArch
// ---------------------------------------------------------------------------------------------

fn retroarch_plan(env: &EmuEnv, rom: &LocalRom, id: &Identity, content_name: String) -> Option<Plan> {
    let layout = env.retroarch.as_ref()?;
    let core = env.core.clone().or_else(|| retroarch::default_core(&rom.platform_slug).map(str::to_string))?;
    let platform = unit::platform_slug(&rom.platform_slug);
    let dirs = retroarch::save_dirs(layout, &core, &rom.launch_path);

    if is_unit_core(&platform, &core) {
        let options = retroarch::core_options(layout, &core);
        let mk = |root: &Path| Route { layout: core.clone(), platform: platform.clone(), root: root.to_path_buf(), options: options.clone() };
        // The first folder that actually holds this game's saves; failing that, where RetroArch
        // would put them (the first one that exists, else the first it could use).
        let existing: Vec<&PathBuf> = dirs.iter().filter(|d| d.is_dir()).collect();
        let root = if existing.len() > 1 {
            existing
                .iter()
                .find(|d| matches!(unit::collect(&mk(d), id, &content_name, None), Ok(Some(_))))
                .copied()
                .unwrap_or(existing[0])
        } else {
            existing.first().copied().unwrap_or(&dirs[0])
        };
        return Some(Plan { label: core.clone(), kind: Kind::Unit(mk(root)), content_name });
    }

    let exts = env.profile.saves.as_ref().map(|r| r.extensions.clone()).filter(|e| !e.is_empty()).unwrap_or_else(|| vec!["srm".into()]);
    let (path, exists) = match retroarch::find_raw_save(layout, &rom.stem, &exts) {
        Some(p) => (p, true),
        None => (dirs[0].join(format!("{}.{}", rom.stem, exts[0])), false),
    };
    Some(Plan { label: core, kind: Kind::Raw { path, exists }, content_name })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{Os, PathCtx};
    use crate::profiles;

    fn rom(stem: &str, platform: &str) -> LocalRom {
        LocalRom {
            rom_id: 1,
            platform_slug: platform.into(),
            launch_path: PathBuf::from(format!("/roms/{platform}/{stem}.rom")),
            stem: stem.into(),
            ..Default::default()
        }
    }

    fn env(id: &str, root: &Path) -> EmuEnv {
        let retro = (id == "retroarch").then(|| retroarch::layout(root, Os::Linux, &PathCtx::default()));
        EmuEnv { profile: profiles::get(id).unwrap(), data_root: root.to_path_buf(), retroarch: retro, core: None }
    }

    fn unit_route(p: &Plan) -> &Route {
        match &p.kind {
            Kind::Unit(r) => r,
            other => panic!("expected a unit, got {other:?}"),
        }
    }

    #[test]
    fn every_standalone_emulator_routes_to_the_layout_argosy_uses() {
        let t = tempfile::tempdir().unwrap();
        for (emu, platform, layout, sigil_platform) in [
            ("ppsspp", "psp", "ppsspp_standalone", "psp"),
            ("dolphin", "ngc", "dolphin_standalone", "gamecube"),
            ("dolphin", "wii", "dolphin_standalone", "wii"),
            ("duckstation", "psx", "duckstation", "psx"),
            ("pcsx2", "ps2", "pcsx2_standalone", "ps2"),
            ("azahar", "3ds", "azahar", "3ds"),
            ("azahar", "new-nintendo-3ds", "azahar", "3ds"),
        ] {
            let p = plan(&env(emu, t.path()), &rom("g", platform), &Identity::default()).unwrap_or_else(|| panic!("{emu}/{platform}"));
            let r = unit_route(&p);
            assert_eq!((r.layout.as_str(), r.platform.as_str()), (layout, sigil_platform), "{emu}/{platform}");
            assert_eq!(r.root, t.path());
            // Argosy uploads under the emulator's own id (Dolphin is `dolphin` for Wii too).
            assert_eq!(p.label, emu);
        }
    }

    #[test]
    fn an_emulator_is_not_routed_for_a_platform_it_does_not_play() {
        let t = tempfile::tempdir().unwrap();
        assert!(plan(&env("ppsspp", t.path()), &rom("g", "ps2"), &Identity::default()).is_none());
        // xemu only has saves for the original Xbox.
        assert!(plan(&env("xemu", t.path()), &rom("g", "ps2"), &Identity::default()).is_none());
    }

    #[test]
    fn xemu_reads_the_game_s_udata_folder_from_the_disk_image_xemu_is_set_up_with() {
        let t = tempfile::tempdir().unwrap();
        let id = Identity { save_id: "4d530004".into(), ..Default::default() };

        // No xemu.toml: the image xemu's own setup names, beside it.
        let p = plan(&env("xemu", t.path()), &rom("g", "xbox"), &id).unwrap();
        assert_eq!(p.label, "xemu");
        match &p.kind {
            Kind::Xbox { image, save_id } => {
                assert_eq!(image, &t.path().join("xbox_hdd.qcow2"));
                assert_eq!(save_id, "4D530004", "the title id is upper-case, as the zip's root folder is");
            }
            other => panic!("expected an Xbox save, got {other:?}"),
        }

        // A configured path wins, and a relative one is relative to xemu's folder.
        std::fs::write(t.path().join("xemu.toml"), "[sys.files]\nhdd_path = 'disks/mine.qcow2'\n").unwrap();
        let p = plan(&env("xemu", t.path()), &rom("g", "xbox"), &id).unwrap();
        assert!(matches!(&p.kind, Kind::Xbox { image, .. } if image == &t.path().join("disks/mine.qcow2")), "{p:?}");

        // A ROM Sigil could not read still gets a plan; reading the save then says why it can't.
        let p = plan(&env("xemu", t.path()), &rom("g", "xbox"), &Identity::default()).unwrap();
        assert!(matches!(&p.kind, Kind::Xbox { save_id, .. } if save_id.is_empty()));
    }

    #[test]
    fn content_name_is_the_file_the_emulator_loads() {
        let t = tempfile::tempdir().unwrap();
        let p = plan(&env("ppsspp", t.path()), &rom("Daxter (USA)", "psp"), &Identity::default()).unwrap();
        assert_eq!(p.content_name, "Daxter (USA).rom");
    }

    #[test]
    fn ini_sections_are_read_exactly() {
        let ini = "\u{feff}[Other]\nCard1Type = Wrong\n; comment\n[MemoryCards]\nCard1Type=Shared\nCard2Type = None\n\n[Next]\nCard1Type = No\n";
        let m = ini_section(ini, "MemoryCards");
        assert_eq!(m.get("Card1Type").map(String::as_str), Some("Shared"));
        assert_eq!(m.get("Card2Type").map(String::as_str), Some("None"));
        assert_eq!(m.len(), 2);
        assert!(ini_section(ini, "Absent").is_empty());
    }

    #[test]
    fn duckstation_card_settings_become_layout_options_and_unknown_values_are_dropped() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("settings.ini"), "[MemoryCards]\nCard1Type = Shared\nCard2Type = None\n").unwrap();
        let p = plan(&env("duckstation", t.path()), &rom("g", "psx"), &Identity::default()).unwrap();
        // `None` is not a card type the layout keys on, so only slot 1 is passed.
        assert_eq!(unit_route(&p).options, [("Card1Type".to_string(), "Shared".to_string())]);
    }

    #[test]
    fn pcsx2_passes_the_cards_the_user_named() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("inis")).unwrap();
        std::fs::write(t.path().join("inis/PCSX2.ini"), "[MemoryCards]\nSlot1_Filename = Mcd002.ps2\nSlot2_Filename = Extra\n").unwrap();
        let p = plan(&env("pcsx2", t.path()), &rom("g", "ps2"), &Identity::default()).unwrap();
        assert_eq!(
            unit_route(&p).options,
            [("Slot1_Filename".to_string(), "Mcd002.ps2".to_string()), ("Slot2_Filename".to_string(), "Extra".to_string())]
        );
    }

    #[test]
    fn dolphin_settings_are_found_in_the_data_folder_or_a_flatpak_config_folder() {
        let t = tempfile::tempdir().unwrap();
        // In the user folder.
        let a = t.path().join("a");
        std::fs::create_dir_all(a.join("Config")).unwrap();
        std::fs::write(a.join("Config/Dolphin.ini"), "[Core]\nSlotA = 1\nMemoryCardSize = 2\nSlotB = 255\n").unwrap();
        assert_eq!(dolphin_options(&a), [("SlotA".to_string(), "1".to_string()), ("MemoryCardSize".to_string(), "2".to_string())]);
        // Flatpak: data and config are siblings.
        let data = t.path().join("app/data/dolphin-emu");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(t.path().join("app/config/dolphin-emu")).unwrap();
        std::fs::write(t.path().join("app/config/dolphin-emu/Dolphin.ini"), "[Core]\nSlotA = 8\n").unwrap();
        assert_eq!(dolphin_options(&data), [("SlotA".to_string(), "8".to_string())]);
        // A card type Dolphin doesn't offer (memory card disabled) is not passed.
        std::fs::write(a.join("Config/Dolphin.ini"), "[Core]\nSlotA = 255\n").unwrap();
        assert!(dolphin_options(&a).is_empty());
    }

    #[test]
    fn retroarch_routes_the_cores_argosy_routes_as_units_and_the_rest_raw() {
        let t = tempfile::tempdir().unwrap();
        let mut e = env("retroarch", t.path());
        for (platform, core, unit) in [
            ("gb", "gambatte", true),
            ("gbc", "mgba", true),
            ("gb", "gearboy", false), // Argosy has no layout for it
            ("n64", "mupen64plus_next", true),
            ("n64", "parallel_n64", true),
            ("dc", "flycast", true),
            ("segacd", "genesis_plus_gx", true),
            ("genesis", "genesis_plus_gx", false), // only Mega-CD games keep a backup RAM
            ("segacd", "picodrive", false),
            ("snes", "snes9x", false),
            ("psx", "pcsx_rearmed", false), // built-in-only layout in Argosy, raw under RetroArch
        ] {
            e.core = Some(core.into());
            let p = plan(&e, &rom("g", platform), &Identity::default()).unwrap();
            assert_eq!(matches!(p.kind, Kind::Unit(_)), unit, "{platform}/{core}");
            assert_eq!(p.label, core, "RomM is told the core's name");
        }
    }

    #[test]
    fn retroarch_without_an_explicit_core_uses_the_platform_default() {
        let t = tempfile::tempdir().unwrap();
        let p = plan(&env("retroarch", t.path()), &rom("Zelda", "snes"), &Identity::default()).unwrap();
        assert_eq!(p.label, "snes9x");
        match p.kind {
            Kind::Raw { path, exists } => {
                assert_eq!(path, t.path().join("saves/Zelda.srm"));
                assert!(!exists);
            }
            k => panic!("{k:?}"),
        }
    }

    #[test]
    fn retroarch_raw_save_is_found_even_when_retroarch_sorted_it_into_a_folder() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("saves/Snes9x")).unwrap();
        std::fs::write(t.path().join("saves/Snes9x/Zelda.sav"), "x").unwrap();
        let p = plan(&env("retroarch", t.path()), &rom("Zelda", "snes"), &Identity::default()).unwrap();
        match p.kind {
            Kind::Raw { path, exists } => {
                assert_eq!(path, t.path().join("saves/Snes9x/Zelda.sav"));
                assert!(exists);
            }
            k => panic!("{k:?}"),
        }
    }

    #[test]
    fn a_new_raw_save_goes_where_retroarch_would_write_it() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("retroarch.cfg"), "sort_savefiles_enable = \"true\"\n").unwrap();
        let p = plan(&env("retroarch", t.path()), &rom("Zelda", "snes"), &Identity::default()).unwrap();
        match p.kind {
            Kind::Raw { path, exists } => {
                assert_eq!(path, t.path().join("saves/Snes9x/Zelda.srm"));
                assert!(!exists);
            }
            k => panic!("{k:?}"),
        }
    }
}
