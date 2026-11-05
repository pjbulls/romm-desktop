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
    /// xemu keeps both inside its virtual hard disk image.
    const SAVES_ONLY: &[&str] = &["xemu"];

    #[test]
    fn all_builtin_profiles_parse_and_are_complete() {
        let ps = builtin();
        let ids: Vec<_> = ps.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["retroarch", "dolphin", "ppsspp", "duckstation", "pcsx2", "azahar", "xemu"]);
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
