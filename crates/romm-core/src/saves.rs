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

/// MD5 of everything `r` yields, read in blocks so a large save is never held in memory twice.
pub fn md5_reader<R: Read>(r: &mut R) -> std::io::Result<String> {
    let mut h = Md5::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn md5_file(path: &Path) -> std::io::Result<String> {
    md5_reader(&mut fs::File::open(path)?)
}

/// Most entries an archive from the server may have before it is refused outright. A save is a
/// handful of files; this is for what is not a save, or is meant to do harm.
pub const MAX_ARCHIVE_ENTRIES: usize = 20_000;

/// Reads one archive entry in full, out of what is left of `budget` (which it reduces). `None` is
/// "too much": the entry says it is over budget, or turns out to be. What counts is what the entry
/// actually yields, not the size its header claims, so an archive can't understate itself.
pub fn read_within_budget<R: Read>(entry: &mut R, declared_size: u64, budget: &mut u64) -> std::io::Result<Option<Vec<u8>>> {
    if declared_size > *budget {
        return Ok(None);
    }
    let mut data = Vec::with_capacity(declared_size.min(1 << 24) as usize);
    entry.take(*budget + 1).read_to_end(&mut data)?;
    if data.len() as u64 > *budget {
        return Ok(None);
    }
    *budget -= data.len() as u64;
    Ok(Some(data))
}

/// Exactly `len` hex digits.
pub fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// An Xbox title id (original Xbox and Xbox 360 alike): 8 hex digits. It names a game's save
/// folder in xemu's disk image and in Xenia's content folder.
pub fn is_title_id(s: &str) -> bool {
    is_hex(s, 8)
}

pub fn iso(t: SystemTime) -> String {
    DateTime::<Utc>::from(t).format(ISO_FORMAT).to_string()
}

/// How RomM writes a timestamp.
pub const ISO_FORMAT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// `20260109T143000`: what a backup folder or a conflict archive is named after, so each is told
/// apart from the others made on other occasions.
pub fn stamp() -> String {
    Utc::now().format("%Y%m%dT%H%M%S").to_string()
}

/// Archived copies kept during conflict resolution carry this marker in their name and are never
/// synced back.
pub const CONFLICT_MARK: &str = ".conflict-";

/// What a local copy that lost a conflict is called when it is kept on the server: `base`, the
/// marker, then a time stamp (`Zelda.state1.conflict-20260109T143000`). A save gives `base`
/// without its extension and puts the extension after the stamp.
pub fn conflict_name(base: &str) -> String {
    format!("{base}{CONFLICT_MARK}{}", stamp())
}

/// Where what a sync is about to overwrite is copied first: `<backups>/<stamp>/<emulator>`. Each
/// occasion gets a folder of its own, so a later backup can't overwrite an earlier one.
pub fn backup_dir_for(backups: &Path, emulator: &str) -> PathBuf {
    backups.join(stamp()).join(emulator)
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
    unpack_bundle_within(bytes, root, backup_dir, MAX_ARCHIVE_ENTRIES, MAX_BUNDLE_BYTES)
}

/// A legacy bundle holds a few save files and memory cards; nothing legitimate comes near this.
const MAX_BUNDLE_BYTES: u64 = 1 << 30;

fn unpack_bundle_within(bytes: &[u8], root: &Path, backup_dir: &Path, max_entries: usize, max_bytes: u64) -> Result<Vec<String>> {
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
    if manifest.files.len() > max_entries {
        return Err(Error::msg(format!("the save bundle lists {} files; refusing to unpack it", manifest.files.len())));
    }
    // Validate every path, and read every file, up front so we never half-restore.
    for f in &manifest.files {
        safe_rel(f).ok_or_else(|| Error::msg(format!("refusing unsafe path in bundle: {f}")))?;
    }
    let mut budget = max_bytes;
    let mut contents = Vec::with_capacity(manifest.files.len());
    for f in &manifest.files {
        let mut entry = ar.by_name(f)?;
        let size = entry.size();
        let data = read_within_budget(&mut entry, size, &mut budget)?
            .ok_or_else(|| Error::msg(format!("the save bundle unpacks to more than {} MiB; refusing to unpack it", max_bytes >> 20)))?;
        contents.push(data);
    }
    let mut written = Vec::new();
    for (f, data) in manifest.files.iter().zip(contents) {
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

    #[test]
    fn streaming_md5_agrees_with_hashing_the_whole_buffer_at_every_block_boundary() {
        // The reader works in 64 KiB blocks: empty, one byte, either side of a block, several blocks.
        for len in [0usize, 1, 64 * 1024 - 1, 64 * 1024, 64 * 1024 + 1, 3 * 64 * 1024 + 17] {
            let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
            assert_eq!(md5_reader(&mut Cursor::new(&data)).unwrap(), md5_bytes(&data), "{len} bytes");
        }
    }

    #[test]
    fn md5_of_a_file_is_the_md5_of_its_contents() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("a.bin");
        let data = vec![7u8; 100_000];
        fs::write(&p, &data).unwrap();
        assert_eq!(md5_file(&p).unwrap(), md5_bytes(&data));
        assert!(md5_file(&t.path().join("missing")).is_err());
    }

    #[test]
    fn a_title_id_is_exactly_eight_hex_digits_in_either_case() {
        for ok in ["4D5307E6", "4d5307e6", "00000000"] {
            assert!(is_title_id(ok), "{ok}");
        }
        for bad in ["", "4D5307E", "4D5307E6A", "4D5307EG", " 4D5307E6", "4D53 7E6"] {
            assert!(!is_title_id(bad), "{bad:?}");
        }
        // Xenia's profile folders are named by a 16-digit XUID.
        assert!(is_hex("0123456789ABCDEF", 16) && !is_hex("0123456789ABCDE", 16));
    }

    #[test]
    fn a_stamp_is_a_sortable_second_resolution_timestamp() {
        let s = stamp();
        // 20260109T143000
        assert_eq!(s.len(), 15, "{s}");
        assert_eq!(s.as_bytes()[8], b'T', "{s}");
        assert!(s.bytes().enumerate().all(|(i, b)| i == 8 || b.is_ascii_digit()), "{s}");
    }

    #[test]
    fn a_conflict_name_is_the_base_then_the_marker_then_a_stamp() {
        let n = conflict_name("Zelda.state1");
        let stamp_part = n.strip_prefix("Zelda.state1.conflict-").unwrap_or_else(|| panic!("{n}"));
        assert_eq!(stamp_part.len(), 15, "{n}");
        assert!(n.contains(CONFLICT_MARK), "what the state sync filters out must be what is written: {n}");
    }

    #[test]
    fn a_backup_folder_is_the_stamp_then_the_emulator_under_the_backups() {
        let d = backup_dir_for(Path::new("/b"), "dolphin");
        assert_eq!(d.parent().and_then(Path::parent), Some(Path::new("/b")), "{d:?}");
        assert_eq!(d.file_name().and_then(|n| n.to_str()), Some("dolphin"));
        assert_eq!(d.parent().and_then(|p| p.file_name()).map(|n| n.len()), Some(15), "{d:?}");
    }

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

    struct MustNotBeRead;
    impl Read for MustNotBeRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("an entry that already says it is too big must not be read at all");
        }
    }

    #[test]
    fn an_entry_within_budget_is_read_and_charged_for() {
        let mut budget = 10;
        assert_eq!(read_within_budget(&mut Cursor::new(b"abcd"), 4, &mut budget).unwrap().as_deref(), Some(&b"abcd"[..]));
        assert_eq!(budget, 6);
        // The budget is shared by every entry of the archive, and may be used exactly.
        assert_eq!(read_within_budget(&mut Cursor::new(b"123456"), 6, &mut budget).unwrap().as_deref(), Some(&b"123456"[..]));
        assert_eq!(budget, 0);
        assert_eq!(read_within_budget(&mut Cursor::new(b""), 0, &mut budget).unwrap().as_deref(), Some(&b""[..]));
        assert_eq!(read_within_budget(&mut Cursor::new(b"x"), 1, &mut budget).unwrap(), None);
    }

    #[test]
    fn an_entry_that_admits_it_is_too_big_is_not_read() {
        let mut budget = 10;
        assert_eq!(read_within_budget(&mut MustNotBeRead, 11, &mut budget).unwrap(), None);
        assert_eq!(budget, 10);
    }

    #[test]
    fn an_entry_that_understates_its_size_is_still_caught_by_what_it_yields() {
        let mut budget = 50;
        // Its header says 10 bytes; it yields 100.
        assert_eq!(read_within_budget(&mut Cursor::new(vec![0u8; 100]), 10, &mut budget).unwrap(), None);
    }

    #[test]
    fn a_legacy_bundle_with_too_many_files_is_refused_and_nothing_is_written() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        for f in ["a.sav", "b.sav", "c.sav"] {
            write(&src.join(f), "x");
        }
        let bundle = legacy_bundle("dolphin", &src, &["a.sav", "b.sav", "c.sav"]);
        let dst = t.path().join("dst");

        let err = unpack_bundle_within(&bundle, &dst, &t.path().join("bak"), 2, 1 << 20).unwrap_err().to_string();

        assert!(err.contains("lists 3 files"), "{err}");
        assert!(!dst.exists());
        // At the limit is fine.
        assert_eq!(unpack_bundle_within(&bundle, &dst, &t.path().join("bak"), 3, 1 << 20).unwrap().len(), 3);
    }

    #[test]
    fn a_legacy_bundle_that_unpacks_to_too_much_is_refused_before_any_file_is_written() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        write(&src.join("a.sav"), &"a".repeat(600));
        write(&src.join("b.sav"), &"b".repeat(600));
        let bundle = legacy_bundle("dolphin", &src, &["a.sav", "b.sav"]);
        let dst = t.path().join("dst");

        // Each file is within the budget on its own; together they are not. The first must not be
        // left behind: a bundle is restored whole or not at all.
        let err = unpack_bundle_within(&bundle, &dst, &t.path().join("bak"), 10, 1000).unwrap_err().to_string();

        assert!(err.contains("unpacks to more than"), "{err}");
        assert!(!dst.join("a.sav").exists() && !dst.join("b.sav").exists(), "nothing may be half-restored");
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
