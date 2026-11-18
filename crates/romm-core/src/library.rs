//! Installing ROMs and BIOS/firmware files from RomM onto disk.

use std::fs;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Component, Path, PathBuf};

use walkdir::WalkDir;

use crate::api::types::{Platform, Rom};
use crate::api::RommClient;
use crate::error::{Error, Result};
use crate::resolve::Resolver;
use crate::saves::{md5_file, safe_component, safe_rel};
use crate::state::LocalRom;
use crate::sync::EmuEnv;

/// Which file in an extracted multi-file ROM the emulator should be pointed at
/// (lower = preferred): playlists and cue sheets before the raw data they reference.
///
/// `zar` (a Xenia archive) ranks with the disc images and a bare `xex` last. An extracted Xbox 360
/// disc is a folder of loose files (many of them `.bin`/`.img` data), so `BOOT_FILE` is checked first.
const LAUNCH_PRIORITY: &[&str] = &[
    "m3u", "cue", "gdi", "chd", "cso", "iso", "zar", "rvz", "gcz", "wia", "wbfs", "pbp", "img", "bin", "ccd", "mds",
    "xex",
];

/// The boot module of an extracted Xbox 360 disc: what Xenia must be pointed at, whatever else the
/// folder holds. No other platform has a file by this name.
const BOOT_FILE: &str = "default.xex";

pub fn find_launch_file(dir: &Path) -> Option<PathBuf> {
    // (0 for the boot module else 1, extension rank, depth of a boot module), then path: the smallest
    // wins, so the boot module nearest the root is chosen.
    let mut best: Option<((u8, usize, usize), PathBuf)> = None;
    for e in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
        let ext = e.path().extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
        let rank = LAUNCH_PRIORITY.iter().position(|x| *x == ext).unwrap_or(LAUNCH_PRIORITY.len());
        let is_boot = e.file_name().to_str().is_some_and(|n| n.eq_ignore_ascii_case(BOOT_FILE));
        let key = if is_boot { (0, rank, e.depth()) } else { (1, rank, 0) };
        let better = match &best {
            None => true,
            Some((k, p)) => key < *k || (key == *k && e.path() < p.as_path()),
        };
        if better {
            best = Some((key, e.into_path()));
        }
    }
    best.map(|(_, p)| p)
}

/// Unpacks a multi-file ROM's zip into `dest`, entry by entry, straight from the file on disk to
/// the files it holds. Neither the archive nor any one entry is ever held in memory: a disc image
/// is several GB, and the machine this runs on is somebody's desktop.
pub fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    /// Large enough to keep the disk busy, small enough to be nothing next to a disc image.
    const COPY_BUF: usize = 1 << 20;
    let mut ar = zip::ZipArchive::new(BufReader::new(fs::File::open(zip_path)?))?;
    for i in 0..ar.len() {
        let mut entry = ar.by_index(i)?;
        let rel = entry
            .enclosed_name()
            .ok_or_else(|| Error::msg(format!("unsafe path in ROM archive: {}", entry.name())))?;
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(p) = out.parent() {
            fs::create_dir_all(p)?;
        }
        // Copying to the end of the entry is also what makes the zip crate check its CRC, so a
        // damaged archive is an error here, not a quietly broken disc image.
        let mut file = BufWriter::with_capacity(COPY_BUF, fs::File::create(&out)?);
        std::io::copy(&mut entry, &mut file)?;
        file.flush()?;
    }
    Ok(())
}

/// Download a ROM into `<rom_dir>/<platform folder>/`.
///
/// Single-file ROMs are saved as-is (a `.zip` ROM stays zipped; arcade sets need that).
/// Multi-file ROMs arrive from RomM as a zip and are extracted into their own folder.
pub async fn install_rom(
    client: &RommClient,
    rom: &Rom,
    rom_dir: &Path,
    progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
) -> Result<LocalRom> {
    // Both names come from the server and become directories under `rom_dir`. A name that climbs out
    // of it (`..`) or replaces it (an absolute path replaces the base in `join`) would put the
    // download, and later the uninstall's `remove_dir_all`, anywhere the user can write.
    let platform_dir = rom_dir.join(
        safe_component(&rom.platform_fs_slug)
            .ok_or_else(|| Error::msg(format!("the server gave an unsafe platform folder name ({:?}); nothing was downloaded", rom.platform_fs_slug)))?,
    );
    let (path, launch_path) = if rom.has_multiple_files {
        let folder_name = if rom.fs_name_no_ext.is_empty() { &rom.fs_name } else { &rom.fs_name_no_ext };
        let folder_name = safe_component(folder_name)
            .ok_or_else(|| Error::msg(format!("the server gave an unsafe folder name for this ROM ({folder_name:?}); nothing was downloaded")))?;
        let folder_name = folder_name.to_string_lossy().into_owned();
        let folder = platform_dir.join(&folder_name);
        let tmp_zip = platform_dir.join(format!(".{folder_name}.download.zip"));
        client.download_rom(rom, &tmp_zip, progress).await?;
        let (z, f) = (tmp_zip.clone(), folder.clone());
        let res = tokio::task::spawn_blocking(move || extract_zip(&z, &f))
            .await
            .map_err(|e| Error::msg(e.to_string()))?;
        let _ = fs::remove_file(&tmp_zip);
        res?;
        let launch = find_launch_file(&folder)
            .ok_or_else(|| Error::msg(format!("no playable file found in {}", folder.display())))?;
        (folder, launch)
    } else {
        let dest = platform_dir.join(safe_rel(&rom.fs_name).ok_or_else(|| Error::msg("unsafe ROM file name"))?);
        let already = fs::metadata(&dest).map(|m| rom.fs_size_bytes > 0 && m.len() == rom.fs_size_bytes).unwrap_or(false);
        if !already {
            client.download_rom(rom, &dest, progress).await?;
        }
        (dest.clone(), dest)
    };
    let stem = launch_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(LocalRom {
        rom_id: rom.id,
        name: rom.title().to_string(),
        platform_slug: rom.platform_slug.clone(),
        path,
        root: Some(rom_dir.to_path_buf()),
        launch_path,
        stem,
        cover_path: rom.path_cover_small.clone(),
        identity: None,
    })
}

/// Whether uninstalling `rom` may delete what its `path` names. It may only if that is something an
/// install puts under the ROM folder: inside the folder it was installed into (`rom.root`; for a
/// game installed before that was recorded, `current_rom_dir`), with no `..` in the way, and below
/// the platform folder: the ROM folder itself and a platform's folder are never a game.
///
/// The check is by path, not by following links, so a platform folder that is a symlink to another
/// disk (a common setup) still works. `path` comes from `state.json`, which an older version filled
/// from names the server chose, so this is the last line of defence and a cheap one.
pub fn ensure_removable(rom: &LocalRom, current_rom_dir: &Path) -> Result<()> {
    let root = rom.root.as_deref().unwrap_or(current_rom_dir);
    let outside = || {
        Error::msg(format!(
            "{} is not inside the ROM folder ({}), so it was not deleted. Delete it yourself if it is yours.",
            rom.path.display(),
            root.display()
        ))
    };
    let rel = rom.path.strip_prefix(root).map_err(|_| outside())?;
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(outside());
    }
    // A folder is `<platform>/<game>`; a single file may sit directly in the ROM folder in an old install.
    let shallowest = if rom.path.is_dir() { 2 } else { 1 };
    if rel.components().count() < shallowest {
        return Err(Error::msg(format!(
            "{} is the ROM folder or a platform folder, not a game, so it was not deleted.",
            rom.path.display()
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BiosStatus {
    Installed,
    AlreadyPresent,
    /// Downloaded, but RomM has no checksum to verify it against.
    InstalledUnverified,
}

#[derive(Debug, Clone)]
pub struct BiosOutcome {
    pub file: String,
    pub status: BiosStatus,
}

impl EmuEnv {
    /// Where this emulator looks for BIOS files: RetroArch's own `system` folder, else the profile's
    /// folder under the emulator's data.
    pub fn bios_dir(&self) -> Result<PathBuf> {
        let rule = self.profile.bios.as_ref().ok_or_else(|| Error::msg(format!("{} has no known BIOS folder", self.profile.name)))?;
        Ok(match &self.retroarch {
            Some(layout) => layout.system.clone(),
            None => self.data_root.join(&rule.dir),
        })
    }
}

/// Installs the BIOS files RomM holds for `platform_slug` into the folder the emulator chosen for
/// that platform reads them from. Returns that folder and what became of each file.
pub async fn install_platform_bios(
    client: &RommClient,
    resolver: &Resolver<'_>,
    platform_slug: &str,
    progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
) -> Result<(PathBuf, Vec<BiosOutcome>)> {
    let platform = client
        .platforms()
        .await?
        .into_iter()
        .find(|p| p.slug == platform_slug)
        .ok_or_else(|| Error::msg(format!("no platform '{platform_slug}' on the server")))?;
    if platform.firmware.is_empty() {
        return Err(Error::msg(format!("RomM has no BIOS files for {}. Add them to the platform's bios folder on the server and rescan.", platform.title())));
    }
    let (env, _) = resolver.emu_env(&resolver.emulator_for(platform_slug)?)?;
    let dir = env.bios_dir()?;
    let outcomes = install_bios(client, &platform, &dir, progress).await?;
    Ok((dir, outcomes))
}

/// Install a platform's firmware into `dest_dir`, verifying each file's MD5 against RomM's record.
pub async fn install_bios(
    client: &RommClient,
    platform: &Platform,
    dest_dir: &Path,
    progress: &mut (dyn FnMut(u64, Option<u64>) + Send),
) -> Result<Vec<BiosOutcome>> {
    let mut out = Vec::new();
    for fw in platform.firmware.iter().filter(|f| !f.missing_from_fs) {
        let rel = safe_rel(&fw.file_name).ok_or_else(|| Error::msg(format!("unsafe firmware name: {}", fw.file_name)))?;
        let dest = dest_dir.join(rel);
        let want = fw.md5_hash.to_ascii_lowercase();
        if dest.is_file() && !want.is_empty() && md5_file(&dest)? == want {
            out.push(BiosOutcome { file: fw.file_name.clone(), status: BiosStatus::AlreadyPresent });
            continue;
        }
        // Beside the destination, not onto it: the file already there (it may be the one that works)
        // is replaced only by a download that has been checked, never by one that then fails the check.
        let side = dest.with_file_name(format!(".{}.verifying", dest.file_name().unwrap_or_default().to_string_lossy()));
        client.download_firmware(fw, &side, progress).await?;
        if want.is_empty() {
            fs::rename(&side, &dest)?;
            out.push(BiosOutcome { file: fw.file_name.clone(), status: BiosStatus::InstalledUnverified });
            continue;
        }
        let got = match md5_file(&side) {
            Ok(got) => got,
            Err(e) => {
                let _ = fs::remove_file(&side);
                return Err(e.into());
            }
        };
        if got != want {
            let _ = fs::remove_file(&side);
            let kept = if dest.is_file() { " Your existing file was left as it was." } else { "" };
            return Err(Error::msg(format!(
                "{}: checksum mismatch (expected {want}, got {got}); the download was discarded.{kept}",
                fw.file_name
            )));
        }
        fs::rename(&side, &dest)?;
        out.push(BiosOutcome { file: fw.file_name.clone(), status: BiosStatus::Installed });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(id: &str, root: &str) -> EmuEnv {
        EmuEnv { profile: crate::profiles::get(id).unwrap(), data_root: PathBuf::from(root), retroarch: None, core: None }
    }

    #[test]
    fn a_standalone_emulators_bios_folder_is_under_its_data() {
        assert_eq!(env("pcsx2", "/data/pcsx2").bios_dir().unwrap(), PathBuf::from("/data/pcsx2/bios"));
    }

    #[test]
    fn retroarch_keeps_bios_files_in_its_own_system_folder() {
        let root = std::path::Path::new("/ra");
        let mut e = env("retroarch", "/ra");
        e.retroarch = Some(crate::retroarch::layout(root, crate::paths::Os::Linux, &crate::paths::PathCtx::default()));
        assert_eq!(e.bios_dir().unwrap(), e.retroarch.as_ref().unwrap().system);
    }

    #[test]
    fn an_emulator_with_no_bios_rule_has_no_folder_to_name() {
        let err = env("dolphin", "/data/dolphin").bios_dir().unwrap_err().to_string();
        assert_eq!(err, "Dolphin has no known BIOS folder");
    }

    #[test]
    fn launch_file_prefers_playlist_then_cue_over_bin() {
        let t = tempfile::tempdir().unwrap();
        for f in ["Game/Game (Disc 1).bin", "Game/Game (Disc 1).cue", "Game/Game.m3u", "Game/readme.txt"] {
            let p = t.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        }
        assert_eq!(find_launch_file(&t.path().join("Game")).unwrap(), t.path().join("Game/Game.m3u"));
        fs::remove_file(t.path().join("Game/Game.m3u")).unwrap();
        assert_eq!(find_launch_file(&t.path().join("Game")).unwrap(), t.path().join("Game/Game (Disc 1).cue"));
        assert!(find_launch_file(&t.path().join("nope")).is_none());
    }


    fn touch_all(root: &Path, files: &[&str]) {
        for f in files {
            let p = root.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        }
    }

    /// An extracted Xbox 360 disc is loose files, plenty of them `.bin` data and several `.xex`.
    #[test]
    fn an_extracted_xbox_360_disc_launches_default_xex() {
        let t = tempfile::tempdir().unwrap();
        touch_all(t.path(), &["Game/Data/data0.bin", "Game/Data/data1.img", "Game/Mods/a.xex", "Game/default.xex", "Game/Zed/default.xex"]);
        // `Zed/` sorts before `default.xex` by path, but the one at the root is the boot module.
        assert_eq!(find_launch_file(&t.path().join("Game")).unwrap(), t.path().join("Game/default.xex"));
        // The boot module is matched without regard to case, as on the disc's own filesystem.
        let u = tempfile::tempdir().unwrap();
        touch_all(u.path(), &["G/a.bin", "G/DEFAULT.XEX"]);
        assert_eq!(find_launch_file(&u.path().join("G")).unwrap(), u.path().join("G/DEFAULT.XEX"));
    }

    #[test]
    fn xbox_360_images_and_bare_xex_files_are_launchable_without_a_default_xex() {
        let t = tempfile::tempdir().unwrap();
        touch_all(t.path(), &["Img/readme.txt", "Img/Halo 3.zar", "Img/Halo 3.iso"]);
        assert_eq!(find_launch_file(&t.path().join("Img")).unwrap(), t.path().join("Img/Halo 3.iso"));
        fs::remove_file(t.path().join("Img/Halo 3.iso")).unwrap();
        assert_eq!(find_launch_file(&t.path().join("Img")).unwrap(), t.path().join("Img/Halo 3.zar"));

        let u = tempfile::tempdir().unwrap();
        touch_all(u.path(), &["X/notes.txt", "X/game.xex"]);
        assert_eq!(find_launch_file(&u.path().join("X")).unwrap(), u.path().join("X/game.xex"));
    }

    fn installed(root: Option<&Path>, path: &Path) -> LocalRom {
        LocalRom { rom_id: 1, path: path.to_path_buf(), root: root.map(Path::to_path_buf), ..Default::default() }
    }

    #[test]
    fn a_game_inside_the_rom_folder_may_be_uninstalled() {
        let t = tempfile::tempdir().unwrap();
        touch_all(t.path(), &["psx/Game/Game.m3u", "psx/Game.iso", "psx/sub/Deep.iso"]);
        for p in ["psx/Game", "psx/Game.iso", "psx/sub/Deep.iso"] {
            ensure_removable(&installed(Some(t.path()), &t.path().join(p)), Path::new("/unrelated")).unwrap_or_else(|e| panic!("{p}: {e}"));
        }
    }

    #[test]
    fn the_rom_folder_and_a_platform_folder_are_never_a_game() {
        let t = tempfile::tempdir().unwrap();
        touch_all(t.path(), &["psx/Game/Game.m3u"]);
        for p in [t.path().to_path_buf(), t.path().join("psx"), t.path().join("psx/")] {
            let e = ensure_removable(&installed(Some(t.path()), &p), t.path()).unwrap_err().to_string();
            assert!(e.contains("not a game"), "{}: {e}", p.display());
        }
    }

    #[test]
    fn a_path_that_climbs_out_or_points_elsewhere_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        touch_all(t.path(), &["psx/Game/Game.m3u"]);
        touch_all(other.path(), &["Documents/a.txt"]);
        let climbing = t.path().join("psx/../..");
        let up_and_down = t.path().join("../elsewhere");
        for p in [climbing, up_and_down, other.path().join("Documents"), PathBuf::from("/"), PathBuf::from("relative/game")] {
            let e = ensure_removable(&installed(Some(t.path()), &p), t.path()).unwrap_err().to_string();
            assert!(e.contains("not inside the ROM folder"), "{}: {e}", p.display());
        }
    }

    #[test]
    fn an_install_is_judged_against_the_folder_it_was_made_in_not_the_one_set_since() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        touch_all(old.path(), &["psx/Game/Game.m3u"]);
        // The user moved the ROM folder in settings; the game still lives in the old one.
        ensure_removable(&installed(Some(old.path()), &old.path().join("psx/Game")), new.path()).unwrap();
        // One installed before the folder was recorded can only be judged against the current one.
        let e = ensure_removable(&installed(None, &old.path().join("psx/Game")), new.path()).unwrap_err().to_string();
        assert!(e.contains("not inside the ROM folder"), "{e}");
        ensure_removable(&installed(None, &old.path().join("psx/Game")), old.path()).unwrap();
    }

    #[test]
    fn a_file_directly_in_the_rom_folder_is_still_removable_but_a_folder_there_is_not() {
        let t = tempfile::tempdir().unwrap();
        touch_all(t.path(), &["Loose.iso", "Folder/a.bin"]);
        ensure_removable(&installed(Some(t.path()), &t.path().join("Loose.iso")), t.path()).unwrap();
        assert!(ensure_removable(&installed(Some(t.path()), &t.path().join("Folder")), t.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_platform_folder_that_is_a_symlink_to_another_disk_still_works() {
        let t = tempfile::tempdir().unwrap();
        let disk = tempfile::tempdir().unwrap();
        touch_all(disk.path(), &["Game/Game.m3u"]);
        std::os::unix::fs::symlink(disk.path(), t.path().join("psx")).unwrap();
        ensure_removable(&installed(Some(t.path()), &t.path().join("psx/Game")), t.path()).unwrap();
    }
}
