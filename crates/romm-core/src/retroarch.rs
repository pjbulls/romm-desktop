//! RetroArch specifics: reading retroarch.cfg, locating cores and per-game saves,
//! and the default platform → libretro core table.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use walkdir::WalkDir;

use crate::paths::{Os, PathCtx};

/// Default libretro core per RomM platform slug (core name without `_libretro`).
/// Overridable per platform in the app config.
pub const DEFAULT_CORES: &[(&str, &str)] = &[
    ("nes", "fceumm"),
    ("snes", "snes9x"),
    ("sfam", "snes9x"),
    ("n64", "mupen64plus_next"),
    ("gb", "gambatte"),
    ("gbc", "gambatte"),
    ("gba", "mgba"),
    ("nds", "melonds"),
    ("genesis", "genesis_plus_gx"),
    ("sms", "genesis_plus_gx"),
    ("gamegear", "genesis_plus_gx"),
    ("segacd", "genesis_plus_gx"),
    ("dc", "flycast"),
    ("atari2600", "stella"),
    ("atari7800", "prosystem"),
    ("lynx", "handy"),
    ("tg16", "mednafen_pce_fast"),
    ("virtualboy", "mednafen_vb"),
    ("wonderswan", "mednafen_wswan"),
    ("wonderswan-color", "mednafen_wswan"),
    ("neo-geo-pocket", "mednafen_ngp"),
    ("neo-geo-pocket-color", "mednafen_ngp"),
    ("arcade", "fbneo"),
    // PS1 fallback when DuckStation isn't used:
    ("psx", "pcsx_rearmed"),
];

pub fn default_core(slug: &str) -> Option<&'static str> {
    DEFAULT_CORES.iter().find(|(s, _)| *s == slug).map(|(_, c)| *c)
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
    /// RetroArch files saves under per-core / per-content subfolders when these are on.
    pub sort_saves_by_core: bool,
    pub sort_saves_by_content: bool,
    /// Same for save states; downloads can't be placed correctly when these are on.
    pub sort_states_by_core: bool,
    pub sort_states_by_content: bool,
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

    Layout {
        root: root.to_path_buf(),
        saves: resolve_dir(cfg.get("savefile_directory"), root, "saves", ctx),
        states: resolve_dir(cfg.get("savestate_directory"), root, "states", ctx),
        system: resolve_dir(cfg.get("system_directory"), root, "system", ctx),
        core_dirs,
        sort_saves_by_core: flag("sort_savefiles_enable"),
        sort_saves_by_content: flag("sort_savefiles_by_content_enable"),
        sort_states_by_core: flag("sort_savestates_enable"),
        sort_states_by_content: flag("sort_savestates_by_content_enable"),
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
    }
}
