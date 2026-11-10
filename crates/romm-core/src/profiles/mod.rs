//! Data-driven emulator profiles. Adding an emulator means adding a TOML file,
//! not writing Rust. The built-ins are embedded in the binary.

use serde::Deserialize;

use crate::error::Result;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Candidate {
    Plain(String),
    Full {
        path: String,
        /// Only counts if this path exists (e.g. a portable-mode marker file).
        requires: Option<String>,
        /// Where to create the data dir if none of the candidates exist yet.
        fallback: Option<bool>,
    },
}

impl Candidate {
    pub fn path(&self) -> &str {
        match self {
            Candidate::Plain(p) => p,
            Candidate::Full { path, .. } => path,
        }
    }
    pub fn is_fallback(&self) -> bool {
        matches!(self, Candidate::Full { fallback: Some(true), .. })
    }
    pub fn requires(&self) -> Option<&str> {
        match self {
            Candidate::Plain(_) => None,
            Candidate::Full { requires, .. } => requires.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Detect {
    #[serde(default)]
    pub linux_bins: Vec<String>,
    #[serde(default)]
    pub windows_exes: Vec<String>,
    #[serde(default)]
    pub flatpak_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Launch {
    /// Placeholders: `{rom}` and (RetroArch only) `{core}`.
    pub args: Vec<String>,
    /// The emulator cannot open a `.m3u` playlist (Xenia and PCSX2 read it as a disc image and fail).
    /// A multi-disc game is then launched on the first disc the playlist lists instead. RetroArch and
    /// DuckStation take playlists, so the default is that they are passed through.
    #[serde(default)]
    pub no_playlists: bool,
    /// The emulator cannot open a `.cue` sheet (PCSX2 reads it as a disc image and fails). The sheet is
    /// then replaced by the data track file it names. Checked after `no_playlists`, so a playlist
    /// that lists sheets ends up at a track too.
    #[serde(default)]
    pub no_cue_sheets: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DataDirs {
    #[serde(default)]
    pub linux: Vec<Candidate>,
    #[serde(default)]
    pub flatpak: Vec<Candidate>,
    #[serde(default)]
    pub windows: Vec<Candidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SaveMode {
    /// RetroArch: files named after the ROM (`<stem>.<ext>`) in its saves directory. Which of them
    /// are the game's save, and whether they travel raw or as a unit, depends on the core.
    Stem,
    /// A standalone emulator with its own save layout (shared cards, per-title folders). Saves are
    /// found by game id (`unit`, via Sigil); save *states* are attributed by diffing a folder.
    Diff,
    /// Saves live inside a virtual hard-disk image (xemu): read out of it by title id, one zip per
    /// game (`xbox`). Only meaningful as a *save* rule; there is no such thing for states.
    Hdd,
    /// A game's saves are a folder tree the emulator keeps under its own data folder (Xenia's
    /// `content/<profile>/<title id>/00000001`), zipped as one unit per game (`xenia`). Only
    /// meaningful as a *save* rule.
    Folder,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SaveRule {
    pub mode: SaveMode,
    /// Extensions of the raw save file, most preferred first (`stem` mode).
    #[serde(default)]
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StateRule {
    pub mode: SaveMode,
    /// Folder (relative to the data root) holding state files; flat, one file per slot.
    /// Not needed in `stem` mode (RetroArch's own savestate directory is used).
    #[serde(default)]
    pub dir: Option<String>,
    /// File extensions to ignore (screenshots stored next to states).
    #[serde(default)]
    pub ignore_extensions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BiosRule {
    /// Relative to the data root.
    pub dir: String,
}

/// Which of an emulator's folders a settings file is relative to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Root {
    /// The data root (`[data]`): where saves, states and, for most emulators, the settings live.
    #[default]
    Data,
    /// The config root (`[config]`), for emulators that keep settings somewhere other than their data.
    Config,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
}

/// How a value is written into a TOML file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TomlKind {
    #[default]
    String,
    Int,
    Bool,
}

/// One thing a setting's choice does to an emulator. `{value}` in any string is replaced by the
/// choice's value. See `emu_settings` for how these are carried out.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Apply {
    /// Command-line arguments. They go ahead of the user's own extra args, so those still win.
    Args { args: Vec<String> },
    /// Keys in a section of an INI file. The file must already exist: an emulator that has never
    /// run has no settings file, and one we made would stop it running its first-time setup.
    Ini {
        #[serde(default)]
        root: Root,
        file: String,
        section: String,
        sets: Vec<KeyValue>,
    },
    /// One key in a TOML file (a dotted `path`, e.g. `display.quality.surface_scale`).
    Toml {
        #[serde(default)]
        root: Root,
        file: String,
        path: String,
        value: String,
        #[serde(default)]
        kind: TomlKind,
    },
    /// Options of the libretro core a RetroArch setting belongs to, in RetroArch's per-core options file.
    RetroarchOption { sets: Vec<KeyValue> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SettingChoice {
    pub value: String,
    pub label: String,
    /// What this choice does, when that differs from the setting's own `apply` (e.g. a flag for
    /// "on" and another for "off", or two options set together).
    #[serde(default)]
    pub apply: Vec<Apply>,
}

/// A setting the app can take over for an emulator. Until the user picks a choice the emulator's
/// own setting is left alone.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Setting {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub help: String,
    /// RetroArch only: the libretro core this setting belongs to (each core names its options differently).
    #[serde(default)]
    pub core: Option<String>,
    pub choices: Vec<SettingChoice>,
    /// What every choice does, with `{value}` filled in. A choice's own `apply` replaces it.
    #[serde(default)]
    pub apply: Vec<Apply>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// RomM platform slugs this emulator handles by default.
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub detect: Detect,
    pub launch: Launch,
    #[serde(default)]
    pub data: DataDirs,
    #[serde(default)]
    pub saves: Option<SaveRule>,
    #[serde(default)]
    pub states: Option<StateRule>,
    #[serde(default)]
    pub bios: Option<BiosRule>,
    /// Where the emulator keeps its settings files when that is not its data folder (empty: the
    /// data folder). Same candidate rules as `[data]`.
    #[serde(default)]
    pub config: DataDirs,
    /// Settings the app can control (see `emu_settings`).
    #[serde(default)]
    pub settings: Vec<Setting>,
}

impl Profile {
    pub fn from_toml(s: &str) -> Result<Profile> {
        Ok(toml::from_str(s)?)
    }
}

const BUILTIN: &[&str] = &[
    include_str!("retroarch.toml"),
    include_str!("dolphin.toml"),
    include_str!("ppsspp.toml"),
    include_str!("duckstation.toml"),
    include_str!("pcsx2.toml"),
    include_str!("azahar.toml"),
    include_str!("xemu.toml"),
    include_str!("xenia.toml"),
];

pub fn builtin() -> Vec<Profile> {
    BUILTIN
        .iter()
        .map(|s| Profile::from_toml(s).expect("built-in profile must parse"))
        .collect()
}

pub fn get(id: &str) -> Option<Profile> {
    builtin().into_iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Emulators that can be launched but not synced. None at the moment.
    const LAUNCH_ONLY: &[&str] = &[];

    /// Emulators whose saves can be read (and uploaded) but that have no save-state or BIOS rule:
    /// xemu keeps both inside its virtual hard disk image, and Xenia has neither feature.
    const SAVES_ONLY: &[&str] = &["xemu", "xenia"];

    /// Which emulators are known not to open a `.m3u` (checked against each one's source). Adding one
    /// is a deliberate act: it changes what that emulator is launched with, so it is listed here
    /// rather than inferred. RetroArch and DuckStation do take playlists.
    #[test]
    fn the_emulators_marked_as_unable_to_open_playlists() {
        let marked: Vec<String> = builtin().into_iter().filter(|p| p.launch.no_playlists).map(|p| p.id).collect();
        assert_eq!(marked, ["pcsx2", "xenia"]);
    }

    /// Likewise for `.cue` sheets. RetroArch and DuckStation read them; PCSX2's reader has a TODO for
    /// cue support, and Xenia has no use for them.
    #[test]
    fn the_emulators_marked_as_unable_to_open_cue_sheets() {
        let marked: Vec<String> = builtin().into_iter().filter(|p| p.launch.no_cue_sheets).map(|p| p.id).collect();
        assert_eq!(marked, ["pcsx2"]);
    }

    #[test]
    fn all_builtin_profiles_parse_and_are_complete() {
        let ps = builtin();
        let ids: Vec<_> = ps.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["retroarch", "dolphin", "ppsspp", "duckstation", "pcsx2", "azahar", "xemu", "xenia"]);
        for p in &ps {
            assert!(p.launch.args.iter().any(|a| a == "{rom}"), "{} launch has no {{rom}}", p.id);
            if LAUNCH_ONLY.contains(&p.id.as_str()) {
                assert!(
                    p.saves.is_none() && p.states.is_none() && p.bios.is_none(),
                    "{} is listed as launch-only but declares save/state/bios rules; remove it from LAUNCH_ONLY",
                    p.id
                );
            } else if SAVES_ONLY.contains(&p.id.as_str()) {
                assert!(p.saves.is_some(), "{} has no save rule", p.id);
                assert!(p.states.is_none() && p.bios.is_none(), "{} is listed as saves-only but has state/bios rules", p.id);
            } else {
                assert!(p.saves.is_some(), "{} has no save rule", p.id);
                assert!(p.states.is_some(), "{} has no state rule", p.id);
            }
            assert!(!p.data.linux.is_empty() && !p.data.windows.is_empty(), "{} lacks OS data dirs", p.id);
        }
        assert!(get("retroarch").unwrap().launch.args.contains(&"{core}".to_string()));
    }

    #[test]
    fn standalone_emulators_use_their_own_layout_and_retroarch_uses_stems() {
        for id in ["dolphin", "ppsspp", "duckstation", "pcsx2", "azahar"] {
            assert_eq!(get(id).unwrap().saves.unwrap().mode, SaveMode::Diff, "{id}");
        }
        let ra = get("retroarch").unwrap().saves.unwrap();
        assert_eq!(ra.mode, SaveMode::Stem);
        // Argosy looks for `.srm` first, then `.sav`.
        assert_eq!(ra.extensions, ["srm", "sav"]);
    }
}

#[cfg(test)]
mod state_rule_tests {
    use super::*;

    #[test]
    fn diff_emulators_name_a_state_folder() {
        for (id, dir) in [
            ("dolphin", "StateSaves"),
            ("ppsspp", "PPSSPP_STATE"),
            ("duckstation", "savestates"),
            ("pcsx2", "sstates"),
            ("azahar", "states"),
        ] {
            let r = get(id).unwrap().states.unwrap();
            assert_eq!(r.mode, SaveMode::Diff);
            assert_eq!(r.dir.as_deref(), Some(dir), "{id}");
        }
        assert_eq!(get("retroarch").unwrap().states.unwrap().mode, SaveMode::Stem);
    }
}

#[cfg(test)]
mod azahar_xemu_tests {
    use super::*;

    #[test]
    fn azahar_plays_3ds_and_new_3ds() {
        let p = get("azahar").unwrap();
        assert_eq!(p.platforms, ["3ds", "new-nintendo-3ds"]);
        assert_eq!(p.saves.unwrap().mode, SaveMode::Diff);
    }

    #[test]
    fn xemu_is_original_xbox_only_with_saves_read_from_the_disk_image() {
        let p = get("xemu").unwrap();
        assert_eq!(p.platforms, ["xbox"]);
        assert_eq!(p.launch.args, ["-dvd_path", "{rom}"]);
        assert_eq!(p.saves.unwrap().mode, SaveMode::Hdd);
        assert!(p.states.is_none() && p.bios.is_none());
    }
}

#[cfg(test)]
mod xenia_tests {
    use super::*;

    #[test]
    fn xenia_plays_xbox_360_only_and_leaves_the_original_xbox_to_xemu() {
        let p = get("xenia").unwrap();
        assert_eq!(p.platforms, ["xbox360"]);
        assert!(!get("xemu").unwrap().platforms.contains(&"xbox360".to_string()));
        assert!(!p.platforms.contains(&"xbox".to_string()));
    }

    /// `--` ends option parsing, so a ROM named `-weird.iso` cannot be read as a flag, and the
    /// extra args (which launch.rs puts first) still land before it.
    #[test]
    fn the_game_is_a_positional_argument_after_double_dash() {
        assert_eq!(get("xenia").unwrap().launch.args, ["--", "{rom}"]);
    }

    #[test]
    fn xenia_syncs_saves_by_folder_and_has_no_states_or_bios() {
        let p = get("xenia").unwrap();
        assert_eq!(p.saves.unwrap().mode, SaveMode::Folder);
        assert!(p.states.is_none(), "Xenia has no save states");
        assert!(p.bios.is_none(), "Xenia needs no BIOS");
    }

    #[test]
    fn there_is_no_flatpak_to_detect() {
        let p = get("xenia").unwrap();
        assert!(p.detect.flatpak_id.is_none());
        assert!(p.data.flatpak.is_empty());
    }
}
