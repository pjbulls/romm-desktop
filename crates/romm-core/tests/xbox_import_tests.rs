//! Putting a save back into an xemu disk image. The properties that matter, checked against QEMU
//! (`qemu-img check`) and against a walk of every file on the partition before and after:
//!
//! * the title's folder ends up holding exactly the zip, and **nothing else on the disk changes**;
//! * the image stays structurally valid, whatever flavour it is (snapshots, compressed, FAT16 ...);
//! * anything refused (a hostile zip, a full disk) leaves the image byte-for-byte as it was;
//! * a failure at any point of a real import, or a crash, is undone exactly.

mod common;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use common::{qemu, Flaky};
use romm_core::saves::md5_bytes;
use romm_core::unit::content_hash;
use romm_core::xbox::qcow2::{journal_path, recover_journal, Qcow2};
use romm_core::xbox::{import_save, BlockRead, FileHdd, XboxHdd};
use serde_json::Value;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

const WRITABLE: [&str; 5] = ["hdd-v3.qcow2", "hdd-v2.qcow2", "hdd-compressed.qcow2", "hdd-4k.qcow2", "hdd-snapshot.qcow2"];

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xbox").join(name)
}

fn manifest() -> Value {
    serde_json::from_slice(&std::fs::read(fixture("manifest.json")).unwrap()).unwrap()
}

fn copy_fixture(dir: &Path, name: &str) -> PathBuf {
    let img = dir.join(name);
    std::fs::copy(fixture(name), &img).unwrap();
    img
}

/// A save zip in Argosy's shape: `<ROOT>/<path>`, with folders listed.
fn make_zip(root: &str, dirs: &[&str], files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let o = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    for d in dirs {
        z.add_directory(format!("{root}/{d}"), o).unwrap();
    }
    for (p, data) in files {
        z.start_file(format!("{root}/{p}"), o).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap().into_inner()
}

fn blob(seed: u8, n: usize) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

/// A save with the awkward shapes: nesting, an empty file, an exact cluster, several clusters.
fn sample_save(root: &str) -> Vec<u8> {
    make_zip(
        root,
        &["slot0", "slot0/deep", "empty-dir"],
        &[
            ("TitleMeta.xbx", blob(1, 700)),
            ("slot0/SaveMeta.xbx", blob(2, 16 * 1024)),
            ("slot0/deep/world.dat", blob(3, 150_000)),
            ("slot0/deep/zero.bin", vec![]),
            ("big.bin", blob(4, 1_000_000)),
        ],
    )
}

type Tree = BTreeMap<String, Option<(usize, String)>>;

/// Every folder and file on the E: partition: path -> (size, md5) for a file, `None` for a folder.
fn walk_all<D: BlockRead>(hdd: &mut XboxHdd<D>) -> Tree {
    fn go<D: BlockRead>(hdd: &mut XboxHdd<D>, first: u32, prefix: &str, out: &mut Tree) {
        for e in hdd.fatx().read_dir(first).unwrap() {
            let path = format!("{prefix}/{}", e.name);
            if e.is_dir() {
                out.insert(path.clone(), None);
                go(hdd, e.first_cluster, &path, out);
            } else {
                let data = hdd.fatx().read_file(&e).unwrap();
                out.insert(path, Some((data.len(), md5_bytes(&data))));
            }
        }
    }
    let root = hdd.fatx().root_cluster();
    let mut out = Tree::new();
    go(hdd, root, "", &mut out);
    out
}

fn tree_of(img: &Path) -> Tree {
    walk_all(&mut FileHdd::open(img).unwrap())
}

fn under(t: &Tree, prefix_lower: &str) -> Tree {
    t.iter().filter(|(p, _)| p.to_ascii_lowercase().starts_with(prefix_lower)).map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn outside(t: &Tree, prefix_lower: &str) -> Tree {
    t.iter().filter(|(p, _)| !p.to_ascii_lowercase().starts_with(prefix_lower)).map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn check_qemu(img: &Path) {
    if qemu::available() {
        qemu::check(img).unwrap_or_else(|e| panic!("{e}"));
    }
}

#[test]
fn a_save_replaces_only_its_own_folder_on_every_kind_of_image() {
    if !qemu::available() {
        eprintln!("SKIPPED the QEMU checks: qemu-img is not installed (the rest still runs)");
    }
    let m = manifest();
    let titles: Vec<String> = m["e"]["titles"].as_object().unwrap().keys().cloned().collect();
    for flavour in WRITABLE {
        for id in &titles {
            let t = tempfile::tempdir().unwrap();
            let img = copy_fixture(t.path(), flavour);
            let before = tree_of(&img);
            let zip = sample_save(&id.to_ascii_uppercase());

            let summary = import_save(&img, id, &zip).unwrap_or_else(|e| panic!("{flavour}/{id}: {e}"));
            assert_eq!((summary.files, summary.dirs), (5, 3), "{flavour}/{id}");

            let after = tree_of(&img);
            let folder = format!("/udata/{}", id.to_ascii_lowercase());
            assert_eq!(outside(&after, &folder), outside(&before, &folder), "{flavour}/{id}: something else on the disk changed");

            // The folder is exactly the zip, and exports to the zip's own content hash.
            let mut hdd = FileHdd::open(&img).unwrap();
            let exported = hdd.export_zip(id).unwrap().expect("the title has a save");
            assert_eq!(content_hash(&exported), content_hash(&zip), "{flavour}/{id}");
            let mine = under(&after, &folder);
            assert_eq!(mine.values().filter(|v| v.is_some()).count(), 5, "{flavour}/{id}");
            assert_eq!(mine.get(&format!("{}/slot0/deep/world.dat", before.keys().find(|k| k.to_ascii_lowercase() == folder).unwrap())), Some(&Some((150_000, md5_bytes(&blob(3, 150_000))))));

            assert!(!journal_path(&img).exists());
            check_qemu(&img);
        }
    }
}

#[test]
fn a_game_with_no_folder_yet_gets_one_named_as_the_console_names_them() {
    let t = tempfile::tempdir().unwrap();
    let img = copy_fixture(t.path(), "hdd-v3.qcow2");
    let before = tree_of(&img);
    assert!(!before.keys().any(|k| k.to_ascii_lowercase().contains("deadbeef")));

    import_save(&img, "DEADBEEF", &sample_save("DEADBEEF")).unwrap();
    let after = tree_of(&img);
    assert!(after.contains_key("/UDATA/deadbeef/big.bin"), "lower-case folder, as the console writes them: {:?}", after.keys().filter(|k| k.contains("eadbeef")).collect::<Vec<_>>());
    assert_eq!(outside(&after, "/udata/deadbeef"), before);
    check_qemu(&img);
}

#[test]
fn restoring_a_smaller_save_removes_what_the_server_save_does_not_have() {
    let t = tempfile::tempdir().unwrap();
    let img = copy_fixture(t.path(), "hdd-v3.qcow2");
    import_save(&img, "4D530004", &sample_save("4D530004")).unwrap();
    let small = make_zip("4D530004", &[], &[("only.bin", blob(9, 100))]);
    let before_small = tree_of(&img);

    import_save(&img, "4D530004", &small).unwrap();
    let after = tree_of(&img);
    let mine = under(&after, "/udata/4d530004");
    assert_eq!(mine.keys().cloned().collect::<Vec<_>>(), ["/UDATA/4d530004", "/UDATA/4d530004/only.bin"].map(String::from).to_vec().into_iter().filter(|k| mine.contains_key(k)).collect::<Vec<_>>());
    assert_eq!(mine.len(), 2, "{mine:?}");
    assert_eq!(outside(&after, "/udata/4d530004"), outside(&before_small, "/udata/4d530004"));

    // The space came back: putting the big one in again fits, and the disk is still sound.
    import_save(&img, "4D530004", &sample_save("4D530004")).unwrap();
    check_qemu(&img);
}

#[test]
fn a_folder_with_hundreds_of_files_makes_its_directory_grow() {
    let t = tempfile::tempdir().unwrap();
    let img = copy_fixture(t.path(), "hdd-v3.qcow2");
    let files: Vec<(String, Vec<u8>)> = (0..700).map(|i| (format!("f{i:04}.dat"), blob(i as u8, 10 + i))).collect();
    let refs: Vec<(&str, Vec<u8>)> = files.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
    let before = tree_of(&img);

    let s = import_save(&img, "41430001", &make_zip("41430001", &[], &refs)).unwrap();
    assert_eq!(s.files, 700);
    let after = tree_of(&img);
    assert_eq!(under(&after, "/udata/41430001/").len(), 700);
    assert_eq!(outside(&after, "/udata/41430001"), outside(&before, "/udata/41430001"));
    check_qemu(&img);
}

#[test]
fn a_zip_the_console_could_not_hold_is_refused_before_anything_is_written() {
    let t = tempfile::tempdir().unwrap();
    let img = copy_fixture(t.path(), "hdd-v3.qcow2");
    let original = std::fs::read(&img).unwrap();
    let ok = ("a.bin", blob(1, 10));
    let long = "x".repeat(43);

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("not a zip at all", b"this is not a zip".to_vec()),
        ("another game's folder", make_zip("11111111", &[], &[ok.clone()])),
        ("climbing out of the folder", make_zip("4D530004", &[], &[("../../TDATA/x", blob(1, 1))])),
        ("a backslash path", make_zip("4D530004", &[], &[("a\\b.bin", blob(1, 1))])),
        ("a name of 43 characters", make_zip("4D530004", &[], &[(long.as_str(), blob(1, 1))])),
        ("a forbidden character", make_zip("4D530004", &[], &[("a:b", blob(1, 1))])),
        ("a non-ASCII name", make_zip("4D530004", &[], &[("sp\u{e4}ce", blob(1, 1))])),
        ("two names differing only in case", make_zip("4D530004", &[], &[("A.bin", blob(1, 1)), ("a.BIN", blob(2, 1))])),
        ("no files at all", make_zip("4D530004", &["only-a-dir"], &[])),
        ("a file inside a file", make_zip("4D530004", &[], &[("f", blob(1, 1)), ("f/g", blob(2, 1))])),
    ];
    for (what, zip) in cases {
        let e = import_save(&img, "4D530004", &zip).unwrap_err().to_string();
        assert!(!e.is_empty(), "{what}");
        assert_eq!(std::fs::read(&img).unwrap(), original, "{what}: the image must be untouched ({e})");
        assert!(!journal_path(&img).exists(), "{what}");
    }
    // A bad title id is refused too.
    assert!(import_save(&img, "../..", &sample_save("4D530004")).is_err());
    assert_eq!(std::fs::read(&img).unwrap(), original);
}

#[test]
fn a_save_too_big_for_the_disk_is_refused_without_writing_a_byte() {
    let m = manifest();
    let (off, size) = (m["small"]["offset"].as_u64().unwrap(), m["small"]["size"].as_u64().unwrap());
    let t = tempfile::tempdir().unwrap();
    let img = copy_fixture(t.path(), "small-fat16.qcow2");
    let original = std::fs::read(&img).unwrap();
    let file = OpenOptions::new().read(true).write(true).open(&img).unwrap();
    let mut hdd = XboxHdd::with_region(Qcow2::new(file).unwrap(), off, size).unwrap();
    hdd.device_mut().begin(Some(&journal_path(&img))).unwrap();
    let zip = make_zip("AABBCCDD", &[], &[("huge.bin", vec![7u8; 40_000_000])]);
    let e = hdd.import_zip("AABBCCDD", &zip).unwrap_err().to_string();
    assert!(e.contains("not enough free space"), "{e}");
    hdd.device_mut().rollback().unwrap();
    assert_eq!(std::fs::read(&img).unwrap(), original);
}

#[test]
fn it_works_on_a_fat16_volume_too() {
    let m = manifest();
    let (off, size) = (m["small"]["offset"].as_u64().unwrap(), m["small"]["size"].as_u64().unwrap());
    let t = tempfile::tempdir().unwrap();
    let img = copy_fixture(t.path(), "small-fat16.qcow2");
    let file = OpenOptions::new().read(true).write(true).open(&img).unwrap();
    let mut hdd = XboxHdd::with_region(Qcow2::new(file).unwrap(), off, size).unwrap();
    assert!(hdd.fatx().is_fat16());
    let before = walk_all(&mut hdd);
    hdd.device_mut().begin(Some(&journal_path(&img))).unwrap();
    let zip = sample_save("AABBCCDD");
    hdd.import_zip("AABBCCDD", &zip).unwrap();
    hdd.device_mut().commit().unwrap();
    drop(hdd);

    let mut again = XboxHdd::with_region(Qcow2::new(std::fs::File::open(&img).unwrap()).unwrap(), off, size).unwrap();
    let after = walk_all(&mut again);
    assert_eq!(outside(&after, "/udata/aabbccdd"), outside(&before, "/udata/aabbccdd"));
    assert_eq!(under(&after, "/udata/aabbccdd/").values().filter(|v| v.is_some()).count(), 5);
    assert_eq!(content_hash(&again.export_zip("AABBCCDD").unwrap().unwrap()), content_hash(&zip));
    check_qemu(&img);
}

#[test]
fn a_failure_at_any_point_of_a_real_import_leaves_the_image_exactly_as_it_was() {
    for flavour in ["hdd-v3.qcow2", "hdd-snapshot.qcow2", "hdd-compressed.qcow2"] {
        let original = std::fs::read(fixture(flavour)).unwrap();
        let zip = sample_save("4D530004");
        let (mut stopped, mut finished) = (0, false);
        // Cut the transaction off after 1, 4, 7, ... file writes until a whole import fits.
        for budget in (1..2000i64).step_by(3) {
            let t = tempfile::tempdir().unwrap();
            let img = t.path().join("img.qcow2");
            std::fs::write(&img, &original).unwrap();
            let left = Rc::new(Cell::new(-1i64));
            let f = Flaky { f: OpenOptions::new().read(true).write(true).open(&img).unwrap(), left: left.clone() };
            let mut hdd = XboxHdd::open_e(Qcow2::new(f).unwrap()).unwrap();
            hdd.device_mut().begin(Some(&journal_path(&img))).unwrap();
            left.set(budget);
            match hdd.import_zip("4D530004", &zip) {
                Ok(_) => {
                    left.set(-1);
                    hdd.device_mut().commit().unwrap();
                    finished = true;
                    check_qemu(&img);
                    break;
                }
                Err(_) => {
                    stopped += 1;
                    left.set(-1);
                    hdd.device_mut().rollback().unwrap();
                    assert_eq!(std::fs::read(&img).unwrap(), original, "{flavour}: failing after {budget} writes was not undone exactly");
                    assert!(!journal_path(&img).exists());
                }
            }
        }
        assert!(finished && stopped > 10, "{flavour}: finished={finished}, interrupted={stopped}");
    }
}

#[test]
fn a_crash_during_an_import_is_undone_when_the_image_is_next_opened() {
    let t = tempfile::tempdir().unwrap();
    let img = copy_fixture(t.path(), "hdd-snapshot.qcow2");
    let original = std::fs::read(&img).unwrap();
    let before = tree_of(&img);

    // The import runs to the end and the process dies before it can commit.
    let mut hdd = XboxHdd::open_e(Qcow2::new(OpenOptions::new().read(true).write(true).open(&img).unwrap()).unwrap()).unwrap();
    hdd.device_mut().begin(Some(&journal_path(&img))).unwrap();
    hdd.import_zip("4D530004", &sample_save("4D530004")).unwrap();
    drop(hdd);
    assert_ne!(std::fs::read(&img).unwrap(), original);

    // Reading the image (what the next sync does) puts it right first: the half-made save is never seen.
    assert_eq!(tree_of(&img), before);
    assert_eq!(std::fs::read(&img).unwrap(), original);
    assert!(!journal_path(&img).exists());
    assert!(!recover_journal(&img).unwrap());
}

#[test]
fn an_image_xemu_has_open_is_not_written() {
    let t = tempfile::tempdir().unwrap();
    let img = copy_fixture(t.path(), "hdd-v3.qcow2");
    let mut bytes = std::fs::read(&img).unwrap();
    bytes[79] |= 1; // QEMU's "in use" flag
    std::fs::write(&img, &bytes).unwrap();
    let e = import_save(&img, "4D530004", &sample_save("4D530004")).unwrap_err().to_string();
    assert!(e.contains("in use"), "{e}");
    assert_eq!(std::fs::read(&img).unwrap(), bytes);
    assert!(!journal_path(&img).exists());
}

#[test]
fn consecutive_imports_keep_a_snapshot_image_valid() {
    let t = tempfile::tempdir().unwrap();
    let img = copy_fixture(t.path(), "hdd-snapshot.qcow2");
    let titles: Vec<String> = manifest()["e"]["titles"].as_object().unwrap().keys().cloned().collect();
    for round in 0..3u8 {
        for id in &titles {
            let zip = make_zip(&id.to_ascii_uppercase(), &["d"], &[("a.bin", blob(round, 20_000 * (round as usize + 1))), ("d/b.bin", blob(round + 50, 70_000))]);
            import_save(&img, id, &zip).unwrap();
            assert_eq!(content_hash(&FileHdd::open(&img).unwrap().export_zip(id).unwrap().unwrap()), content_hash(&zip));
        }
        check_qemu(&img);
    }
    // The snapshot is still there, and still intact.
    if qemu::available() {
        let out = std::process::Command::new("qemu-img").args(["snapshot", "-l"]).arg(&img).output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("before-save"));
    }
}

fn free_clusters(img: &Path) -> u64 {
    let mut hdd = FileHdd::open(img).unwrap();
    hdd.fatx().free_clusters().unwrap()
}

#[test]
fn replacing_a_save_gives_back_every_cluster_the_old_one_used() {
    let t = tempfile::tempdir().unwrap();
    // One disk gets the big save and then the small one; the other gets only the small one.
    let a = copy_fixture(t.path(), "hdd-v3.qcow2");
    let b = t.path().join("b.qcow2");
    std::fs::copy(fixture("hdd-v3.qcow2"), &b).unwrap();
    let small = make_zip("4D530004", &["x"], &[("only.bin", blob(9, 100)), ("x/y.bin", blob(8, 40_000))]);

    import_save(&a, "4D530004", &sample_save("4D530004")).unwrap();
    import_save(&a, "4D530004", &small).unwrap();
    import_save(&b, "4D530004", &small).unwrap();
    assert_eq!(free_clusters(&a), free_clusters(&b), "clusters of the replaced folder were not freed");
    assert_eq!(content_hash(&FileHdd::open(&a).unwrap().export_zip("4D530004").unwrap().unwrap()), content_hash(&small));
}

// ---- directory bookkeeping, on a volume small enough to lay out by hand -----------------------

const BPC: usize = 16384;

/// A FAT16 FATX volume of 8 MiB at offset 0 (16 KiB clusters): FAT at 0x1000 (one page), file area
/// at 0x2000, so cluster `c` lives at `0x2000 + (c - 1) * 16 KiB`.
struct Mini(Vec<u8>);

impl Mini {
    const SIZE: u64 = 8 * 1024 * 1024;
    fn new() -> Mini {
        let mut m = Mini(vec![0u8; Self::SIZE as usize]);
        m.0[..4].copy_from_slice(b"FATX");
        m.0[4..8].copy_from_slice(&1u32.to_le_bytes());
        m.0[8..12].copy_from_slice(&32u32.to_le_bytes());
        m.0[12..16].copy_from_slice(&1u32.to_le_bytes());
        m.fat(0, 0xFFF8);
        m
    }
    fn fat(&mut self, c: u32, v: u16) {
        let at = 0x1000 + c as usize * 2;
        self.0[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn at(c: u32) -> usize {
        0x2000 + (c as usize - 1) * BPC
    }
    /// A directory cluster: these entries, then end markers (0xFF) for the rest.
    fn dir(&mut self, c: u32, entries: &[Vec<u8>]) {
        let base = Self::at(c);
        self.0[base..base + BPC].fill(0xFF);
        for (i, e) in entries.iter().enumerate() {
            self.0[base + i * 64..base + i * 64 + 64].copy_from_slice(e);
        }
    }
    /// A raw slot, wherever it is (used to plant stale entries after an end marker).
    fn slot(&mut self, c: u32, i: usize, e: &[u8]) {
        let at = Self::at(c) + i * 64;
        self.0[at..at + 64].copy_from_slice(e);
    }
}

fn dir_entry(name: &str, first: u32) -> Vec<u8> {
    let mut e = vec![0xFFu8; 64];
    e[0] = name.len() as u8;
    e[1] = 0x10;
    e[2..2 + name.len()].copy_from_slice(name.as_bytes());
    e[44..48].copy_from_slice(&first.to_le_bytes());
    e[48..52].copy_from_slice(&0u32.to_le_bytes());
    e[52..64].fill(0);
    e
}

fn titles_of(hdd: &mut XboxHdd<romm_core::xbox::MemDisk>) -> Vec<String> {
    let mut t: Vec<String> = hdd.titles().unwrap().into_iter().map(|t| t.save_id).collect();
    t.sort();
    t
}

fn tiny_zip(id: &str) -> Vec<u8> {
    make_zip(id, &[], &[("a.bin", blob(1, 50))])
}

#[test]
fn a_new_entry_does_not_bring_a_stale_entry_after_the_end_marker_back_to_life() {
    let mut m = Mini::new();
    m.fat(1, 0xFFFF);
    m.fat(2, 0xFFFF);
    m.fat(3, 0xFFFF);
    m.dir(1, &[dir_entry("UDATA", 2)]);
    m.dir(2, &[dir_entry("00000002", 3)]);
    // A leftover from a deleted game, planted just after the end marker (slot 1) at slot 2.
    m.slot(2, 2, &dir_entry("deadbeef", 3));
    m.dir(3, &[]);
    let mut hdd = XboxHdd::with_region(romm_core::xbox::MemDisk(m.0), 0, Mini::SIZE).unwrap();
    assert_eq!(titles_of(&mut hdd), ["00000002"], "the stale entry is hidden to begin with");

    hdd.import_zip("00000004", &tiny_zip("00000004")).unwrap();
    assert_eq!(titles_of(&mut hdd), ["00000002", "00000004"], "the stale 'deadbeef' must stay dead");
}

#[test]
fn an_end_marker_in_the_last_slot_hides_the_stale_cluster_that_follows() {
    let mut m = Mini::new();
    for c in 1..=3 {
        m.fat(c, 0xFFFF);
    }
    m.fat(2, 4); // UDATA continues into cluster 4
    m.fat(4, 0xFFFF);
    m.fat(5, 0xFFFF);
    m.dir(1, &[dir_entry("UDATA", 2)]);
    // 255 games, the end marker in slot 255, and a cluster after it holding a stale entry.
    let games: Vec<Vec<u8>> = (0..255).map(|i| dir_entry(&format!("{:08x}", 0x100 + i), 3)).collect();
    m.dir(2, &games);
    m.dir(3, &[]);
    m.dir(4, &[dir_entry("deadbeef", 3)]);
    let mut hdd = XboxHdd::with_region(romm_core::xbox::MemDisk(m.0), 0, Mini::SIZE).unwrap();
    assert_eq!(titles_of(&mut hdd).len(), 255);

    hdd.import_zip("00000004", &tiny_zip("00000004")).unwrap();
    let t = titles_of(&mut hdd);
    assert_eq!(t.len(), 256, "255 + the new one");
    assert!(t.contains(&"00000004".to_string()) && !t.contains(&"DEADBEEF".to_string()), "the stale cluster must stay hidden");
}

#[test]
fn deleted_slots_are_reused_before_the_directory_grows() {
    let mut m = Mini::new();
    for c in 1..=3 {
        m.fat(c, 0xFFFF);
    }
    m.dir(1, &[dir_entry("UDATA", 2)]);
    let mut gone = dir_entry("00000009", 3);
    gone[0] = 0xE5;
    m.dir(2, &[dir_entry("00000002", 3), gone, dir_entry("00000003", 3)]);
    m.dir(3, &[]);
    let mut hdd = XboxHdd::with_region(romm_core::xbox::MemDisk(m.0), 0, Mini::SIZE).unwrap();
    hdd.import_zip("00000004", &tiny_zip("00000004")).unwrap();
    assert_eq!(titles_of(&mut hdd), ["00000002", "00000003", "00000004"]);
    // It took the deleted slot (index 1), so the live ones keep their places around it.
    let order: Vec<String> = hdd.titles().unwrap().into_iter().map(|t| t.save_id).collect();
    assert_eq!(order, ["00000002", "00000004", "00000003"]);
}
