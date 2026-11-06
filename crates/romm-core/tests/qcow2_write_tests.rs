//! The qcow2 writer, judged by QEMU. Random writes of every awkward kind (in place, into space that
//! was never allocated, across cluster boundaries, into compressed clusters, into clusters and L2
//! tables a snapshot shares) are applied to a qcow2 image and, identically, to a plain raw "model"
//! of the disk. Afterwards `qemu-img check` must find the image consistent and `qemu-img compare`
//! must read it as the same disk as the model. Rollback and crash recovery are checked the same way.

mod common;

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use common::{qemu, Flaky};
use romm_core::xbox::qcow2::{journal_path, recover_journal, Qcow2};
use romm_core::xbox::{BlockRead, BlockWrite, E_OFFSET};

const FLAVOURS: [&str; 6] = ["hdd-v3.qcow2", "hdd-v2.qcow2", "hdd-compressed.qcow2", "hdd-4k.qcow2", "hdd-snapshot.qcow2", "small-fat16.qcow2"];

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xbox").join(name)
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 24) as u8).collect()
    }
}

/// One random write: a mix of where (the start of E: where fixtures have data, anywhere, straddling
/// a cluster boundary) and how much (a few bytes to a couple of clusters).
fn random_write(rng: &mut Rng, size: u64, cluster: u64) -> (u64, Vec<u8>) {
    let len = match rng.below(4) {
        0 => 1 + rng.below(64),
        1 => 512 + rng.below(4096),
        2 => 1 + rng.below(70_000),
        _ => 130_000 + rng.below(100_000),
    }
    .min(size / 2);
    let off = match rng.below(4) {
        0 => E_OFFSET.min(size / 2) + rng.below(2 << 20),
        1 => rng.below(size),
        2 => (rng.below(size / cluster).max(1) * cluster).saturating_sub(rng.below(100)),
        _ => E_OFFSET.min(size / 2) + 0x1000 + rng.below(1 << 20),
    };
    let off = off.min(size - len);
    (off, rng.bytes(len as usize))
}

fn apply_to_model(raw: &Path, off: u64, data: &[u8]) {
    let mut f = OpenOptions::new().write(true).open(raw).unwrap();
    f.seek(SeekFrom::Start(off)).unwrap();
    f.write_all(data).unwrap();
}

fn open_rw(path: &Path) -> Qcow2<File> {
    Qcow2::new(OpenOptions::new().read(true).write(true).open(path).unwrap()).unwrap()
}

fn file_bytes(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap()
}

#[test]
fn random_writes_leave_a_disk_qemu_reads_exactly_like_the_model() {
    if !qemu::available() {
        eprintln!("SKIPPED: qemu-img is not installed, so there is no independent judge of the image");
        return;
    }
    for flavour in FLAVOURS {
        for seed in [1u64, 2, 3] {
            let t = tempfile::tempdir().unwrap();
            let img = t.path().join("img.qcow2");
            std::fs::copy(fixture(flavour), &img).unwrap();
            let model = t.path().join("model.raw");
            qemu::to_raw(&img, &model);

            let mut disk = open_rw(&img);
            let (size, cs) = (disk.size(), disk.cluster_size());
            let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0xA24B_AED4_963E_E407));
            disk.begin(Some(&journal_path(&img))).unwrap();
            for _ in 0..60 {
                let (off, data) = random_write(&mut rng, size, cs);
                disk.write_at(off, &data).unwrap_or_else(|e| panic!("{flavour}/{seed}: write {off:#x}+{}: {e}", data.len()));
                apply_to_model(&model, off, &data);
                // What was just written reads back, through the crate's own reader, before commit.
                let mut back = vec![0u8; data.len()];
                disk.read_at(off, &mut back).unwrap();
                assert_eq!(back, data, "{flavour}/{seed}: read-back at {off:#x}");
            }
            disk.commit().unwrap();
            drop(disk);

            assert!(!journal_path(&img).exists(), "a committed write leaves no journal");
            qemu::check(&img).unwrap_or_else(|e| panic!("{flavour}/{seed}: {e}"));
            qemu::same_disk(&model, &img).unwrap_or_else(|e| panic!("{flavour}/{seed}: {e}"));
        }
    }
}

#[test]
fn rollback_restores_the_image_byte_for_byte() {
    for flavour in FLAVOURS {
        let t = tempfile::tempdir().unwrap();
        let img = t.path().join("img.qcow2");
        std::fs::copy(fixture(flavour), &img).unwrap();
        let before = file_bytes(&img);

        let mut disk = open_rw(&img);
        let (size, cs) = (disk.size(), disk.cluster_size());
        let mut rng = Rng(77);
        disk.begin(Some(&journal_path(&img))).unwrap();
        for _ in 0..40 {
            let (off, data) = random_write(&mut rng, size, cs);
            disk.write_at(off, &data).unwrap();
        }
        assert_ne!(file_bytes(&img), before, "{flavour}: the writes did something");
        disk.rollback().unwrap();

        assert_eq!(file_bytes(&img), before, "{flavour}: rollback is exact");
        assert!(!journal_path(&img).exists());
        // And the in-memory view was undone with it: a read agrees with the file, not the cache.
        let mut fresh = open_rw(&img);
        let mut a = vec![0u8; 4096];
        let mut b = vec![0u8; 4096];
        for off in [0, E_OFFSET.min(size / 2), E_OFFSET.min(size / 2) + 0x1000] {
            disk.read_at(off, &mut a).unwrap();
            fresh.read_at(off, &mut b).unwrap();
            assert_eq!(a, b, "{flavour}: stale cache after rollback at {off:#x}");
        }
    }
}

#[test]
fn a_crash_midway_is_undone_the_next_time_the_image_is_opened() {
    for flavour in FLAVOURS {
        let t = tempfile::tempdir().unwrap();
        let img = t.path().join("img.qcow2");
        std::fs::copy(fixture(flavour), &img).unwrap();
        let before = file_bytes(&img);

        let mut disk = open_rw(&img);
        let (size, cs) = (disk.size(), disk.cluster_size());
        let mut rng = Rng(5);
        disk.begin(Some(&journal_path(&img))).unwrap();
        for _ in 0..30 {
            let (off, data) = random_write(&mut rng, size, cs);
            disk.write_at(off, &data).unwrap();
        }
        // The process dies here: no commit, no rollback, nothing runs.
        std::mem::forget(disk);
        assert!(journal_path(&img).exists());
        assert_ne!(file_bytes(&img), before);

        assert!(recover_journal(&img).unwrap(), "{flavour}: there was something to undo");
        assert_eq!(file_bytes(&img), before, "{flavour}: recovery is exact");
        assert!(!journal_path(&img).exists());
        assert!(!recover_journal(&img).unwrap(), "nothing left to do the second time");
    }
}

#[test]
fn a_failure_at_any_point_rolls_the_image_back_exactly() {
    for flavour in ["hdd-v3.qcow2", "hdd-compressed.qcow2", "hdd-snapshot.qcow2", "hdd-4k.qcow2"] {
        let original = file_bytes(&fixture(flavour));
        // Stop after 1, 2, 3, ... file writes until the whole transaction fits.
        let mut stopped_early = 0;
        for budget in 1..400i64 {
            let t = tempfile::tempdir().unwrap();
            let img = t.path().join("img.qcow2");
            std::fs::write(&img, &original).unwrap();
            let left = Rc::new(Cell::new(budget));
            let f = Flaky { f: OpenOptions::new().read(true).write(true).open(&img).unwrap(), left: left.clone() };
            let mut disk = Qcow2::new(f).unwrap();
            let (size, cs) = (disk.size(), disk.cluster_size());
            let mut rng = Rng(11);
            disk.begin(Some(&journal_path(&img))).unwrap();
            let mut failed = false;
            for _ in 0..6 {
                let (off, data) = random_write(&mut rng, size, cs);
                if disk.write_at(off, &data).is_err() {
                    failed = true;
                    break;
                }
            }
            if !failed {
                disk.commit().unwrap();
                assert!(budget > 20, "{flavour}: 6 writes cannot need fewer than 20 file writes ({budget})");
                break;
            }
            stopped_early += 1;
            left.set(-1); // the disk works again for the undo
            disk.rollback().unwrap();
            assert_eq!(file_bytes(&img), original, "{flavour}: failure after {budget} writes was not undone exactly");
            assert!(!journal_path(&img).exists());
            if qemu::available() {
                qemu::check(&img).unwrap_or_else(|e| panic!("{flavour}/{budget}: {e}"));
            }
        }
        assert!(stopped_early > 20, "{flavour}: the failure injection actually interrupted the transaction ({stopped_early})");
    }
}

#[test]
fn an_image_that_is_in_use_or_not_ours_is_refused() {
    let t = tempfile::tempdir().unwrap();
    // QEMU's dirty flag (bit 0 of the incompatible features): xemu has it open, or it crashed.
    let img = t.path().join("dirty.qcow2");
    let mut bytes = file_bytes(&fixture("hdd-v3.qcow2"));
    bytes[79] |= 1;
    std::fs::write(&img, bytes).unwrap();
    let mut disk = open_rw(&img);
    let e = disk.begin(None).unwrap_err().to_string();
    assert!(e.contains("in use"), "{e}");

    // A journal that belongs to a different image is never applied to this one.
    let other = t.path().join("other.qcow2");
    std::fs::copy(fixture("hdd-4k.qcow2"), &other).unwrap();
    let mut d = open_rw(&other);
    d.begin(Some(&journal_path(&other))).unwrap();
    d.write_at(0x1000, b"x").unwrap();
    std::mem::forget(d);
    let mine = t.path().join("mine.qcow2");
    std::fs::copy(fixture("hdd-v3.qcow2"), &mine).unwrap();
    std::fs::rename(journal_path(&other), journal_path(&mine)).unwrap();
    let before = file_bytes(&mine);
    let e = recover_journal(&mine).unwrap_err().to_string();
    assert!(e.contains("left over from another image"), "{e}");
    assert_eq!(file_bytes(&mine), before, "the wrong journal must not touch the image");
}
