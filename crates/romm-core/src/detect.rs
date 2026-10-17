//! Finds installed emulators: user override, then PATH, then known Windows install
//! locations, then Flatpak.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::paths::{Os, PathCtx};
use crate::profiles::Profile;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    Native(PathBuf),
    Flatpak(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub profile_id: String,
    pub kind: InstallKind,
}

impl Installed {
    pub fn is_flatpak(&self) -> bool {
        matches!(self.kind, InstallKind::Flatpak(_))
    }
    /// Directory containing the executable (for portable-mode data dirs).
    pub fn exe_dir(&self) -> Option<PathBuf> {
        match &self.kind {
            InstallKind::Native(p) => p.parent().map(Path::to_path_buf),
            InstallKind::Flatpak(_) => None,
        }
    }
}

fn which(name: &str, path_env: Option<&OsStr>, os: Os) -> Option<PathBuf> {
    let path_env = path_env?;
    for dir in std::env::split_paths(path_env) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
        if os == Os::Windows && cand.extension().is_none() {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

fn flatpak_installed(id: &str, ctx: &PathCtx) -> bool {
    let system = Path::new("/var/lib/flatpak/app").join(id);
    let user = ctx.xdg_data.as_ref().map(|d| d.join("flatpak/app").join(id));
    system.is_dir() || user.is_some_and(|u| u.is_dir())
}

pub fn detect(
    profile: &Profile,
    override_path: Option<&Path>,
    os: Os,
    ctx: &PathCtx,
    path_env: Option<&OsStr>,
) -> Option<Installed> {
    let mk = |kind| Some(Installed { profile_id: profile.id.clone(), kind });

    if let Some(p) = override_path.filter(|p| p.is_file()) {
        return mk(InstallKind::Native(p.to_path_buf()));
    }
    match os {
        Os::Linux => {
            for bin in &profile.detect.linux_bins {
                if let Some(p) = which(bin, path_env, os) {
                    return mk(InstallKind::Native(p));
                }
            }
        }
        Os::Windows => {
            for exe in &profile.detect.windows_exes {
                if let Some(p) = ctx.expand(exe).filter(|p| p.is_file()) {
                    return mk(InstallKind::Native(p));
                }
                // Also look the exe's file name up on PATH.
                if let Some(name) = Path::new(exe).file_name().and_then(|n| n.to_str()) {
                    if let Some(p) = which(name, path_env, os) {
                        return mk(InstallKind::Native(p));
                    }
                }
            }
        }
    }
    if os == Os::Linux {
        if let Some(id) = &profile.detect.flatpak_id {
            if flatpak_installed(id, ctx) {
                return mk(InstallKind::Flatpak(id.clone()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles;

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }

    #[test]
    fn finds_binary_on_path() {
        let t = tempfile::tempdir().unwrap();
        touch(&t.path().join("bin/dolphin-emu"));
        let p = profiles::get("dolphin").unwrap();
        let path = std::env::join_paths([t.path().join("bin")]).unwrap();
        let got = detect(&p, None, Os::Linux, &PathCtx::default(), Some(&path)).unwrap();
        assert_eq!(got.kind, InstallKind::Native(t.path().join("bin/dolphin-emu")));
    }

    #[test]
    fn override_beats_path_and_flatpak_is_last() {
        let t = tempfile::tempdir().unwrap();
        touch(&t.path().join("bin/ppsspp"));
        touch(&t.path().join("mine/PPSSPPSDL"));
        let p = profiles::get("ppsspp").unwrap();
        let path = std::env::join_paths([t.path().join("bin")]).unwrap();
        let ov = t.path().join("mine/PPSSPPSDL");
        let got = detect(&p, Some(&ov), Os::Linux, &PathCtx::default(), Some(&path)).unwrap();
        assert_eq!(got.kind, InstallKind::Native(ov));

        let ctx = PathCtx { xdg_data: Some(t.path().join("share")), ..Default::default() };
        std::fs::create_dir_all(t.path().join("share/flatpak/app/org.ppsspp.PPSSPP")).unwrap();
        let got = detect(&p, None, Os::Linux, &ctx, None).unwrap();
        assert_eq!(got.kind, InstallKind::Flatpak("org.ppsspp.PPSSPP".into()));
        assert!(got.is_flatpak() && got.exe_dir().is_none());
    }

    #[test]
    fn windows_known_location() {
        let t = tempfile::tempdir().unwrap();
        touch(&t.path().join("PF/PCSX2/pcsx2-qt.exe"));
        let ctx = PathCtx { programfiles: Some(t.path().join("PF")), ..Default::default() };
        let p = profiles::get("pcsx2").unwrap();
        let got = detect(&p, None, Os::Windows, &ctx, None).unwrap();
        assert_eq!(got.exe_dir().unwrap(), t.path().join("PF/PCSX2"));
    }

    #[test]
    fn none_when_absent() {
        let p = profiles::get("duckstation").unwrap();
        assert!(detect(&p, None, Os::Linux, &PathCtx::default(), None).is_none());
    }
}
