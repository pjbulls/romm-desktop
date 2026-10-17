//! Installing ROMs and BIOS/firmware files from RomM onto disk.

use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::api::types::{Platform, Rom};
use crate::api::RommClient;
use crate::error::{Error, Result};
use crate::saves::{md5_file, safe_rel};
use crate::state::LocalRom;

/// Which file in an extracted multi-file ROM the emulator should be pointed at
/// (lower = preferred): playlists and cue sheets before the raw data they reference.
const LAUNCH_PRIORITY: &[&str] = &[
    "m3u", "cue", "gdi", "chd", "cso", "iso", "rvz", "gcz", "wia", "wbfs", "pbp", "img", "bin", "ccd", "mds",
];

pub fn find_launch_file(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    for e in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
        let ext = e.path().extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
        let rank = LAUNCH_PRIORITY.iter().position(|x| *x == ext).unwrap_or(LAUNCH_PRIORITY.len());
        let better = match &best {
            None => true,
            Some((r, p)) => rank < *r || (rank == *r && e.path() < p.as_path()),
        };
        if better {
            best = Some((rank, e.into_path()));
        }
    }
    best.map(|(_, p)| p)
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    let bytes = fs::read(zip_path)?;
    let mut ar = zip::ZipArchive::new(Cursor::new(bytes))?;
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
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        fs::write(&out, data)?;
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
    let platform_dir = rom_dir.join(&rom.platform_fs_slug);
    let (path, launch_path) = if rom.has_multiple_files {
        let folder_name = if rom.fs_name_no_ext.is_empty() { rom.fs_name.clone() } else { rom.fs_name_no_ext.clone() };
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
        launch_path,
        stem,
        cover_path: rom.path_cover_small.clone(),
    })
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
        client.download_firmware(fw, &dest, progress).await?;
        if want.is_empty() {
            out.push(BiosOutcome { file: fw.file_name.clone(), status: BiosStatus::InstalledUnverified });
            continue;
        }
        let got = md5_file(&dest)?;
        if got != want {
            let _ = fs::remove_file(&dest);
            return Err(Error::msg(format!(
                "{}: checksum mismatch (expected {want}, got {got}); file removed",
                fw.file_name
            )));
        }
        out.push(BiosOutcome { file: fw.file_name.clone(), status: BiosStatus::Installed });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
