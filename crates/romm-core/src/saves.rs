//! Local save handling: hashing, folder snapshots/diffs, and the zip "bundle"
//! format used for emulators whose saves live in shared folders.
//!
//! Safety rules enforced here:
//!   * restoring a bundle never writes outside the emulator's data root;
//!   * any local file about to be overwritten is first copied to the backup dir;
//!   * writes go through a temp file + rename, so a crash can't leave a torn memory card.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

use crate::error::{Error, Result};

/// The slot romm-desktop keeps its "current" save in. RomM pairs saves on
/// (rom_id, slot), and we additionally scope negotiation to our emulator id.
pub const SLOT: &str = "autosave";
pub const MANIFEST: &str = "romm-bundle.json";

/// relative path (forward slashes) → md5
pub type Snapshot = BTreeMap<String, String>;

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

/// Hash every file under `root/<sub>` for each `sub`.
pub fn snapshot(root: &Path, subdirs: &[String]) -> std::io::Result<Snapshot> {
    let mut snap = Snapshot::new();
    for sub in subdirs {
        let dir = root.join(sub);
        if !dir.is_dir() {
            continue;
        }
        for e in WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()) {
            if e.file_type().is_file() {
                snap.insert(rel_string(root, e.path()), md5_file(e.path())?);
            }
        }
    }
    Ok(snap)
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

/// Files that are new or whose content changed. Deletions are ignored on purpose:
/// an emulator removing a file must never delete anything on the server.
pub fn changed_files(before: &Snapshot, after: &Snapshot) -> Vec<String> {
    after.iter().filter(|(k, v)| before.get(*k) != Some(*v)).map(|(k, _)| k.clone()).collect()
}

/// Version-independent identity of a set of files (unlike the zip's own bytes).
pub fn fingerprint(root: &Path, files: &[String]) -> std::io::Result<String> {
    let mut sorted: Vec<&String> = files.iter().collect();
    sorted.sort();
    let mut h = Md5::new();
    for f in sorted {
        h.update(f.as_bytes());
        h.update([0]);
        h.update(md5_file(&root.join(f))?.as_bytes());
        h.update([b'\n']);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn latest_mtime(root: &Path, files: &[String]) -> SystemTime {
    files
        .iter()
        .filter_map(|f| fs::metadata(root.join(f)).ok()?.modified().ok())
        .max()
        .unwrap_or(SystemTime::UNIX_EPOCH)
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

/// Deterministic zip: sorted entries, fixed timestamps, so identical content
/// always yields identical bytes (and therefore an identical md5).
pub fn pack_bundle(emulator: &str, root: &Path, files: &[String]) -> Result<Vec<u8>> {
    let mut names: Vec<String> = files.to_vec();
    names.sort();
    names.dedup();
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    let manifest = Manifest { format: 1, emulator: emulator.to_string(), files: names.clone() };
    zw.start_file(MANIFEST, opts)?;
    zw.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    for n in &names {
        let rel = safe_rel(n).ok_or_else(|| Error::msg(format!("unsafe path in bundle: {n}")))?;
        zw.start_file(n, opts)?;
        zw.write_all(&fs::read(root.join(rel))?)?;
    }
    Ok(zw.finish()?.into_inner())
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

/// Restore a bundle under `root`. Returns the relative paths written.
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

    #[test]
    fn snapshot_diff_detects_new_and_modified_but_not_deleted() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        write(&r.join("memcards/a.mcd"), "one");
        write(&r.join("memcards/b.mcd"), "two");
        write(&r.join("other/ignored.txt"), "x");
        let subs = vec!["memcards".to_string()];
        let before = snapshot(r, &subs).unwrap();
        assert_eq!(before.len(), 2, "only watched roots are scanned");

        write(&r.join("memcards/a.mcd"), "ONE");
        write(&r.join("memcards/sub/c.mcd"), "new");
        fs::remove_file(r.join("memcards/b.mcd")).unwrap();
        write(&r.join("other/ignored.txt"), "changed");
        let after = snapshot(r, &subs).unwrap();
        assert_eq!(changed_files(&before, &after), ["memcards/a.mcd", "memcards/sub/c.mcd"]);
    }

    #[test]
    fn bundle_is_deterministic_and_roundtrips() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        write(&src.join("GC/USA/Card A.raw"), "memcard-bytes");
        write(&src.join("Wii/title/00010000/52534245/data/save.bin"), "wii");
        let files = vec!["Wii/title/00010000/52534245/data/save.bin".to_string(), "GC/USA/Card A.raw".to_string()];

        let a = pack_bundle("dolphin", &src, &files).unwrap();
        // Different input order, later mtime: identical bytes.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&src.join("GC/USA/Card A.raw"), "memcard-bytes");
        let mut rev = files.clone();
        rev.reverse();
        let b = pack_bundle("dolphin", &src, &rev).unwrap();
        assert_eq!(md5_bytes(&a), md5_bytes(&b));
        assert!(is_zip(&a));

        let dst = t.path().join("dst");
        let written = unpack_bundle(&a, &dst, &t.path().join("bak")).unwrap();
        assert_eq!(written, ["GC/USA/Card A.raw", "Wii/title/00010000/52534245/data/save.bin"]);
        assert_eq!(fs::read_to_string(dst.join("GC/USA/Card A.raw")).unwrap(), "memcard-bytes");
        assert_eq!(fingerprint(&src, &files).unwrap(), fingerprint(&dst, &files).unwrap());
    }

    #[test]
    fn unpack_backs_up_overwritten_files_only_when_different() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        write(&src.join("memcards/x.mcd"), "server");
        let bundle = pack_bundle("duckstation", &src, &["memcards/x.mcd".into()]).unwrap();

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
