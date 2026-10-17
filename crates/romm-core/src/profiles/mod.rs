//! Data-driven emulator profiles. Adding an emulator means adding a TOML file,
//! not writing Rust. The five built-ins are embedded in the binary.

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
    /// One file per game, named after the ROM (`<stem>.<ext>`).
    Stem,
    /// Shared folders; saves are attributed to a game by diffing them around a session.
    Diff,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SaveRule {
    pub mode: SaveMode,
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Folders (relative to the data root) to watch in `diff` mode.
    #[serde(default)]
    pub roots: Vec<String>,
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

    #[test]
    fn all_builtin_profiles_parse_and_are_complete() {
        let ps = builtin();
        let ids: Vec<_> = ps.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["retroarch", "dolphin", "ppsspp", "duckstation", "pcsx2"]);
        for p in &ps {
            assert!(p.launch.args.iter().any(|a| a == "{rom}"), "{} launch has no {{rom}}", p.id);
            assert!(p.saves.is_some(), "{} has no save rule", p.id);
            assert!(p.states.is_some(), "{} has no state rule", p.id);
            assert!(!p.data.linux.is_empty() && !p.data.windows.is_empty(), "{} lacks OS data dirs", p.id);
        }
        assert!(get("retroarch").unwrap().launch.args.contains(&"{core}".to_string()));
    }

    #[test]
    fn diff_profiles_declare_roots() {
        for id in ["dolphin", "ppsspp", "duckstation", "pcsx2"] {
            let r = get(id).unwrap().saves.unwrap();
            assert_eq!(r.mode, SaveMode::Diff);
            assert!(!r.roots.is_empty(), "{id}");
        }
    }
}

#[cfg(test)]
mod state_rule_tests {
    use super::*;

    #[test]
    fn diff_emulators_name_a_state_folder() {
        for (id, dir) in [("dolphin", "StateSaves"), ("ppsspp", "PPSSPP_STATE"), ("duckstation", "savestates"), ("pcsx2", "sstates")] {
            let r = get(id).unwrap().states.unwrap();
            assert_eq!(r.mode, SaveMode::Diff);
            assert_eq!(r.dir.as_deref(), Some(dir), "{id}");
        }
        assert_eq!(get("retroarch").unwrap().states.unwrap().mode, SaveMode::Stem);
    }
}
