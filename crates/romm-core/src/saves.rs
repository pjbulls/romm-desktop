//! Local file helpers: hashing, safe paths, atomic writes, state-file snapshots, and the reader
//! for the zip "bundle" older romm-desktop versions uploaded.
//!
//! What a game's save *is*, and the format it travels in, is decided by `unit` (Sigil, as in
//! Argosy). The bundle format here (a zip with a `romm-bundle.json` manifest and data-root-relative
//! paths) is no longer written; it is only read, so saves uploaded before the switch still restore.
//!
//! Safety rules enforced here:
//!   * restoring a bundle never writes outside the emulator's data root;
//!   * any local file about to be overwritten is first copied to the backup dir;
//!   * writes go through a temp file + rename, so a crash can't leave a torn memory card.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;
use zip::ZipArchive;

use crate::error::{Error, Result};

/// The slot romm-desktop keeps its "current" save in. RomM pairs saves on
/// (rom_id, slot), and we additionally scope negotiation to our emulator id.
pub const SLOT: &str = "autosave";
pub const MANIFEST: &str = "romm-bundle.json";

pub fn md5_bytes(b: &[u8]) -> String {
    hex::encode(Md5::digest(b))
}

pub fn md5_file(path: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Md5::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn iso(t: SystemTime) -> String {
    DateTime::<Utc>::from(t).format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn rel_string(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// A relative path that stays inside its root (no `..`, no absolute paths, no drive prefixes).
pub fn safe_rel(s: &str) -> Option<PathBuf> {
    let p = Path::new(s);
    if s.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(p.to_path_buf())
}

/// (size, mtime in ns) per file; much cheaper than hashing for large save states.
pub type StatSnapshot = BTreeMap<String, (u64, u128)>;

pub fn mtime_ns(t: SystemTime) -> u128 {
    t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

pub fn snapshot_stat(root: &Path, subdirs: &[String]) -> std::io::Result<StatSnapshot> {
    let mut snap = StatSnapshot::new();
    for sub in subdirs {
        let dir = root.join(sub);
        if !dir.is_dir() {
            continue;
        }
        for e in WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
            let m = e.metadata().map_err(std::io::Error::other)?;
            snap.insert(rel_string(root, e.path()), (m.len(), mtime_ns(m.modified()?)));
        }
    }
    Ok(snap)
}

pub fn changed_stat(before: &StatSnapshot, after: &StatSnapshot) -> Vec<String> {
    after.iter().filter(|(k, v)| before.get(*k) != Some(*v)).map(|(k, _)| k.clone()).collect()
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Manifest {
    pub format: u32,
    pub emulator: String,
    pub files: Vec<String>,
}

pub fn is_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

/// A zip an older romm-desktop uploaded: it carries the `romm-bundle.json` manifest and paths
/// relative to the emulator's data folder. Argosy's units never contain that file.
pub fn is_legacy_bundle(bytes: &[u8]) -> bool {
    is_zip(bytes) && ZipArchive::new(Cursor::new(bytes)).is_ok_and(|mut z| z.by_name(MANIFEST).is_ok())
}

/// Copy `path` (if it exists) to `backup_dir/<rel>`.
pub fn backup_file(path: &Path, backup_dir: &Path, rel: &str) -> std::io::Result<()> {
    if path.is_file() {
        let dest = backup_dir.join(rel);
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p)?;
        }
        fs::copy(path, dest)?;
    }
    Ok(())
}

/// Write via temp file + rename in the same directory.
pub fn write_atomic(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".romm-tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, dest)
}

/// Restore an older romm-desktop bundle under `root`. Returns the relative paths written.
pub fn unpack_bundle(bytes: &[u8], root: &Path, backup_dir: &Path) -> Result<Vec<String>> {
    let mut ar = ZipArchive::new(Cursor::new(bytes))?;
    let manifest: Manifest = {
        let mut m = ar
            .by_name(MANIFEST)
            .map_err(|_| Error::msg("not a romm-desktop save bundle (no manifest)"))?;
        let mut s = String::new();
        m.read_to_string(&mut s)?;
        serde_json::from_str(&s)?
    };
    if manifest.format != 1 {
        return Err(Error::msg(format!("unsupported bundle format {}", manifest.format)));
    }
    // Validate every path up front so we never half-restore.
    for f in &manifest.files {
        safe_rel(f).ok_or_else(|| Error::msg(format!("refusing unsafe path in bundle: {f}")))?;
    }
    let mut written = Vec::new();
    for f in &manifest.files {
        let mut data = Vec::new();
        ar.by_name(f)?.read_to_end(&mut data)?;
        let dest = root.join(safe_rel(f).expect("validated"));
        if dest.is_file() && fs::read(&dest)? != data {
            backup_file(&dest, backup_dir, f)?;
        }
        write_atomic(&dest, &data)?;
        written.push(f.clone());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    /// A bundle exactly as older romm-desktop versions wrote it.
    fn legacy_bundle(emulator: &str, root: &Path, files: &[&str]) -> Vec<u8> {
        use std::io::Write;
        use zip::{write::SimpleFileOptions, ZipWriter};
        let opts = SimpleFileOptions::default();
        let mut names: Vec<String> = files.iter().map(|f| f.to_string()).collect();
        names.sort();
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        let manifest = Manifest { format: 1, emulator: emulator.to_string(), files: names.clone() };
        zw.start_file(MANIFEST, opts).unwrap();
        zw.write_all(&serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        for n in &names {
            zw.start_file(n, opts).unwrap();
            zw.write_all(&fs::read(root.join(n)).unwrap()).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    #[test]
    fn legacy_bundles_are_recognised_and_unpacked_under_the_data_root() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        write(&src.join("SAVEDATA/ULUS10064DATA00/DATA.BIN"), "psp-save");
        write(&src.join("SAVEDATA/ULUS10064DATA00/PARAM.SFO"), "sfo");
        let bundle = legacy_bundle("ppsspp", &src, &["SAVEDATA/ULUS10064DATA00/DATA.BIN", "SAVEDATA/ULUS10064DATA00/PARAM.SFO"]);
        assert!(is_legacy_bundle(&bundle));

        let dst = t.path().join("dst");
        let written = unpack_bundle(&bundle, &dst, &t.path().join("bak")).unwrap();
        assert_eq!(written, ["SAVEDATA/ULUS10064DATA00/DATA.BIN", "SAVEDATA/ULUS10064DATA00/PARAM.SFO"]);
        assert_eq!(fs::read_to_string(dst.join("SAVEDATA/ULUS10064DATA00/DATA.BIN")).unwrap(), "psp-save");
    }

    #[test]
    fn an_argosy_unit_is_not_a_legacy_bundle() {
        use std::io::Write;
        use zip::{write::SimpleFileOptions, ZipWriter};
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        zw.start_file("ULUS10064DATA00/PARAM.SFO", SimpleFileOptions::default()).unwrap();
        zw.write_all(b"x").unwrap();
        let unit = zw.finish().unwrap().into_inner();
        assert!(is_zip(&unit));
        assert!(!is_legacy_bundle(&unit));
        assert!(!is_legacy_bundle(b"\x00\x01SRAM"));
    }

    #[test]
    fn unpack_backs_up_overwritten_files_only_when_different() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        write(&src.join("memcards/x.mcd"), "server");
        let bundle = legacy_bundle("duckstation", &src, &["memcards/x.mcd"]);

        let dst = t.path().join("dst");
        write(&dst.join("memcards/x.mcd"), "local-progress");
        let bak = t.path().join("bak");
        unpack_bundle(&bundle, &dst, &bak).unwrap();
        assert_eq!(fs::read_to_string(dst.join("memcards/x.mcd")).unwrap(), "server");
        assert_eq!(fs::read_to_string(bak.join("memcards/x.mcd")).unwrap(), "local-progress");

        // Same content again: nothing new to back up.
        fs::remove_dir_all(&bak).unwrap();
        unpack_bundle(&bundle, &dst, &bak).unwrap();
        assert!(!bak.exists());
    }

    #[test]
    fn rejects_path_traversal_in_manifest() {
        use std::io::Write;
        use zip::{write::SimpleFileOptions, ZipWriter};
        let t = tempfile::tempdir().unwrap();
        let opts = SimpleFileOptions::default();
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        zw.start_file(MANIFEST, opts).unwrap();
        zw.write_all(br#"{"format":1,"emulator":"x","files":["../evil.txt"]}"#).unwrap();
        zw.start_file("../evil.txt", opts).unwrap();
        zw.write_all(b"pwned").unwrap();
        let bytes = zw.finish().unwrap().into_inner();

        let root = t.path().join("root");
        let err = unpack_bundle(&bytes, &root, &t.path().join("bak")).unwrap_err();
        assert!(err.to_string().contains("unsafe path"), "{err}");
        assert!(!t.path().join("evil.txt").exists());
    }

    #[test]
    fn plain_files_are_not_bundles() {
        let t = tempfile::tempdir().unwrap();
        assert!(!is_zip(b"\x00\x01SRAM"));
        let err = unpack_bundle(b"nope", t.path(), t.path()).unwrap_err();
        assert!(matches!(err, Error::Zip(_)));
    }

    #[test]
    fn safe_rel_rules() {
        assert!(safe_rel("a/b.txt").is_some());
        assert!(safe_rel("../x").is_none());
        assert!(safe_rel("/etc/passwd").is_none());
        assert!(safe_rel("a/../../x").is_none());
        assert!(safe_rel("").is_none());
    }
}
