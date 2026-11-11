//! Xenia (Canary) saves: a game's saved-game folder, zipped the way Argosy zips it.
//!
//! Sigil reads an Xbox 360 game's title id out of its ROM (`unit::identify`) but has no save layout
//! for Xenia, so this is the Rust counterpart of Argosy's `Xbox360FolderHandler`. Xenia keeps every
//! piece of guest content under one root, and a game's saves are a plain folder tree:
//!
//! ```text
//! <content root>/<profile XUID>/<title id>/00000001/<package>/...   saved games
//!                              <title id>/00000002/...                 DLC (never touched)
//!                              <title id>/000B0000/...                 title updates (never touched)
//!                              <title id>/Headers/00000001/<package>.header
//! ```
//!
//! (`ContentManager::ResolvePackageRoot`, `ContentPackageDirectory`.) The upload is a zip of the
//! `00000001` folder with `00000001/` as its root entry, exactly Argosy's archive, so the two clients
//! read each other's saves and RomM's content hash of them agrees.
//!
//! # The header
//!
//! A saved game's *metadata* (title id, content type, display and file name) is not inside that
//! folder: Xenia keeps it in the sibling `Headers/00000001/<package>.header`. It matters. Xenia's
//! content enumeration (`ContentManager::ListContent`) skips any package whose header is missing or
//! lacks a valid magic, so a game's "Load" list would not show a save restored without it. The
//! archive cannot carry it (that would change its layout and its hash, and break Argosy's pairing),
//! so on restore a package that has no header gets one in the legacy format Xenia still reads and
//! then upgrades itself (`XCONTENT_DATA_AGGREGATE`, see [`legacy_header`]). An existing header is
//! never touched.

use std::fs;
use std::io::{Cursor, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use walkdir::WalkDir;
use zip::write::SimpleFileOptions;

use crate::error::{Error, Result};
use crate::saves::{self, is_hex, is_title_id};
use crate::unit::{self, Identity};

/// The content type folder of saved games (`XContentType::kSavedGame`), and the archive's root.
pub const SAVED_GAME: &str = "00000001";

/// The config file Canary reads and rewrites in its storage root.
const CONFIG_NAME: &str = "xenia-canary.config.toml";
/// Where Xenia keeps a directory package's metadata, next to the content type folders.
const HEADERS_DIR: &str = "Headers";
/// Device-wide content (installed DLC, the dashboard) lives under this XUID, not under a profile.
const MACHINE_XUID: &str = "0000000000000000";
/// An archive that unpacks to more than this is not a save.
const MAX_UNPACKED: u64 = 1 << 30;

/// The title id of the game Sigil identified, as Xenia spells it in folder names (8 upper-case hex
/// digits). Sigil's own save id for an Xbox 360 game is the title id.
pub fn title_id_of(id: &Identity) -> Option<String> {
    [&id.save_id, &id.title_id].into_iter().find(|s| is_title_id(s)).map(|s| s.to_ascii_uppercase())
}

// ---------------------------------------------------------------------------------------------
// Finding the save
// ---------------------------------------------------------------------------------------------

/// Where Xenia keeps guest content: `Storage.content_root` from its config when set (relative to the
/// storage root, as Xenia reads it), else `<storage root>/content`.
pub fn content_root(storage_root: &Path) -> PathBuf {
    configured_content_root(storage_root).unwrap_or_else(|| storage_root.join("content"))
}

fn configured_content_root(storage_root: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(storage_root.join(CONFIG_NAME)).ok()?;
    let doc: toml::Table = text.parse().ok()?;
    let value = doc.get("Storage")?.get("content_root")?.as_str()?.trim();
    if value.is_empty() {
        return None;
    }
    let p = PathBuf::from(value);
    Some(if p.is_absolute() { p } else { storage_root.join(p) })
}

/// A directory entry of `dir` named `name`, matched exactly first and then without regard to case
/// (Xenia writes upper-case hex, but a copied or hand-made tree may not).
fn child(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.is_dir() {
        return Some(exact);
    }
    fs::read_dir(dir).ok()?.flatten().find(|e| e.file_name().to_str().is_some_and(|n| n.eq_ignore_ascii_case(name)) && e.path().is_dir()).map(|e| e.path())
}

/// The profile folders: 16 hex digits, not the machine's. Argosy skips the machine XUID too.
fn profile_dirs(content: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(content) else { return vec![] };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter(|e| e.file_name().to_str().is_some_and(|n| is_hex(n, 16) && n != MACHINE_XUID))
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

/// The newest write among the files under `dir`; `None` when there are none.
fn newest_file_mtime(dir: &Path) -> Option<SystemTime> {
    WalkDir::new(dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()).filter_map(|e| e.metadata().ok()?.modified().ok()).max()
}

/// How recently a profile was used: its newest saved game anywhere, else the folder's own time.
fn profile_activity(profile: &Path) -> SystemTime {
    let saves = fs::read_dir(profile)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|title| child(&title.path(), SAVED_GAME))
        .filter_map(|dir| newest_file_mtime(&dir))
        .max();
    saves.or_else(|| fs::metadata(profile).ok()?.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The saved-game folder of `title_id` that already holds saves. With several profiles it is the one
/// written most recently (Argosy picks by the newest file, too).
pub fn find_save_dir(content: &Path, title_id: &str) -> Option<PathBuf> {
    let title = title_id.to_ascii_uppercase();
    profile_dirs(content)
        .into_iter()
        .filter_map(|p| child(&p, &title))
        .filter_map(|t| child(&t, SAVED_GAME))
        .filter_map(|dir| newest_file_mtime(&dir).map(|t| (t, dir)))
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)))
        .map(|(_, dir)| dir)
}

/// Where a downloaded save goes: over the existing saved-game folder, or, for a game never played
/// here, in the most recently used profile. Never invents a profile: Xenia creates those, and a
/// folder under an XUID it does not know would be a save nobody can load.
fn restore_target(content: &Path, title_id: &str) -> Result<PathBuf> {
    let title = title_id.to_ascii_uppercase();
    if let Some(dir) = find_save_dir(content, &title) {
        return Ok(dir);
    }
    let profile = profile_dirs(content)
        .into_iter()
        .max_by(|a, b| profile_activity(a).cmp(&profile_activity(b)).then_with(|| b.cmp(a)))
        .ok_or_else(|| {
            Error::msg(format!(
                "Xenia has no profile yet ({} holds no profile folder), so there is nowhere to put the save. \
                 Start Xenia once, create a profile and sign in, then sync again",
                content.display()
            ))
        })?;
    let title_dir = child(&profile, &title).unwrap_or_else(|| profile.join(&title));
    Ok(child(&title_dir, SAVED_GAME).unwrap_or_else(|| title_dir.join(SAVED_GAME)))
}

// ---------------------------------------------------------------------------------------------
// Collecting
// ---------------------------------------------------------------------------------------------

/// A saved-game folder, zipped.
#[derive(Debug, Clone)]
pub struct Collected {
    /// The archive that is uploaded.
    pub data: Vec<u8>,
    /// RomM's `content_hash` of `data`.
    pub content_hash: String,
    /// Files (not folders) in it.
    pub files: usize,
    /// When the newest of them was written.
    pub modified: SystemTime,
}

/// Zips the saved-game folder `dir`: every file as `00000001/<path>`, every folder as an entry of its
/// own, in name order, exactly as Argosy lays it out. `None` when the folder holds no files (Argosy
/// refuses to upload an empty archive, too).
pub fn collect(dir: &Path) -> Result<Option<Collected>> {
    let opts = SimpleFileOptions::default();
    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut files = 0usize;
    let mut modified: Option<SystemTime> = None;
    for entry in WalkDir::new(dir).min_depth(1).sort_by_file_name() {
        let entry = entry.map_err(|e| Error::msg(format!("can't read {}: {e}", dir.display())))?;
        let rel = entry.path().strip_prefix(dir).expect("walkdir stays under its root");
        let Some(rel) = rel_name(rel) else {
            tracing::warn!("not archiving {}: its name is not valid text", entry.path().display());
            continue;
        };
        let name = format!("{SAVED_GAME}/{rel}");
        if entry.file_type().is_dir() {
            zw.add_directory(format!("{name}/"), opts)?;
        } else if entry.file_type().is_file() {
            zw.start_file(&name, opts)?;
            zw.write_all(&fs::read(entry.path())?)?;
            files += 1;
            if let Ok(t) = entry.metadata().map_err(std::io::Error::from).and_then(|m| m.modified()) {
                modified = modified.max(Some(t));
            }
        }
    }
    if files == 0 {
        return Ok(None);
    }
    let data = zw.finish()?.into_inner();
    let content_hash = unit::content_hash(&data);
    Ok(Some(Collected { data, content_hash, files, modified: modified.unwrap_or_else(SystemTime::now) }))
}

/// `a/b/c` for a relative path, or `None` if any part is not valid UTF-8.
fn rel_name(rel: &Path) -> Option<String> {
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

// ---------------------------------------------------------------------------------------------
// Restoring
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
struct Entry {
    rel: PathBuf,
    dir: bool,
    data: Vec<u8>,
}

/// Reads a saved-game archive fully into memory, refusing anything that is not one: a root other
/// than `00000001` (a Sigil unit, an older romm-desktop bundle, another emulator's save), a path
/// that escapes it, no files at all, or an absurd size.
fn read_archive(bytes: &[u8]) -> Result<Vec<Entry>> {
    read_archive_within(bytes, saves::MAX_ARCHIVE_ENTRIES, MAX_UNPACKED)
}

fn read_archive_within(bytes: &[u8], max_entries: usize, max_bytes: u64) -> Result<Vec<Entry>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| Error::msg("the server's save is not a zip archive, so it isn't a Xenia saved game"))?;
    if zip.len() > max_entries {
        return Err(Error::msg(format!("the save archive has {} entries; refusing to apply it", zip.len())));
    }
    let mut out = Vec::new();
    let mut budget = max_bytes;
    let mut files = 0usize;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let path = f.enclosed_name().ok_or_else(|| Error::msg(format!("unsafe path in the save archive: {}", f.name())))?;
        let mut comps = path.components();
        match comps.next() {
            Some(Component::Normal(root)) if root == SAVED_GAME => {}
            _ => {
                return Err(Error::msg(format!(
                    "the server's save isn't a Xenia saved game: {} is outside {SAVED_GAME}/ (it may belong to another emulator)",
                    f.name()
                )))
            }
        }
        let rel: PathBuf = comps.collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        if f.is_dir() {
            out.push(Entry { rel, dir: true, data: Vec::new() });
            continue;
        }
        let size = f.size();
        let data = saves::read_within_budget(&mut f, size, &mut budget)?
            .ok_or_else(|| Error::msg(format!("the save archive unpacks to more than {} MiB; refusing to apply it", max_bytes >> 20)))?;
        files += 1;
        out.push(Entry { rel, dir: false, data });
    }
    if files == 0 {
        return Err(Error::msg("the server's save archive holds no files"));
    }
    Ok(out)
}

/// What a restore did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    /// The saved-game folder that now holds the save.
    pub dir: PathBuf,
    pub files: usize,
    /// Packages that had no metadata header and were given one (see the module docs).
    pub headers_written: usize,
}

/// Replaces `title_id`'s saved-game folder with the archive's contents. The whole folder is the save,
/// so files the archive lacks are removed, as Argosy does. The new tree is built beside the old one
/// and swapped in, so a failure part-way leaves the old save as it was. The caller keeps a copy of
/// what was there (the sync engine backs it up first).
pub fn restore(content: &Path, title_id: &str, bytes: &[u8]) -> Result<Restored> {
    if !is_title_id(title_id) {
        return Err(Error::msg(format!("\"{title_id}\" is not an Xbox 360 title id")));
    }
    let entries = read_archive(bytes)?;
    let target = restore_target(content, title_id)?;
    let parent = target.parent().ok_or_else(|| Error::msg("saved-game folder has no parent"))?.to_path_buf();
    fs::create_dir_all(&parent)?;

    let stage = parent.join(format!("{SAVED_GAME}.romm-new"));
    let old = parent.join(format!("{SAVED_GAME}.romm-old"));
    let _ = fs::remove_dir_all(&stage);
    let _ = fs::remove_dir_all(&old);
    fs::create_dir_all(&stage)?;
    let mut files = 0usize;
    let staged: Result<()> = (|| {
        for e in &entries {
            let path = stage.join(&e.rel);
            if e.dir {
                fs::create_dir_all(&path)?;
            } else {
                if let Some(p) = path.parent() {
                    fs::create_dir_all(p)?;
                }
                fs::write(&path, &e.data)?;
                files += 1;
            }
        }
        Ok(())
    })();
    if let Err(e) = staged {
        let _ = fs::remove_dir_all(&stage);
        return Err(e);
    }

    if target.exists() {
        fs::rename(&target, &old)?;
    }
    if let Err(e) = fs::rename(&stage, &target) {
        if old.exists() {
            let _ = fs::rename(&old, &target);
        }
        let _ = fs::remove_dir_all(&stage);
        return Err(e.into());
    }
    let _ = fs::remove_dir_all(&old);

    let headers_written = ensure_headers(&target, title_id)?;
    Ok(Restored { dir: target, files, headers_written })
}

/// Gives every directory package under `saved` that has no metadata header one. `saved` is
/// `<title>/00000001`; the headers live in `<title>/Headers/00000001/<package>.header`.
fn ensure_headers(saved: &Path, title_id: &str) -> Result<usize> {
    let Some(title_dir) = saved.parent() else { return Ok(0) };
    let Ok(tid) = u32::from_str_radix(title_id, 16) else { return Ok(0) };
    let headers = child(title_dir, HEADERS_DIR).unwrap_or_else(|| title_dir.join(HEADERS_DIR));
    let headers = child(&headers, SAVED_GAME).unwrap_or_else(|| headers.join(SAVED_GAME));
    let mut written = 0;
    for pkg in fs::read_dir(saved)?.flatten().filter(|e| e.path().is_dir()) {
        let Some(name) = pkg.file_name().to_str().map(str::to_owned) else { continue };
        let path = headers.join(format!("{name}.header"));
        if path.exists() {
            continue;
        }
        let Some(bytes) = legacy_header(tid, &name) else {
            tracing::warn!("no metadata header written for {name}: Xenia's file name field holds 41 bytes at most");
            continue;
        };
        saves::write_atomic(&path, &bytes)?;
        written += 1;
    }
    Ok(written)
}

// ---------------------------------------------------------------------------------------------
// The metadata header
// ---------------------------------------------------------------------------------------------

/// `XCONTENT_DATA_AGGREGATE` + the 4-byte licence mask that follows it: the header format older
/// Xenia wrote and `ContentPackageDirectory::ReadContentHeaderFile` still reads (any file smaller
/// than a full STFS header and at least this big). Xenia turns it into a full header itself the next
/// time the package is mounted. Big-endian throughout, laid out as in `xcontent.h`:
///
/// | offset | size  | field                                                        |
/// |--------|-------|--------------------------------------------------------------|
/// | 0x000  | 4     | `device_id`                                                  |
/// | 0x004  | 4     | `content_type` (1 = saved game)                              |
/// | 0x008  | 0x100 | `display_name`, UTF-16BE, zero padded                        |
/// | 0x108  | 42    | `file_name`, NUL padded: the package's folder name           |
/// | 0x132  | 2     | padding                                                      |
/// | 0x138  | 8     | `xuid` (left 0, as for a game's own request)                 |
/// | 0x140  | 4     | `title_id`                                                   |
/// | 0x148  | 4     | licence mask (0)                                             |
///
/// The game's own display name for the save is not known here, so the folder name stands in for it;
/// it is what Xenia hands back to the game as the package's file name that matters for opening it.
/// `None` when `package` does not fit the 42-byte file name field.
pub fn legacy_header(title_id: u32, package: &str) -> Option<Vec<u8>> {
    const FILE_NAME_AT: usize = 0x108;
    const FILE_NAME_LEN: usize = 42;
    const DISPLAY_AT: usize = 0x08;
    const DISPLAY_CHARS: usize = 128;
    const TITLE_ID_AT: usize = 0x140;
    const SIZE: usize = 0x148 + 4;

    let name = package.as_bytes();
    // Keep a NUL after it: `file_name()` stops at one, and a full field would run into the padding.
    if name.is_empty() || name.len() >= FILE_NAME_LEN {
        return None;
    }
    let mut b = vec![0u8; SIZE];
    b[0..4].copy_from_slice(&1u32.to_be_bytes());
    b[4..8].copy_from_slice(&1u32.to_be_bytes());
    // Leave the last code unit zero: some games read the display name until two NULs.
    for (i, unit) in package.encode_utf16().take(DISPLAY_CHARS - 1).enumerate() {
        let at = DISPLAY_AT + i * 2;
        b[at..at + 2].copy_from_slice(&unit.to_be_bytes());
    }
    b[FILE_NAME_AT..FILE_NAME_AT + name.len()].copy_from_slice(name);
    b[TITLE_ID_AT..TITLE_ID_AT + 4].copy_from_slice(&title_id.to_be_bytes());
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn write(root: &Path, rel: &str, data: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }

    fn names(zip: &[u8]) -> Vec<String> {
        let mut z = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
        (0..z.len()).map(|i| z.by_index(i).unwrap().name().to_string()).collect()
    }

    fn zip_of(entries: &[(&str, Option<&[u8]>)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = SimpleFileOptions::default();
        for (name, data) in entries {
            match data {
                Some(d) => {
                    w.start_file(*name, o).unwrap();
                    w.write_all(d).unwrap();
                }
                None => w.add_directory(*name, o).unwrap(),
            }
        }
        w.finish().unwrap().into_inner()
    }

    const XUID: &str = "E0001234567890AB";
    const TITLE: &str = "4D5307E6";

    fn content_with_save(root: &Path) -> PathBuf {
        let c = root.join("content");
        write(&c, &format!("{XUID}/{TITLE}/00000001/SAVE1/data.bin"), b"AAAA");
        write(&c, &format!("{XUID}/{TITLE}/00000001/SAVE1/__thumbnail.png"), b"PNG");
        c
    }

    // -------- finding --------

    #[test]
    fn content_lives_under_the_storage_root_unless_the_config_moves_it() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(content_root(t.path()), t.path().join("content"));

        let cfg = |body: &str| fs::write(t.path().join(CONFIG_NAME), body).unwrap();
        cfg("[Storage]\ncontent_root = \"\"\n");
        assert_eq!(content_root(t.path()), t.path().join("content"), "empty means the default");
        cfg("[Storage]\ncontent_root = \"saves/x360\"\n");
        assert_eq!(content_root(t.path()), t.path().join("saves/x360"), "relative to the storage root, as Xenia reads it");
        let abs = t.path().join("elsewhere");
        cfg(&format!("[Storage]\ncontent_root = {:?}\n", abs.to_string_lossy()));
        assert_eq!(content_root(t.path()), abs);
        cfg("this is [not toml");
        assert_eq!(content_root(t.path()), t.path().join("content"), "an unreadable config falls back");
    }

    #[test]
    fn the_title_id_is_read_in_xenias_spelling() {
        let id = |save: &str, title: &str| Identity { save_id: save.into(), title_id: title.into(), ..Default::default() };
        assert_eq!(title_id_of(&id("4d5307e6", "")).as_deref(), Some("4D5307E6"));
        assert_eq!(title_id_of(&id("", "4D5307E6")).as_deref(), Some("4D5307E6"));
        assert_eq!(title_id_of(&id("nonsense", "4D5307E6")).as_deref(), Some("4D5307E6"));
        assert_eq!(title_id_of(&id("", "")), None);
        assert_eq!(title_id_of(&id("4D5307E", "")), None, "seven digits is not a title id");
    }

    #[test]
    fn a_save_is_found_by_title_under_a_profile_and_other_titles_and_the_machine_are_not() {
        let t = tempfile::tempdir().unwrap();
        let c = content_with_save(t.path());
        write(&c, &format!("0000000000000000/{TITLE}/00000001/MACHINE/x"), b"m");
        write(&c, &format!("{XUID}/41560817/00000001/OTHER/x"), b"o");
        assert_eq!(find_save_dir(&c, TITLE).unwrap(), c.join(format!("{XUID}/{TITLE}/00000001")));
        assert_eq!(find_save_dir(&c, "4d5307e6").unwrap(), c.join(format!("{XUID}/{TITLE}/00000001")), "any case");
        assert!(find_save_dir(&c, "DEADBEEF").is_none());
        assert!(find_save_dir(&t.path().join("none"), TITLE).is_none());
    }

    #[test]
    fn dlc_and_title_updates_are_not_the_save() {
        let t = tempfile::tempdir().unwrap();
        let c = t.path().join("content");
        write(&c, &format!("{XUID}/{TITLE}/00000002/DLC/pack.bin"), b"dlc");
        write(&c, &format!("{XUID}/{TITLE}/000B0000/TU/update"), b"tu");
        assert!(find_save_dir(&c, TITLE).is_none());
    }

    #[test]
    fn with_two_profiles_the_one_written_last_wins() {
        let t = tempfile::tempdir().unwrap();
        let c = t.path().join("content");
        let (a, b) = ("E000000000000001", "E000000000000002");
        write(&c, &format!("{a}/{TITLE}/00000001/S/f"), b"old");
        write(&c, &format!("{b}/{TITLE}/00000001/S/f"), b"new");
        let older = SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options().write(true).open(c.join(format!("{a}/{TITLE}/00000001/S/f"))).unwrap().set_modified(older).unwrap();
        assert!(find_save_dir(&c, TITLE).unwrap().starts_with(c.join(b)));
    }

    // -------- collecting --------

    #[test]
    fn the_archive_is_rooted_at_00000001_like_argosys() {
        let t = tempfile::tempdir().unwrap();
        let c = content_with_save(t.path());
        let dir = find_save_dir(&c, TITLE).unwrap();
        let got = collect(&dir).unwrap().unwrap();
        assert_eq!(names(&got.data), ["00000001/SAVE1/", "00000001/SAVE1/__thumbnail.png", "00000001/SAVE1/data.bin"]);
        assert_eq!(got.files, 2);
        assert!(saves::is_zip(&got.data));
    }

    /// The hash RomM computes is over entry names and contents, so what Argosy would have produced
    /// for the same folder hashes the same however it was compressed or ordered.
    #[test]
    fn it_hashes_like_an_archive_argosy_built_for_the_same_folder() {
        let t = tempfile::tempdir().unwrap();
        let c = content_with_save(t.path());
        let ours = collect(&find_save_dir(&c, TITLE).unwrap()).unwrap().unwrap();
        let argosys = zip_of(&[
            ("00000001/SAVE1/", None),
            ("00000001/SAVE1/data.bin", Some(b"AAAA")),
            ("00000001/SAVE1/__thumbnail.png", Some(b"PNG")),
        ]);
        assert_eq!(ours.content_hash, unit::content_hash(&argosys));
    }

    #[test]
    fn the_hash_follows_the_contents_and_nothing_else() {
        let t = tempfile::tempdir().unwrap();
        let c = content_with_save(t.path());
        let dir = find_save_dir(&c, TITLE).unwrap();
        let first = collect(&dir).unwrap().unwrap().content_hash;
        assert_eq!(first, collect(&dir).unwrap().unwrap().content_hash, "stable");
        write(&dir, "SAVE1/data.bin", b"BBBB");
        assert_ne!(first, collect(&dir).unwrap().unwrap().content_hash);
    }

    #[test]
    fn a_folder_with_no_files_collects_nothing() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("00000001");
        fs::create_dir_all(dir.join("EMPTY")).unwrap();
        assert!(collect(&dir).unwrap().is_none());
    }

    // -------- restoring --------

    #[test]
    fn a_save_round_trips_to_the_same_hash() {
        let t = tempfile::tempdir().unwrap();
        let c = content_with_save(t.path());
        let sent = collect(&find_save_dir(&c, TITLE).unwrap()).unwrap().unwrap();

        // A second machine: it has a profile (Xenia made it) but no save yet.
        let u = tempfile::tempdir().unwrap();
        fs::create_dir_all(u.path().join(format!("content/{XUID}"))).unwrap();
        let r = restore(&u.path().join("content"), TITLE, &sent.data).unwrap();
        assert_eq!(r.files, 2);
        assert_eq!(r.dir, u.path().join(format!("content/{XUID}/{TITLE}/00000001")));
        let back = collect(&r.dir).unwrap().unwrap();
        assert_eq!(back.content_hash, sent.content_hash, "restored bytes must hash as the server's did, or every sync is a conflict");
        assert_eq!(fs::read(r.dir.join("SAVE1/data.bin")).unwrap(), b"AAAA");
    }

    #[test]
    fn restoring_replaces_the_whole_folder_and_leaves_dlc_and_updates_alone() {
        let t = tempfile::tempdir().unwrap();
        let c = content_with_save(t.path());
        write(&c, &format!("{XUID}/{TITLE}/00000001/STALE/old.bin"), b"gone");
        write(&c, &format!("{XUID}/{TITLE}/00000002/DLC/pack.bin"), b"dlc");
        write(&c, &format!("{XUID}/{TITLE}/000B0000/TU/update"), b"tu");
        let archive = zip_of(&[("00000001/SAVE1/", None), ("00000001/SAVE1/data.bin", Some(b"NEW"))]);
        let r = restore(&c, TITLE, &archive).unwrap();
        assert_eq!(fs::read(r.dir.join("SAVE1/data.bin")).unwrap(), b"NEW");
        assert!(!r.dir.join("STALE").exists(), "the archive is the whole save");
        assert_eq!(fs::read(c.join(format!("{XUID}/{TITLE}/00000002/DLC/pack.bin"))).unwrap(), b"dlc");
        assert_eq!(fs::read(c.join(format!("{XUID}/{TITLE}/000B0000/TU/update"))).unwrap(), b"tu");
        // No staging leftovers beside it.
        let left: Vec<_> = fs::read_dir(r.dir.parent().unwrap()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(left.iter().all(|n| !n.contains("romm-")), "{left:?}");
    }

    #[test]
    fn there_is_no_inventing_a_profile() {
        let t = tempfile::tempdir().unwrap();
        let c = t.path().join("content");
        fs::create_dir_all(c.join("0000000000000000")).unwrap(); // the machine's is not a profile
        let archive = zip_of(&[("00000001/S/f", Some(b"x"))]);
        let e = restore(&c, TITLE, &archive).unwrap_err().to_string();
        assert!(e.contains("no profile") && e.contains("sign in"), "{e}");
        assert!(!c.join(format!("0000000000000000/{TITLE}")).exists(), "nothing was written");
        assert!(restore(&t.path().join("missing"), TITLE, &archive).is_err());
    }

    #[test]
    fn a_game_never_played_here_goes_to_the_most_recently_used_profile() {
        let t = tempfile::tempdir().unwrap();
        let c = t.path().join("content");
        let (a, b) = ("E000000000000001", "E000000000000002");
        write(&c, &format!("{a}/41560817/00000001/S/f"), b"x");
        write(&c, &format!("{b}/41560817/00000001/S/f"), b"y");
        let older = SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options().write(true).open(c.join(format!("{a}/41560817/00000001/S/f"))).unwrap().set_modified(older).unwrap();
        let r = restore(&c, TITLE, &zip_of(&[("00000001/S/f", Some(b"z"))])).unwrap();
        assert_eq!(r.dir, c.join(format!("{b}/{TITLE}/00000001")));
    }

    #[test]
    fn archives_that_are_not_xenia_saves_are_refused_before_anything_is_touched() {
        let t = tempfile::tempdir().unwrap();
        let c = content_with_save(t.path());
        let dir = find_save_dir(&c, TITLE).unwrap();
        let before = collect(&dir).unwrap().unwrap().content_hash;
        for (what, bad) in [
            ("another root", zip_of(&[("SAVEDATA/x", Some(b"x"))])),
            ("a legacy bundle", zip_of(&[("romm-bundle.json", Some(b"{}")), ("00000001/S/f", Some(b"x"))])),
            ("a traversal", zip_of(&[("00000001/../../escape", Some(b"x"))])),
            ("an absolute path", zip_of(&[("/00000001/S/f", Some(b"x"))])),
            ("no files", zip_of(&[("00000001/S/", None)])),
            ("not a zip", b"just bytes".to_vec()),
        ] {
            assert!(restore(&c, TITLE, &bad).is_err(), "{what} must be refused");
            assert_eq!(collect(&dir).unwrap().unwrap().content_hash, before, "{what} must leave the save alone");
        }
        assert!(!c.join("escape").exists() && !t.path().join("escape").exists());
        assert!(restore(&c, "NOTHEX!!", &zip_of(&[("00000001/S/f", Some(b"x"))])).is_err());
    }

    // -------- headers --------

    fn header_path(c: &Path, pkg: &str) -> PathBuf {
        c.join(format!("{XUID}/{TITLE}/Headers/00000001/{pkg}.header"))
    }

    #[test]
    fn a_restored_package_without_a_header_gets_one_so_xenia_lists_it() {
        let t = tempfile::tempdir().unwrap();
        let c = t.path().join("content");
        fs::create_dir_all(c.join(XUID)).unwrap();
        let r = restore(&c, TITLE, &zip_of(&[("00000001/SAVE1/data.bin", Some(b"x")), ("00000001/SLOT2/data.bin", Some(b"y"))])).unwrap();
        assert_eq!(r.headers_written, 2);
        let h = fs::read(header_path(&c, "SAVE1")).unwrap();
        assert_eq!(h, legacy_header(0x4D5307E6, "SAVE1").unwrap());
        assert!(header_path(&c, "SLOT2").is_file());
    }

    #[test]
    fn an_existing_header_is_never_overwritten() {
        let t = tempfile::tempdir().unwrap();
        let c = content_with_save(t.path());
        let real = vec![7u8; 0x2000]; // what Xenia itself wrote
        write(&c, &format!("{XUID}/{TITLE}/Headers/00000001/SAVE1.header"), &real);
        let r = restore(&c, TITLE, &zip_of(&[("00000001/SAVE1/data.bin", Some(b"new"))])).unwrap();
        assert_eq!(r.headers_written, 0);
        assert_eq!(fs::read(header_path(&c, "SAVE1")).unwrap(), real);
    }

    #[test]
    fn the_headers_are_not_part_of_the_archive() {
        let t = tempfile::tempdir().unwrap();
        let c = content_with_save(t.path());
        write(&c, &format!("{XUID}/{TITLE}/Headers/00000001/SAVE1.header"), b"hdr");
        let got = collect(&find_save_dir(&c, TITLE).unwrap()).unwrap().unwrap();
        assert!(names(&got.data).iter().all(|n| n.starts_with("00000001/") && !n.contains("header")), "{:?}", names(&got.data));
    }

    /// Byte-level check against `XCONTENT_DATA_AGGREGATE` in xcontent.h (offsets compiled and checked
    /// against Xenia's own static_asserts: size 0x148, xuid 0x138, title_id 0x140).
    #[test]
    fn the_legacy_header_matches_xenias_struct_layout() {
        let h = legacy_header(0x4D5307E6, "SAVE1").unwrap();
        assert_eq!(h.len(), 0x14C, "at least sizeof(XCONTENT_DATA_AGGREGATE), far below a full STFS header (0x971A)");
        assert_eq!(&h[0..4], &[0, 0, 0, 1], "device id");
        assert_eq!(&h[4..8], &[0, 0, 0, 1], "content type: saved game");
        assert_eq!(&h[8..8 + 10], &[0, b'S', 0, b'A', 0, b'V', 0, b'E', 0, b'1'], "display name is UTF-16BE");
        assert!(h[8 + 10..0x108].iter().all(|b| *b == 0));
        assert_eq!(&h[0x108..0x10D], b"SAVE1");
        assert!(h[0x10D..0x138].iter().all(|b| *b == 0), "file name is NUL padded and the padding and 0x134 hole are zero");
        assert!(h[0x138..0x140].iter().all(|b| *b == 0), "xuid");
        assert_eq!(&h[0x140..0x144], &[0x4D, 0x53, 0x07, 0xE6], "title id, big-endian");
        assert!(h[0x144..].iter().all(|b| *b == 0), "licence mask");
    }

    #[test]
    fn a_package_name_that_does_not_fit_xenias_field_gets_no_header() {
        assert!(legacy_header(1, &"x".repeat(41)).is_some());
        assert!(legacy_header(1, &"x".repeat(42)).is_none());
        assert!(legacy_header(1, "").is_none());
        // The field counts bytes, not characters: 20 two-byte letters fit, 21 do not. The display name
        // is UTF-16BE, so each of them is one code unit there.
        let h = legacy_header(1, &"é".repeat(20)).unwrap();
        assert_eq!(&h[8..12], &[0x00, 0xE9, 0x00, 0xE9]);
        assert_eq!(&h[0x108..0x10A], "é".as_bytes());
        assert!(legacy_header(1, &"é".repeat(21)).is_none());
    }

    #[test]
    fn an_archive_with_too_many_entries_is_refused() {
        let a = zip_of(&[("00000001/a", Some(b"1")), ("00000001/b", Some(b"2")), ("00000001/c", Some(b"3"))]);
        let err = read_archive_within(&a, 2, 1 << 20).unwrap_err().to_string();
        assert!(err.contains("has 3 entries"), "{err}");
        assert_eq!(read_archive_within(&a, 3, 1 << 20).unwrap().len(), 3);
    }

    #[test]
    fn an_archive_that_unpacks_to_too_much_is_refused_across_its_files() {
        let big = vec![7u8; 600];
        let a = zip_of(&[("00000001/a", Some(&big)), ("00000001/b", Some(&big))]);
        // 600 + 600 > 1000, though neither is alone.
        let err = read_archive_within(&a, 100, 1000).unwrap_err().to_string();
        assert!(err.contains("unpacks to more than"), "{err}");
        assert_eq!(read_archive_within(&a, 100, 1200).unwrap().len(), 2, "exactly the budget is allowed");
    }

    #[test]
    fn read_archive_unpacks_unicode_and_nested_names() {
        let a = zip_of(&[("00000001/Sauvegarde é/niveau 1/état.dat", Some(b"ok"))]);
        let e = read_archive(&a).unwrap();
        assert_eq!(e.len(), 1);
        let mut s = String::new();
        Cursor::new(&e[0].data).read_to_string(&mut s).unwrap();
        assert_eq!(s, "ok");
        assert_eq!(e[0].rel, PathBuf::from("Sauvegarde é/niveau 1/état.dat"));
    }
}
