//! Placeholder expansion and data-directory resolution for emulator profiles.
//! The OS and every directory source are explicit parameters so Windows
//! behaviour can be unit-tested on Linux and vice versa.

use std::path::{Path, PathBuf};

use crate::profiles::{Candidate, Profile};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Linux,
    Windows,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct PathCtx {
    pub home: Option<PathBuf>,
    pub xdg_data: Option<PathBuf>,
    pub xdg_config: Option<PathBuf>,
    pub appdata: Option<PathBuf>,
    pub localappdata: Option<PathBuf>,
    pub programfiles: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub exe_dir: Option<PathBuf>,
}

impl PathCtx {
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
        Self {
            xdg_data: env("XDG_DATA_HOME").or_else(|| home.as_ref().map(|h| h.join(".local/share"))),
            xdg_config: env("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|h| h.join(".config"))),
            appdata: env("APPDATA"),
            localappdata: env("LOCALAPPDATA"),
            programfiles: env("ProgramFiles"),
            documents: directories::UserDirs::new().and_then(|u| u.document_dir().map(Path::to_path_buf)),
            home,
            exe_dir: None,
        }
    }

    pub fn with_exe_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.exe_dir = dir;
        self
    }

    /// Expands `{home}`, `{xdg_data}`, `{xdg_config}`, `{appdata}`, `{localappdata}`,
    /// `{programfiles}`, `{documents}`, `{exe_dir}`. `None` if any placeholder is
    /// unknown or has no value on this machine.
    pub fn expand(&self, template: &str) -> Option<PathBuf> {
        let mut out = String::new();
        let mut rest = template;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let end = rest[start..].find('}')? + start;
            let name = &rest[start + 1..end];
            let val = match name {
                "home" => &self.home,
                "xdg_data" => &self.xdg_data,
                "xdg_config" => &self.xdg_config,
                "appdata" => &self.appdata,
                "localappdata" => &self.localappdata,
                "programfiles" => &self.programfiles,
                "documents" => &self.documents,
                "exe_dir" => &self.exe_dir,
                _ => return None,
            };
            out.push_str(val.as_ref()?.to_str()?);
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        Some(PathBuf::from(out))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRoot {
    pub path: PathBuf,
    /// False when no candidate exists yet (emulator never run); `path` is then
    /// the first usable candidate and can be created on demand.
    pub exists: bool,
}

/// Picks the emulator's data directory: the first candidate that exists (and whose
/// `requires` marker exists), else the first candidate that can be expanded.
pub fn resolve_data_root(profile: &Profile, flatpak: bool, os: Os, ctx: &PathCtx) -> Option<DataRoot> {
    pick(candidates(&profile.data, flatpak, os), ctx)
}

/// Where the emulator keeps its settings files: the `[config]` folder when the profile has one
/// (Azahar splits config from data on Linux), else the data folder it was given.
pub fn resolve_config_root(profile: &Profile, flatpak: bool, os: Os, ctx: &PathCtx, data_root: &Path) -> PathBuf {
    let cands = candidates(&profile.config, flatpak, os);
    if cands.is_empty() {
        return data_root.to_path_buf();
    }
    pick(cands, ctx).map(|r| r.path).unwrap_or_else(|| data_root.to_path_buf())
}

fn candidates(dirs: &crate::profiles::DataDirs, flatpak: bool, os: Os) -> &[Candidate] {
    if flatpak {
        &dirs.flatpak
    } else {
        match os {
            Os::Linux => &dirs.linux,
            Os::Windows => &dirs.windows,
        }
    }
}

fn pick(cands: &[Candidate], ctx: &PathCtx) -> Option<DataRoot> {
    let mut first_plain: Option<PathBuf> = None;
    let mut flagged: Option<PathBuf> = None;
    for c in cands {
        let Some(path) = ctx.expand(c.path()) else { continue };
        if let Some(req) = c.requires() {
            // A satisfied marker (portable.txt etc.) settles it, even if the folder isn't created yet.
            match ctx.expand(req) {
                Some(r) if r.exists() => {
                    let exists = path.is_dir();
                    return Some(DataRoot { path, exists });
                }
                _ => continue,
            }
        }
        if path.is_dir() {
            return Some(DataRoot { path, exists: true });
        }
        if c.is_fallback() && flagged.is_none() {
            flagged = Some(path.clone());
        }
        if first_plain.is_none() {
            first_plain = Some(path);
        }
    }
    flagged.or(first_plain).map(|path| DataRoot { path, exists: false })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles;

    fn ctx(root: &Path) -> PathCtx {
        PathCtx {
            home: Some(root.join("home")),
            xdg_data: Some(root.join("home/.local/share")),
            xdg_config: Some(root.join("home/.config")),
            documents: Some(root.join("docs")),
            ..Default::default()
        }
    }

    #[test]
    fn expands_and_rejects_missing() {
        let c = PathCtx { home: Some("/h".into()), ..Default::default() };
        assert_eq!(c.expand("{home}/x").unwrap(), PathBuf::from("/h/x"));
        assert!(c.expand("{appdata}/x").is_none());
        assert!(c.expand("{bogus}").is_none());
    }

    #[test]
    fn dolphin_prefers_existing_legacy_dir() {
        let t = tempfile::tempdir().unwrap();
        let c = ctx(t.path());
        let p = profiles::get("dolphin").unwrap();
        // Fresh machine: new installs use the XDG location, not the legacy dir.
        let r = resolve_data_root(&p, false, Os::Linux, &c).unwrap();
        assert!(!r.exists);
        assert_eq!(r.path, t.path().join("home/.local/share/dolphin-emu"));
        // Modern dir exists only: it is chosen.
        std::fs::create_dir_all(t.path().join("home/.local/share/dolphin-emu")).unwrap();
        let r = resolve_data_root(&p, false, Os::Linux, &c).unwrap();
        assert!(r.exists && r.path.ends_with("dolphin-emu"), "{r:?}");
        // Legacy dir wins when present.
        std::fs::create_dir_all(t.path().join("home/.dolphin-emu")).unwrap();
        assert!(resolve_data_root(&p, false, Os::Linux, &c).unwrap().path.ends_with(".dolphin-emu"));
    }

    #[test]
    fn flatpak_paths_are_used_for_flatpak_installs() {
        let t = tempfile::tempdir().unwrap();
        let p = profiles::get("ppsspp").unwrap();
        let r = resolve_data_root(&p, true, Os::Linux, &ctx(t.path())).unwrap();
        assert!(r.path.ends_with(".var/app/org.ppsspp.PPSSPP/config/ppsspp/PSP"), "{r:?}");
    }

    #[test]
    fn azahar_data_roots_per_install_kind() {
        let t = tempfile::tempdir().unwrap();
        let p = profiles::get("azahar").unwrap();
        let mut c = ctx(t.path());
        c.appdata = Some(t.path().join("AppData"));

        let r = resolve_data_root(&p, false, Os::Linux, &c).unwrap();
        assert_eq!((r.path, r.exists), (t.path().join("home/.local/share/azahar-emu"), false));
        let r = resolve_data_root(&p, true, Os::Linux, &c).unwrap();
        assert!(r.path.ends_with(".var/app/org.azahar_emu.Azahar/data/azahar-emu"), "{r:?}");

        // Windows: %APPDATA%/Azahar, unless a `user` folder sits next to the executable.
        let exe_dir = t.path().join("portable-az");
        std::fs::create_dir_all(&exe_dir).unwrap();
        c.exe_dir = Some(exe_dir.clone());
        let r = resolve_data_root(&p, false, Os::Windows, &c).unwrap();
        assert_eq!(r.path, t.path().join("AppData/Azahar"));
        std::fs::create_dir_all(exe_dir.join("user")).unwrap();
        let r = resolve_data_root(&p, false, Os::Windows, &c).unwrap();
        assert_eq!((r.path, r.exists), (exe_dir.join("user"), true));
    }

    #[test]
    fn xemu_data_roots_per_install_kind() {
        let t = tempfile::tempdir().unwrap();
        let p = profiles::get("xemu").unwrap();
        let mut c = ctx(t.path());
        c.appdata = Some(t.path().join("AppData"));

        let r = resolve_data_root(&p, false, Os::Linux, &c).unwrap();
        assert_eq!(r.path, t.path().join("home/.local/share/xemu/xemu"));
        let r = resolve_data_root(&p, true, Os::Linux, &c).unwrap();
        assert!(r.path.ends_with(".var/app/app.xemu.xemu/data/xemu/xemu"), "{r:?}");

        // Windows: %APPDATA%/xemu/xemu, unless xemu.toml sits next to the executable (portable).
        let exe_dir = t.path().join("portable-xemu");
        std::fs::create_dir_all(&exe_dir).unwrap();
        c.exe_dir = Some(exe_dir.clone());
        let r = resolve_data_root(&p, false, Os::Windows, &c).unwrap();
        assert_eq!(r.path, t.path().join("AppData/xemu/xemu"));
        std::fs::write(exe_dir.join("xemu.toml"), "").unwrap();
        let r = resolve_data_root(&p, false, Os::Windows, &c).unwrap();
        assert_eq!((r.path, r.exists), (exe_dir, true));
    }

    #[test]
    fn windows_portable_requires_marker() {
        let t = tempfile::tempdir().unwrap();
        let exe_dir = t.path().join("Dolphin");
        std::fs::create_dir_all(exe_dir.join("User")).unwrap();
        let p = profiles::get("dolphin").unwrap();
        let mut c = ctx(t.path());
        c.exe_dir = Some(exe_dir.clone());
        c.appdata = Some(t.path().join("AppData"));
        // User/ exists but portable.txt doesn't: not portable, falls through.
        let r = resolve_data_root(&p, false, Os::Windows, &c).unwrap();
        assert!(r.path.ends_with("Dolphin Emulator"), "{r:?}");
        std::fs::write(exe_dir.join("portable.txt"), "").unwrap();
        assert_eq!(resolve_data_root(&p, false, Os::Windows, &c).unwrap().path, exe_dir.join("User"));
        // Portable marker present but User/ not created yet: still the portable location.
        std::fs::remove_dir_all(exe_dir.join("User")).unwrap();
        let r = resolve_data_root(&p, false, Os::Windows, &c).unwrap();
        assert_eq!((r.path, r.exists), (exe_dir.join("User"), false));
    }

    /// Canary's storage root (xenia_main.cc): the exe folder when portable, else `<user folder>/Xenia`.
    #[test]
    fn xenia_data_roots_follow_canarys_storage_root_rules() {
        let t = tempfile::tempdir().unwrap();
        let p = profiles::get("xenia").unwrap();
        let mut c = ctx(t.path());
        c.appdata = Some(t.path().join("AppData"));
        let exe_dir = t.path().join("xenia-win");
        std::fs::create_dir_all(&exe_dir).unwrap();
        c.exe_dir = Some(exe_dir.clone());

        // Linux, not portable: $XDG_DATA_HOME/Xenia.
        let r = resolve_data_root(&p, false, Os::Linux, &c).unwrap();
        assert_eq!((r.path, r.exists), (t.path().join("home/.local/share/Xenia"), false));
        // Linux with portable.txt beside the binary (the tarball build, never an AppImage).
        std::fs::write(exe_dir.join("portable.txt"), "").unwrap();
        let r = resolve_data_root(&p, false, Os::Linux, &c).unwrap();
        assert_eq!((r.path, r.exists), (exe_dir.clone(), true));
        std::fs::remove_file(exe_dir.join("portable.txt")).unwrap();

        // Windows, never run: Canary defaults to portable, so its own folder.
        let r = resolve_data_root(&p, false, Os::Windows, &c).unwrap();
        assert_eq!((r.path, r.exists), (exe_dir.clone(), true));
        // Windows, run with --portable=false: Documents\Xenia.
        std::fs::create_dir_all(t.path().join("docs/Xenia")).unwrap();
        let r = resolve_data_root(&p, false, Os::Windows, &c).unwrap();
        assert_eq!(r.path, t.path().join("docs/Xenia"));
        // Windows, a portable run left its config next to the exe: that wins over Documents.
        std::fs::write(exe_dir.join("xenia-canary.config.toml"), "").unwrap();
        let r = resolve_data_root(&p, false, Os::Windows, &c).unwrap();
        assert_eq!(r.path, exe_dir);
    }

    #[test]
    fn xenia_has_no_flatpak_data_root() {
        let t = tempfile::tempdir().unwrap();
        let p = profiles::get("xenia").unwrap();
        assert!(resolve_data_root(&p, true, Os::Linux, &ctx(t.path())).is_none());
    }
}
