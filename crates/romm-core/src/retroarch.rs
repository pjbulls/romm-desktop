//! RetroArch specifics: reading retroarch.cfg, locating cores and per-game saves,
//! and the default platform → libretro core table.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use walkdir::WalkDir;

use crate::paths::{Os, PathCtx};

/// Libretro cores that can run each RomM platform (core name without `_libretro`), best
/// choice first. The first entry is the default and what "Automatic" uses, so reordering
/// changes behaviour for everyone who hasn't picked a core.
///
/// This table is also what limits the choices offered for a system: a core is only
/// offered for the platforms it's listed under, so e.g. Game Boy never shows an
/// N64 or PlayStation core. A core can be listed under several platforms.
pub const CORES: &[(&str, &[&str])] = &[
    ("nes", &["fceumm", "nestopia", "mesen", "quicknes"]),
    ("snes", &["snes9x", "bsnes"]),
    ("sfam", &["snes9x", "bsnes"]),
    ("n64", &["mupen64plus_next", "parallel_n64"]),
    ("gb", &["gambatte", "mgba", "sameboy", "gearboy", "tgbdual", "vbam"]),
    ("gbc", &["gambatte", "mgba", "sameboy", "gearboy", "tgbdual", "vbam"]),
    ("gba", &["mgba", "vbam", "gpsp"]),
    ("nds", &["melonds", "melondsds", "desmume"]),
    ("genesis", &["genesis_plus_gx", "picodrive"]),
    ("sms", &["genesis_plus_gx", "picodrive", "gearsystem"]),
    ("gamegear", &["genesis_plus_gx", "gearsystem"]),
    ("segacd", &["genesis_plus_gx", "picodrive"]),
    ("dc", &["flycast"]),
    ("atari2600", &["stella"]),
    ("atari7800", &["prosystem"]),
    ("lynx", &["handy"]),
    ("tg16", &["mednafen_pce_fast", "mednafen_pce"]),
    ("virtualboy", &["mednafen_vb"]),
    ("wonderswan", &["mednafen_wswan"]),
    ("wonderswan-color", &["mednafen_wswan"]),
    ("neo-geo-pocket", &["mednafen_ngp"]),
    ("neo-geo-pocket-color", &["mednafen_ngp"]),
    ("arcade", &["fbneo", "mame2003_plus", "mame"]),
    // PS1 alternatives to DuckStation (which is the default when installed):
    ("psx", &["pcsx_rearmed", "swanstation", "mednafen_psx"]),
];

/// Human-readable core names for the UI; unknown cores fall back to their id.
const CORE_LABELS: &[(&str, &str)] = &[
    ("fceumm", "FCEUmm"),
    ("nestopia", "Nestopia"),
    ("mesen", "Mesen"),
    ("quicknes", "QuickNES"),
    ("snes9x", "Snes9x"),
    ("bsnes", "bsnes"),
    ("mupen64plus_next", "Mupen64Plus-Next"),
    ("parallel_n64", "ParaLLEl N64"),
    ("gambatte", "Gambatte"),
    ("mgba", "mGBA"),
    ("sameboy", "SameBoy"),
    ("gearboy", "Gearboy"),
    ("tgbdual", "TGB Dual"),
    ("vbam", "VBA-M"),
    ("gpsp", "gpSP"),
    ("melonds", "melonDS"),
    ("melondsds", "melonDS DS"),
    ("desmume", "DeSmuME"),
    ("genesis_plus_gx", "Genesis Plus GX"),
    ("picodrive", "PicoDrive"),
    ("gearsystem", "Gearsystem"),
    ("flycast", "Flycast"),
    ("stella", "Stella"),
    ("prosystem", "ProSystem"),
    ("handy", "Handy"),
    ("mednafen_pce_fast", "Beetle PCE Fast"),
    ("mednafen_pce", "Beetle PCE"),
    ("mednafen_vb", "Beetle VB"),
    ("mednafen_wswan", "Beetle WonderSwan"),
    ("mednafen_ngp", "Beetle NeoPop"),
    ("fbneo", "FinalBurn Neo"),
    ("mame2003_plus", "MAME 2003-Plus"),
    ("mame", "MAME"),
    ("pcsx_rearmed", "PCSX-ReARMed"),
    ("swanstation", "SwanStation"),
    ("mednafen_psx", "Beetle PSX"),
];

/// Cores that can run this platform, default first; empty if RetroArch can't play it.
pub fn cores_for(slug: &str) -> &'static [&'static str] {
    CORES.iter().find(|(s, _)| *s == slug).map(|(_, c)| *c).unwrap_or(&[])
}

/// A core's name as RomM and Argosy label it: no `_libretro` suffix or library extension.
pub fn core_slug(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let base = base.rsplit_once('.').filter(|(_, ext)| matches!(*ext, "so" | "dll" | "dylib")).map_or(base, |(b, _)| b);
    base.strip_suffix("_libretro").unwrap_or(base).to_string()
}

pub fn default_core(slug: &str) -> Option<&'static str> {
    cores_for(slug).first().copied()
}

/// Every platform RetroArch has at least one core for.
pub fn supported_slugs() -> impl Iterator<Item = &'static str> {
    CORES.iter().map(|(s, _)| *s)
}

pub fn core_label(core: &str) -> String {
    CORE_LABELS.iter().find(|(c, _)| *c == core).map(|(_, l)| l.to_string()).unwrap_or_else(|| core.to_string())
}

/// Parses `key = "value"` lines.
pub fn parse_cfg(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            let v = v.trim().trim_matches('"').to_string();
            Some((k.trim().to_string(), v))
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub root: PathBuf,
    pub saves: PathBuf,
    pub states: PathBuf,
    pub system: PathBuf,
    pub core_dirs: Vec<PathBuf>,
    /// Where core `.info` files live (they name the folder RetroArch sorts a core's saves into).
    pub info_dirs: Vec<PathBuf>,
    /// RetroArch files saves under per-core / per-content subfolders when these are on.
    pub sort_saves_by_core: bool,
    pub sort_saves_by_content: bool,
    /// Same for save states; downloads can't be placed correctly when these are on.
    pub sort_states_by_core: bool,
    pub sort_states_by_content: bool,
    /// Where per-core and per-game option files live (`rgui_config_directory`): `<name>/<name>.opt`.
    pub config_dir: PathBuf,
    /// The one options file shared by every core (`core_options_path`, else `retroarch-core-options.cfg`).
    pub global_options: PathBuf,
    /// "Use global core options": every core reads `global_options` and per-core files are ignored.
    pub global_core_options: bool,
}

fn resolve_dir(value: Option<&String>, root: &Path, default_sub: &str, ctx: &PathCtx) -> PathBuf {
    let v = value.map(|s| s.trim()).unwrap_or("");
    if v.is_empty() || v == "default" {
        return root.join(default_sub);
    }
    if let Some(rest) = v.strip_prefix(':') {
        // ":/saves" or ":\saves" = relative to RetroArch's own directory.
        return root.join(rest.trim_start_matches(['/', '\\']));
    }
    if let Some(rest) = v.strip_prefix('~') {
        if let Some(home) = &ctx.home {
            return home.join(rest.trim_start_matches(['/', '\\']));
        }
    }
    PathBuf::from(v)
}

pub fn layout(root: &Path, os: Os, ctx: &PathCtx) -> Layout {
    let cfg = std::fs::read_to_string(root.join("retroarch.cfg")).map(|t| parse_cfg(&t)).unwrap_or_default();
    let flag = |k: &str| cfg.get(k).is_some_and(|v| v == "true");

    let mut core_dirs = vec![resolve_dir(cfg.get("libretro_directory"), root, "cores", ctx)];
    if os == Os::Linux {
        // Distro packages (e.g. Debian's libretro-* packages) install here.
        core_dirs.push(PathBuf::from("/usr/lib/libretro"));
        core_dirs.push(PathBuf::from("/usr/lib/x86_64-linux-gnu/libretro"));
    }

    let mut info_dirs = vec![resolve_dir(cfg.get("libretro_info_path"), root, "info", ctx)];
    if os == Os::Linux {
        info_dirs.push(PathBuf::from("/usr/share/libretro/info"));
    }

    Layout {
        root: root.to_path_buf(),
        saves: resolve_dir(cfg.get("savefile_directory"), root, "saves", ctx),
        states: resolve_dir(cfg.get("savestate_directory"), root, "states", ctx),
        system: resolve_dir(cfg.get("system_directory"), root, "system", ctx),
        core_dirs,
        info_dirs,
        sort_saves_by_core: flag("sort_savefiles_enable"),
        sort_saves_by_content: flag("sort_savefiles_by_content_enable"),
        sort_states_by_core: flag("sort_savestates_enable"),
        sort_states_by_content: flag("sort_savestates_by_content_enable"),
        config_dir: resolve_dir(cfg.get("rgui_config_directory"), root, "config", ctx),
        global_options: match cfg.get("core_options_path").map(|v| v.trim()).filter(|v| !v.is_empty()) {
            Some(_) => resolve_dir(cfg.get("core_options_path"), root, "retroarch-core-options.cfg", ctx),
            None => root.join("retroarch-core-options.cfg"),
        },
        global_core_options: flag("global_core_options"),
    }
}

pub fn find_core(layout: &Layout, core: &str, os: Os) -> Option<PathBuf> {
    let file = match os {
        Os::Linux => format!("{core}_libretro.so"),
        Os::Windows => format!("{core}_libretro.dll"),
    };
    layout.core_dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file())
}

/// Newest `<stem>.<ext>` anywhere under the saves dir (handles per-core/per-content sorting).
pub fn find_save(layout: &Layout, stem: &str, ext: &str) -> Option<PathBuf> {
    let want = format!("{stem}.{ext}");
    WalkDir::new(&layout.saves)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.file_name().to_str() == Some(want.as_str()))
        .max_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH))
        .map(|e| e.into_path())
}

/// The raw save file for `stem`: `<stem>.srm`, else `<stem>.sav`, found as Argosy finds it
/// (`.srm` wins even when a newer `.sav` exists), anywhere RetroArch's sorting may have put it.
pub fn find_raw_save(layout: &Layout, stem: &str, extensions: &[String]) -> Option<PathBuf> {
    extensions.iter().find_map(|ext| find_save(layout, stem, ext))
}

/// The name RetroArch gives a core's folder when it sorts saves by core: the `corename` its `.info`
/// file declares ("Snes9x", "Genesis Plus GX"), else the name this tool shows for the core.
pub fn core_dir_name(layout: &Layout, core: &str) -> String {
    for dir in &layout.info_dirs {
        let Ok(text) = std::fs::read_to_string(dir.join(format!("{core}_libretro.info"))) else { continue };
        if let Some(name) = parse_cfg(&text).get("corename").filter(|n| !n.is_empty()) {
            return name.clone();
        }
    }
    core_label(core)
}

/// Directories RetroArch may keep this game's saves in, most likely first. RetroArch's own rule:
/// the saves directory, then a folder named after the core when "Sort Saves into Folders by Core"
/// is on, then one named after the content's folder when "...by Content Directory" is on.
pub fn save_dirs(layout: &Layout, core: &str, content: &Path) -> Vec<PathBuf> {
    let mut sorted = layout.saves.clone();
    if layout.sort_saves_by_core {
        sorted = sorted.join(core_dir_name(layout, core));
    }
    if layout.sort_saves_by_content {
        if let Some(dir) = content.parent().and_then(|p| p.file_name()) {
            sorted = sorted.join(dir);
        }
    }
    let mut dirs = vec![sorted];
    // Saves written before the user turned sorting on stay where they were.
    if !dirs.contains(&layout.saves) {
        dirs.push(layout.saves.clone());
    }
    dirs
}

/// The core options a layout's save file names can depend on, read the way RetroArch keeps them: a
/// per-core `.opt` first, then the global options file. Only keys that belong to the core are
/// returned (its own prefix, plus Flycast's historical `reicast_`).
pub fn core_options(layout: &Layout, core: &str) -> Vec<(String, String)> {
    let name = core_dir_name(layout, core);
    let mut files = vec![layout.config_dir.join(&name).join(format!("{name}.opt"))];
    files.push(layout.global_options.clone());
    let prefixes = [format!("{core}_"), if core == "flycast" { "reicast_".to_string() } else { String::new() }];
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        for (k, v) in parse_cfg(&text) {
            if prefixes.iter().any(|p| !p.is_empty() && k.starts_with(p.as_str())) {
                found.entry(k).or_insert(v); // the per-core file, read first, wins
            }
        }
    }
    found.into_iter().collect()
}

/// The file RetroArch will read a core's options from when no game- or folder-specific file
/// overrides it (checked against RetroArch's `runloop_init_core_options_path`): the per-core
/// `<config>/<name>/<name>.opt`, or the global file when "Use global core options" is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionsTarget {
    pub path: PathBuf,
    /// A per-core file that doesn't exist yet is created by RetroArch from the global file, not
    /// from nothing. Whoever creates it first has to do the same, or the user's other options for
    /// the core would quietly stop applying.
    pub seed: Option<PathBuf>,
}

pub fn options_target(layout: &Layout, core: &str) -> OptionsTarget {
    if layout.global_core_options {
        return OptionsTarget { path: layout.global_options.clone(), seed: None };
    }
    let name = core_dir_name(layout, core);
    let path = layout.config_dir.join(&name).join(format!("{name}.opt"));
    let seed = (!path.exists()).then(|| layout.global_options.clone());
    OptionsTarget { path, seed }
}

/// Option files that take priority over a core's own: RetroArch's per-game and per-folder ones are
/// the other `.opt` files in the core's config folder.
pub fn overriding_option_files(layout: &Layout, core: &str) -> usize {
    let name = core_dir_name(layout, core);
    let own = format!("{name}.opt");
    std::fs::read_dir(layout.config_dir.join(&name))
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.ends_with(".opt") && n != own.as_str()
                })
                .count()
        })
        .unwrap_or(0)
}

/// Does `file_name` look like one of RetroArch's state files for `stem`?
/// `<stem>.state`, `<stem>.state1`…`<stem>.stateNN`, `<stem>.state.auto` (not thumbnails).
pub fn is_state_name(file_name: &str, stem: &str) -> bool {
    let Some(rest) = file_name.strip_prefix(stem).and_then(|r| r.strip_prefix(".state")) else {
        return false;
    };
    rest.is_empty() || rest == ".auto" || rest.chars().all(|c| c.is_ascii_digit())
}

/// All state files for a game (newest wins if the same name exists in several sorted subfolders).
pub fn find_state_files(layout: &Layout, stem: &str) -> Vec<(String, PathBuf)> {
    let mut best: BTreeMap<String, (SystemTime, PathBuf)> = BTreeMap::new();
    for e in WalkDir::new(&layout.states).max_depth(3).into_iter().filter_map(|e| e.ok()) {
        if !e.file_type().is_file() {
            continue;
        }
        let Some(name) = e.file_name().to_str() else { continue };
        if !is_state_name(name, stem) {
            continue;
        }
        let m = e.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH);
        match best.get(name) {
            Some((t, _)) if *t >= m => {}
            _ => {
                best.insert(name.to_string(), (m, e.into_path()));
            }
        }
    }
    best.into_iter().map(|(n, (_, p))| (n, p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cfg_and_resolves_relative_dirs() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(
            t.path().join("retroarch.cfg"),
            "savefile_directory = \":\\mysaves\"\nsystem_directory = \"default\"\nsort_savefiles_enable = \"true\"\nlibretro_directory = \"/opt/cores\"\n",
        )
        .unwrap();
        let l = layout(t.path(), Os::Windows, &PathCtx::default());
        assert_eq!(l.saves, t.path().join("mysaves"));
        assert_eq!(l.system, t.path().join("system"));
        assert_eq!(l.states, t.path().join("states"));
        assert!(l.sort_saves_by_core && !l.sort_saves_by_content);
        assert_eq!(l.core_dirs[0], PathBuf::from("/opt/cores"));
    }

    #[test]
    fn missing_cfg_uses_defaults() {
        let t = tempfile::tempdir().unwrap();
        let l = layout(t.path(), Os::Linux, &PathCtx::default());
        assert_eq!(l.saves, t.path().join("saves"));
        assert!(l.core_dirs.contains(&PathBuf::from("/usr/lib/libretro")));
    }

    #[test]
    fn finds_core_and_newest_save_even_when_sorted_into_subfolders() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("cores")).unwrap();
        std::fs::write(t.path().join("cores/snes9x_libretro.dll"), "").unwrap();
        let l = layout(t.path(), Os::Windows, &PathCtx::default());
        assert!(find_core(&l, "snes9x", Os::Windows).is_some());
        assert!(find_core(&l, "mgba", Os::Windows).is_none());

        std::fs::create_dir_all(t.path().join("saves/Snes9x")).unwrap();
        std::fs::write(t.path().join("saves/Snes9x/Zelda.srm"), "x").unwrap();
        assert_eq!(find_save(&l, "Zelda", "srm").unwrap(), t.path().join("saves/Snes9x/Zelda.srm"));
        assert!(find_save(&l, "Metroid", "srm").is_none());
    }

    #[test]
    fn recognises_state_names_but_not_thumbnails_or_other_games() {
        for ok in ["Zelda.state", "Zelda.state1", "Zelda.state12", "Zelda.state.auto"] {
            assert!(is_state_name(ok, "Zelda"), "{ok}");
        }
        for no in ["Zelda.state1.png", "Zelda.srm", "Zelda 2.state", "Zeldax.state", "Zelda.states", "Zelda.state.bak"] {
            assert!(!is_state_name(no, "Zelda"), "{no}");
        }
    }

    #[test]
    fn finds_state_files_including_sorted_subfolders() {
        let t = tempfile::tempdir().unwrap();
        let l = layout(t.path(), Os::Linux, &PathCtx::default());
        std::fs::create_dir_all(t.path().join("states/Snes9x")).unwrap();
        for f in ["states/Zelda.state1", "states/Snes9x/Zelda.state2", "states/Zelda.state1.png", "states/Other.state1"] {
            std::fs::write(t.path().join(f), "x").unwrap();
        }
        let names: Vec<_> = find_state_files(&l, "Zelda").into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["Zelda.state1", "Zelda.state2"]);
    }

    #[test]
    fn core_table() {
        assert_eq!(default_core("snes"), Some("snes9x"));
        assert_eq!(default_core("ps2"), None);
        assert!(cores_for("ps2").is_empty());
    }

    #[test]
    fn defaults_are_unchanged_by_the_multi_core_table() {
        // These were the single hard-wired cores before cores became selectable;
        // "Automatic" must keep launching the same thing.
        for (slug, core) in [
            ("nes", "fceumm"), ("snes", "snes9x"), ("sfam", "snes9x"), ("n64", "mupen64plus_next"),
            ("gb", "gambatte"), ("gbc", "gambatte"), ("gba", "mgba"), ("nds", "melonds"),
            ("genesis", "genesis_plus_gx"), ("sms", "genesis_plus_gx"), ("gamegear", "genesis_plus_gx"),
            ("segacd", "genesis_plus_gx"), ("dc", "flycast"), ("atari2600", "stella"),
            ("atari7800", "prosystem"), ("lynx", "handy"), ("tg16", "mednafen_pce_fast"),
            ("virtualboy", "mednafen_vb"), ("wonderswan", "mednafen_wswan"),
            ("wonderswan-color", "mednafen_wswan"), ("neo-geo-pocket", "mednafen_ngp"),
            ("neo-geo-pocket-color", "mednafen_ngp"), ("arcade", "fbneo"), ("psx", "pcsx_rearmed"),
        ] {
            assert_eq!(default_core(slug), Some(core), "{slug}");
        }
        assert_eq!(supported_slugs().count(), 24);
    }

    #[test]
    fn core_table_is_well_formed() {
        let mut seen = std::collections::BTreeSet::new();
        for (slug, cores) in CORES {
            assert!(seen.insert(*slug), "duplicate platform {slug}");
            assert!(!cores.is_empty(), "{slug} has no cores");
            let unique: std::collections::BTreeSet<_> = cores.iter().collect();
            assert_eq!(unique.len(), cores.len(), "{slug} lists a core twice");
            for c in *cores {
                assert!(!c.contains("_libretro") && !c.contains('/') && !c.contains(' '), "{c} should be a bare core name");
            }
        }
        // Every core we offer has an explicit display name entry. (Checked by presence, not by
        // "differs from the id": bsnes' official name is literally "bsnes".)
        for (_, cores) in CORES {
            for c in *cores {
                assert!(CORE_LABELS.iter().any(|(id, _)| id == c), "{c} has no entry in CORE_LABELS");
            }
        }
        // ...and no label is left over for a core that's no longer offered.
        let offered: std::collections::BTreeSet<&str> = CORES.iter().flat_map(|(_, c)| c.iter().copied()).collect();
        for (id, _) in CORE_LABELS {
            assert!(offered.contains(id), "CORE_LABELS has an unused entry for {id}");
        }
        assert_eq!(core_label("some_new_core"), "some_new_core");
    }

    #[test]
    fn game_boy_offers_game_boy_cores_only() {
        assert_eq!(cores_for("gb"), ["gambatte", "mgba", "sameboy", "gearboy", "tgbdual", "vbam"]);
        for other in ["mupen64plus_next", "pcsx_rearmed", "flycast", "melonds", "snes9x", "mednafen_vb"] {
            assert!(!cores_for("gb").contains(&other), "{other} can't run Game Boy games");
        }
    }

    #[test]
    fn core_names_are_labelled_the_way_argosy_labels_them() {
        assert_eq!(core_slug("snes9x"), "snes9x");
        assert_eq!(core_slug("snes9x_libretro"), "snes9x");
        assert_eq!(core_slug("snes9x_libretro.so"), "snes9x");
        assert_eq!(core_slug("/cores/mgba_libretro.dll"), "mgba");
        assert_eq!(core_slug("C:\\RA\\cores\\mgba_libretro.dll"), "mgba");
    }

    #[test]
    fn raw_save_prefers_srm_over_a_newer_sav() {
        let t = tempfile::tempdir().unwrap();
        let l = layout(t.path(), Os::Linux, &PathCtx::default());
        std::fs::create_dir_all(t.path().join("saves")).unwrap();
        std::fs::write(t.path().join("saves/Zelda.srm"), "old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(t.path().join("saves/Zelda.sav"), "new").unwrap();
        let exts = vec!["srm".to_string(), "sav".to_string()];
        assert_eq!(find_raw_save(&l, "Zelda", &exts).unwrap(), t.path().join("saves/Zelda.srm"));
        std::fs::remove_file(t.path().join("saves/Zelda.srm")).unwrap();
        assert_eq!(find_raw_save(&l, "Zelda", &exts).unwrap(), t.path().join("saves/Zelda.sav"));
    }

    #[test]
    fn saves_are_sorted_the_way_retroarch_sorts_them() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("info")).unwrap();
        std::fs::write(t.path().join("info/snes9x_libretro.info"), "display_name = \"Nintendo - SNES (Snes9x)\"\ncorename = \"Snes9x\"\n").unwrap();
        std::fs::write(
            t.path().join("retroarch.cfg"),
            "sort_savefiles_enable = \"true\"\nsort_savefiles_by_content_enable = \"true\"\n",
        )
        .unwrap();
        let l = layout(t.path(), Os::Linux, &PathCtx::default());
        let dirs = save_dirs(&l, "snes9x", Path::new("/roms/snes/Zelda.sfc"));
        assert_eq!(dirs[0], t.path().join("saves/Snes9x/snes"));
        assert_eq!(dirs[1], t.path().join("saves"), "older, unsorted saves are still found");

        // Without sorting there is one place.
        let t2 = tempfile::tempdir().unwrap();
        let l2 = layout(t2.path(), Os::Linux, &PathCtx::default());
        assert_eq!(save_dirs(&l2, "snes9x", Path::new("/roms/Zelda.sfc")), [t2.path().join("saves")]);
        // A core with no .info falls back to the name shown in the UI.
        assert_eq!(core_dir_name(&l2, "mgba"), "mGBA");
    }

    #[test]
    fn core_options_come_from_the_core_file_then_the_global_one_and_only_for_that_core() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("config/Genesis Plus GX")).unwrap();
        std::fs::write(
            t.path().join("config/Genesis Plus GX/Genesis Plus GX.opt"),
            "genesis_plus_gx_system_bram = \"per game\"\nsnes9x_overclock = \"x\"\n",
        )
        .unwrap();
        std::fs::write(
            t.path().join("retroarch-core-options.cfg"),
            "genesis_plus_gx_system_bram = \"per bios\"\ngenesis_plus_gx_cart_size = \"2meg\"\nmgba_x = \"1\"\n",
        )
        .unwrap();
        let l = layout(t.path(), Os::Linux, &PathCtx::default());
        assert_eq!(
            core_options(&l, "genesis_plus_gx"),
            [("genesis_plus_gx_cart_size".to_string(), "2meg".to_string()), ("genesis_plus_gx_system_bram".to_string(), "per game".to_string())]
        );
        assert!(core_options(&l, "flycast").is_empty());
    }
}
