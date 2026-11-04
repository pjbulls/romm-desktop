//! Xbox (xemu) save export, checked against implementations this crate shares no code with:
//! the fixtures are built by libfatx and QEMU (`tests/fixtures/xbox/gen_fixtures.py`) and the
//! expected contents come from re-reading them with libfatx, not from the inputs.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::PathBuf;

use romm_core::saves::md5_bytes;
use romm_core::unit::content_hash;
use romm_core::xbox::qcow2::Qcow2;
use romm_core::xbox::{BlockRead, FileHdd, MemDisk, TitleDir, XboxError, XboxHdd, XboxResult, E_OFFSET};
use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xbox").join(name)
}

fn manifest() -> Value {
    serde_json::from_slice(&std::fs::read(fixture("manifest.json")).unwrap()).unwrap()
}

type Files = BTreeMap<String, (u64, String)>;

fn expected_files(title: &Value) -> Files {
    title["files"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), (v["size"].as_u64().unwrap(), v["md5"].as_str().unwrap().to_string()))).collect()
}

/// Entries of an exported zip: (files by path relative to the root, directory entries, root names).
fn read_zip(bytes: &[u8]) -> (Files, BTreeSet<String>, BTreeSet<String>) {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let (mut files, mut dirs, mut roots) = (Files::new(), BTreeSet::new(), BTreeSet::new());
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let name = f.name().to_string();
        roots.insert(name.split('/').next().unwrap().to_string());
        let rel = name.split_once('/').map(|x| x.1).unwrap_or("").to_string();
        if name.ends_with('/') {
            dirs.insert(rel.trim_end_matches('/').to_string());
        } else {
            let mut data = Vec::new();
            f.read_to_end(&mut data).unwrap();
            files.insert(rel, (data.len() as u64, md5_bytes(&data)));
        }
    }
    (files, dirs, roots)
}

/// RomM's content_hash computed from the manifest alone, the way RomM defines it.
fn manifest_hash(root: &str, files: &Files) -> String {
    let mut lines: Vec<String> = files.iter().map(|(p, (_, md5))| format!("{root}/{p}:{md5}")).collect();
    lines.sort();
    md5_bytes(lines.join("\n").as_bytes())
}

const E_VARIANTS: [&str; 4] = ["hdd-v3.qcow2", "hdd-v2.qcow2", "hdd-compressed.qcow2", "hdd-4k.qcow2"];

// ---- the reference fixtures ---------------------------------------------------------------

#[test]
fn every_qcow2_flavour_exports_exactly_what_libfatx_reads() {
    let m = manifest();
    let titles = m["e"]["titles"].as_object().unwrap();
    for variant in E_VARIANTS {
        let mut hdd = FileHdd::open(&fixture(variant)).unwrap_or_else(|e| panic!("{variant}: {e}"));
        let found: BTreeSet<(String, String)> = hdd.titles().unwrap().into_iter().map(|t| (t.save_id, t.dir_name)).collect();
        let want: BTreeSet<(String, String)> = titles.iter().map(|(id, t)| (id.clone(), t["dir_name"].as_str().unwrap().to_string())).collect();
        assert_eq!(found, want, "{variant}: title folders");

        for (id, t) in titles {
            let want_files = expected_files(t);
            let got = hdd.export_zip(id).unwrap_or_else(|e| panic!("{variant} {id}: {e}"));
            if want_files.is_empty() {
                assert!(got.is_none(), "{variant} {id}: a folder with no files exports nothing");
                continue;
            }
            let zip = got.unwrap_or_else(|| panic!("{variant} {id}: expected an archive"));
            let (files, dirs, roots) = read_zip(&zip);
            assert_eq!(roots, BTreeSet::from([id.clone()]), "{variant} {id}: single upper-case root");
            assert_eq!(files, want_files, "{variant} {id}: file names, sizes and md5s");
            let want_dirs: BTreeSet<String> = t["dirs"].as_array().unwrap().iter().map(|d| d.as_str().unwrap().to_string()).collect();
            assert_eq!(dirs, want_dirs, "{variant} {id}: directory entries");
            assert_eq!(content_hash(&zip), manifest_hash(id, &want_files), "{variant} {id}: RomM content_hash");
        }
    }
}

#[test]
fn deleted_entries_are_skipped_and_a_grown_directory_is_fully_read() {
    let m = manifest();
    let t = expected_files(&m["e"]["titles"]["41430001"]);
    assert!(t.contains_key("keep.bin") && !t.contains_key("gone1.bin") && !t.contains_key("gone2.bin"));
    assert_eq!(t.keys().filter(|k| k.starts_with("many/")).count(), 300, "two directory clusters");
    let mut hdd = FileHdd::open(&fixture("hdd-v3.qcow2")).unwrap();
    let (files, ..) = read_zip(&hdd.export_zip("41430001").unwrap().unwrap());
    assert!(files.contains_key("keep.bin"), "an entry between two deleted ones survives");
    assert!(!files.contains_key("gone1.bin") && !files.contains_key("gone2.bin"));
    assert_eq!(files.keys().filter(|k| k.starts_with("many/")).count(), 300);
}

#[test]
fn only_udata_title_folders_are_listed_and_tdata_is_not_exported() {
    let mut hdd = FileHdd::open(&fixture("hdd-v3.qcow2")).unwrap();
    let ids: Vec<String> = hdd.titles().unwrap().into_iter().map(|t| t.save_id).collect();
    assert!(!ids.iter().any(|i| i == "NOTATITLE") && ids.len() == 4, "{ids:?}");
    let (files, ..) = read_zip(&hdd.export_zip("4D530004").unwrap().unwrap());
    assert!(!files.keys().any(|k| k.contains("progress.dat")), "TDATA stays out of Argosy's unit");
}

#[test]
fn the_zip_has_argosys_shape() {
    let mut hdd = FileHdd::open(&fixture("hdd-v3.qcow2")).unwrap();
    let zip = hdd.export_zip("4d530004").unwrap().unwrap(); // a lower-case request still roots at the upper-case id
    let mut z = zip::ZipArchive::new(Cursor::new(&zip)).unwrap();
    let names: Vec<String> = (0..z.len()).map(|i| z.by_index(i).unwrap().name().to_string()).collect();
    assert!(!names.contains(&"4D530004/".to_string()), "Argosy's single-folder zip has no root entry: {names:?}");
    assert!(names.contains(&"4D530004/deep/er/est/".to_string()), "sub-folders get entries");
    assert!(names.iter().all(|n| n.starts_with("4D530004/")));
    assert_eq!(z.by_name("4D530004/TitleMeta.xbx").unwrap().compression(), zip::CompressionMethod::Deflated);
    // deterministic: exporting twice gives identical bytes
    assert_eq!(zip, hdd.export_zip("4D530004").unwrap().unwrap());
}

#[test]
fn unknown_and_malformed_titles() {
    let mut hdd = FileHdd::open(&fixture("hdd-v3.qcow2")).unwrap();
    assert!(hdd.export_zip("DEADBEEF").unwrap().is_none());
    for bad in ["", "4D53", "4D5300041", "ZZZZZZZZ", "../UDATA/"] {
        assert!(matches!(hdd.export_zip(bad), Err(XboxError::Fatx(_))), "{bad:?}");
    }
}

#[test]
fn a_fat16_partition_at_an_explicit_offset() {
    let m = manifest();
    let small = &m["small"];
    let mut hdd = FileHdd::open_region(&fixture("small-fat16.qcow2"), small["offset"].as_u64().unwrap(), small["size"].as_u64().unwrap()).unwrap();
    assert!(hdd.fatx().is_fat16());
    assert_eq!(hdd.titles().unwrap(), vec![TitleDir { save_id: "AABBCCDD".into(), dir_name: "aabbccdd".into() }]);
    let (files, ..) = read_zip(&hdd.export_zip("AABBCCDD").unwrap().unwrap());
    assert_eq!(files, expected_files(&small["titles"]["AABBCCDD"]));
}

// ---- qcow2 refusals -------------------------------------------------------------------------

fn patched(f: impl FnOnce(&mut Vec<u8>)) -> XboxResult<Qcow2<Cursor<Vec<u8>>>> {
    let mut b = std::fs::read(fixture("hdd-v3.qcow2")).unwrap();
    f(&mut b);
    Qcow2::new(Cursor::new(b))
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_be_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_be_bytes());
}

#[test]
fn qcow2_images_it_cannot_read_faithfully_are_refused() {
    assert!(matches!(patched(|b| b[0] = b'X'), Err(XboxError::Qcow2(_))), "bad magic");
    assert!(matches!(patched(|b| put32(b, 4, 4)), Err(XboxError::Unsupported(_))), "version 4");
    assert!(matches!(patched(|b| put64(b, 8, 512)), Err(XboxError::Unsupported(_))), "backing file");
    assert!(matches!(patched(|b| put32(b, 32, 1)), Err(XboxError::Unsupported(_))), "encryption");
    for (bit, what) in [(2, "external data"), (3, "zstd"), (4, "extended L2"), (9, "unknown bit")] {
        assert!(matches!(patched(|b| put64(b, 72, 1 << bit)), Err(XboxError::Unsupported(_))), "{what}");
    }
    assert!(matches!(patched(|b| put64(b, 72, 2)), Err(XboxError::Qcow2(_))), "corrupt flag");
    assert!(matches!(patched(|b| put32(b, 20, 30)), Err(XboxError::Qcow2(_))), "absurd cluster size");
    assert!(matches!(patched(|b| b.truncate(100)), Err(XboxError::Qcow2(_)) | Err(XboxError::Io(_))), "truncated");
    assert!(matches!(patched(|b| put64(b, 40, 1 << 40)), Err(XboxError::Qcow2(_))), "L1 table beyond the file");
}

#[test]
fn a_dirty_image_is_readable_but_says_so() {
    assert!(!patched(|_| {}).unwrap().is_dirty());
    assert!(patched(|b| put64(b, 72, 1)).unwrap().is_dirty());
}

#[test]
fn qcow2_reads_past_the_end_fail_and_unallocated_space_is_zero() {
    let mut q = patched(|_| {}).unwrap();
    let size = q.size();
    let mut buf = [0xAAu8; 4096];
    assert!(q.read_at(size - 4095, &mut buf).is_err());
    q.read_at(size - 4096, &mut buf).unwrap();
    assert_eq!(buf, [0u8; 4096], "the far end of a sparse disk was never written");
    let mut empty = [0u8; 0];
    q.read_at(size, &mut empty).unwrap();
}

// ---- synthetic FATX: what the reference tools can't produce -----------------------------------

/// A disk that only stores what is written, so tests can sit a partition at 2.7 GB.
struct SparseDisk {
    size: u64,
    chunks: HashMap<u64, Vec<u8>>, // keyed by write offset
}
impl SparseDisk {
    fn write(&mut self, off: u64, data: &[u8]) {
        self.chunks.insert(off, data.to_vec());
    }
}
impl BlockRead for SparseDisk {
    fn size(&self) -> u64 {
        self.size
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> XboxResult<()> {
        if off + buf.len() as u64 > self.size {
            return Err(XboxError::Fatx("past end".into()));
        }
        buf.fill(0);
        for (&o, d) in &self.chunks {
            let (s, e) = (o.max(off), (o + d.len() as u64).min(off + buf.len() as u64));
            if s < e {
                buf[(s - off) as usize..(e - off) as usize].copy_from_slice(&d[(s - o) as usize..(e - o) as usize]);
            }
        }
        Ok(())
    }
}

/// Builds a small FAT16 FATX volume (16 KiB clusters) in memory.
struct Vol {
    off: u64,
    img: SparseDisk,
    area: u64,
}
const BPC: u64 = 16384;
impl Vol {
    fn new(off: u64, size: u64, disk_size: u64) -> Vol {
        let entries = size / BPC + 1;
        assert!(entries < 0xFFF0, "FAT16 only");
        let fat_size = (entries * 2).div_ceil(4096) * 4096;
        let mut v = Vol { off, img: SparseDisk { size: disk_size, chunks: HashMap::new() }, area: off + 4096 + fat_size };
        let mut sb = b"FATX".to_vec();
        sb.extend_from_slice(&1u32.to_le_bytes()); // volume id
        sb.extend_from_slice(&32u32.to_le_bytes()); // sectors per cluster
        sb.extend_from_slice(&1u32.to_le_bytes()); // root cluster
        v.img.write(off, &sb);
        v.fat(0, 0xFFF8);
        v.fat(1, 0xFFFF);
        v
    }
    fn fat(&mut self, c: u32, val: u16) {
        self.img.write(self.off + 4096 + c as u64 * 2, &val.to_le_bytes());
    }
    fn cluster(&mut self, c: u32, data: &[u8]) {
        self.img.write(self.area + (c as u64 - 1) * BPC, data);
    }
    fn dir(&mut self, c: u32, entries: &[Vec<u8>]) {
        let mut buf = vec![0xFFu8; BPC as usize];
        for (i, e) in entries.iter().enumerate() {
            buf[i * 64..i * 64 + 64].copy_from_slice(e);
        }
        self.cluster(c, &buf);
    }
}
fn entry(name: &[u8], attr: u8, first: u32, size: u32) -> Vec<u8> {
    let mut e = vec![0xFFu8; 64];
    e[0] = name.len() as u8;
    e[1] = attr;
    e[2..2 + name.len()].copy_from_slice(name);
    e[44..48].copy_from_slice(&first.to_le_bytes());
    e[48..52].copy_from_slice(&size.to_le_bytes());
    e[52..64].fill(0);
    e
}
const DIR: u8 = 0x10;
fn open_small(v: Vol) -> XboxResult<XboxHdd<SparseDisk>> {
    // a retail-shaped disk would be 8 GB; for a tiny volume at its own offset use the region opener's logic
    XboxHdd::open_e(v.img)
}

/// A volume sitting where E: does, on a disk just big enough for it, so `open_e` has to fall back
/// from the retail length to "the rest of the disk".
fn e_volume(build: impl FnOnce(&mut Vol)) -> Vol {
    let size = 8 * 1024 * 1024;
    let mut v = Vol::new(E_OFFSET, size, E_OFFSET + size);
    build(&mut v);
    v
}

#[test]
fn open_e_falls_back_to_the_rest_of_the_disk_for_a_smaller_image() {
    let v = e_volume(|v| {
        v.dir(1, &[entry(b"UDATA", DIR, 2, 0)]);
        v.dir(2, &[entry(b"4D530004", DIR, 3, 0)]);
        v.dir(3, &[entry(b"save.dat", 0, 4, 5)]);
        v.cluster(4, b"hello");
        for c in 2..=4 {
            v.fat(c, 0xFFFF);
        }
    });
    let mut hdd = open_small(v).unwrap();
    assert!(hdd.fatx().is_fat16());
    let (files, ..) = read_zip(&hdd.export_zip("4D530004").unwrap().unwrap());
    assert_eq!(files["save.dat"].1, md5_bytes(b"hello"));
}

#[test]
fn a_42_byte_name_the_longest_the_field_holds() {
    let long = "a".repeat(38) + ".dat";
    assert_eq!(long.len(), 42);
    let v = e_volume(|v| {
        v.dir(1, &[entry(b"UDATA", DIR, 2, 0)]);
        v.dir(2, &[entry(b"00000001", DIR, 3, 0)]);
        v.dir(3, &[entry(long.as_bytes(), 0, 4, 3)]);
        v.cluster(4, b"abc");
        for c in 2..=4 {
            v.fat(c, 0xFFFF);
        }
    });
    let (files, ..) = read_zip(&open_small(v).unwrap().export_zip("00000001").unwrap().unwrap());
    assert!(files.contains_key(&long));
}

#[test]
fn a_zero_byte_ends_the_directory_so_stale_entries_after_it_are_ignored() {
    let v = e_volume(|v| {
        let mut end = vec![0u8; 64]; // 0x00 first byte: end of directory
        end[2..9].copy_from_slice(b"STALE.X");
        v.dir(1, &[entry(b"UDATA", DIR, 2, 0)]);
        v.dir(2, &[entry(b"00000002", DIR, 3, 0), end, entry(b"00000003", DIR, 3, 0)]);
        v.dir(3, &[entry(b"x", 0, 0, 0)]);
        for c in 2..=3 {
            v.fat(c, 0xFFFF);
        }
    });
    let mut hdd = open_small(v).unwrap();
    assert_eq!(hdd.titles().unwrap().len(), 1, "00000003 sits after the end marker");
}

fn corrupt(build: impl FnOnce(&mut Vol)) -> XboxError {
    let v = e_volume(|v| {
        v.dir(1, &[entry(b"UDATA", DIR, 2, 0)]);
        v.fat(2, 0xFFFF);
        build(v);
    });
    let mut hdd = open_small(v).unwrap();
    hdd.export_zip("00000001").expect_err("corruption must be an error, never a guess")
}

#[test]
fn corrupt_volumes_are_errors() {
    // a directory chain that loops
    assert!(matches!(corrupt(|v| { v.dir(2, &[entry(b"00000001", DIR, 3, 0)]); v.dir(3, &[entry(b"a", 0, 4, 1)]); v.fat(3, 4); v.fat(4, 3); v.cluster(4, b"x"); }), XboxError::Fatx(m) if m.contains("loops")));
    // a file whose chain ends before its size does
    assert!(matches!(corrupt(|v| { v.dir(2, &[entry(b"00000001", DIR, 3, 0)]); v.dir(3, &[entry(b"a", 0, 4, 40000)]); v.fat(3, 0xFFFF); v.fat(4, 0xFFFF); }), XboxError::Fatx(m) if m.contains("claims")));
    // a chain running into a free cluster
    assert!(matches!(corrupt(|v| { v.dir(2, &[entry(b"00000001", DIR, 3, 0)]); v.dir(3, &[entry(b"a", 0, 4, 20000)]); v.fat(3, 0xFFFF); v.fat(4, 9); }), XboxError::Fatx(m) if m.contains("free")));
    // a chain pointing outside the volume
    assert!(matches!(corrupt(|v| { v.dir(2, &[entry(b"00000001", DIR, 3, 0)]); v.dir(3, &[entry(b"a", 0, 4, 20000)]); v.fat(3, 0xFFFF); v.fat(4, 0xEEEE); }), XboxError::Fatx(m) if m.contains("outside")));
    // a name longer than the 42-byte field
    assert!(matches!(corrupt(|v| { let mut e = entry(b"a", 0, 4, 1); e[0] = 43; v.dir(2, &[entry(b"00000001", DIR, 3, 0)]); v.dir(3, &[e]); v.fat(3, 0xFFFF); }), XboxError::Fatx(m) if m.contains("43-byte")));
    // a name that tries to leave the folder
    assert!(matches!(corrupt(|v| { v.dir(2, &[entry(b"00000001", DIR, 3, 0)]); v.dir(3, &[entry(b"..", DIR, 4, 0)]); v.fat(3, 0xFFFF); v.fat(4, 0xFFFF); }), XboxError::Fatx(m) if m.contains("unsafe")));
    // a folder that contains itself
    assert!(matches!(corrupt(|v| { v.dir(2, &[entry(b"00000001", DIR, 3, 0)]); v.dir(3, &[entry(b"self", DIR, 3, 0)]); v.fat(3, 0xFFFF); }), XboxError::Fatx(m) if m.contains("loops")));
}

#[test]
fn not_a_fatx_volume() {
    let disk = MemDisk(vec![0u8; 1 << 20]);
    assert!(XboxHdd::open_e(disk).is_err());
    let mut f = File::open(fixture("hdd-v3.qcow2")).unwrap();
    let mut head = [0u8; 4];
    f.read_exact(&mut head).unwrap();
    assert_eq!(&head, b"QFI\xfb", "sanity: the fixtures really are qcow2");
}
