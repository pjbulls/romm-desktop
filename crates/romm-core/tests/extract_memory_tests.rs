//! Unpacking a multi-file ROM must use memory in proportion to a copy buffer, not to the size of the
//! game. A two-disc Xbox 360 game once took ~50 GB of RAM because the whole archive was read into
//! memory and then every entry was buffered again.
//!
//! This file holds ONE test on purpose: it measures the process's peak heap with a counting global
//! allocator, so nothing else may be allocating in parallel.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use romm_core::library::extract_zip;
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

struct Counting;
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            PEAK.fetch_max(CURRENT.fetch_add(l.size(), SeqCst) + l.size(), SeqCst);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        CURRENT.fetch_sub(l.size(), SeqCst);
    }
}

#[global_allocator]
static A: Counting = Counting;

const MIB: usize = 1 << 20;
const ENTRY_MIB: usize = 32;

/// One MiB with a pattern that differs between blocks, so a wrong or shuffled copy is noticed.
fn block(n: usize) -> Vec<u8> {
    (0..MIB).map(|i| ((i * 31 + n * 7) % 251) as u8).collect()
}

#[test]
fn unpacking_uses_memory_for_a_copy_buffer_not_for_the_game() {
    let t = tempfile::tempdir().unwrap();
    let zip_path = t.path().join("game.zip");
    {
        // Stored (not compressed), so the archive on disk is as big as what it holds.
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let mut zw = zip::ZipWriter::new(BufWriter::new(File::create(&zip_path).unwrap()));
        zw.add_directory("Disc 1/", opts).unwrap();
        for disc in ["Disc 1/data.iso", "Disc 2/data.iso"] {
            zw.start_file(disc, opts).unwrap();
            for n in 0..ENTRY_MIB {
                zw.write_all(&block(n)).unwrap();
            }
        }
        zw.start_file("Disc 2/empty.bin", opts).unwrap();
        zw.finish().unwrap().flush().unwrap();
    }
    let zip_len = std::fs::metadata(&zip_path).unwrap().len() as usize;
    assert!(zip_len > 2 * ENTRY_MIB * MIB, "the archive really is as big as its contents: {zip_len}");

    let dest = t.path().join("out");
    let before = CURRENT.load(SeqCst);
    PEAK.store(before, SeqCst);
    extract_zip(&zip_path, &dest).unwrap();
    let extra = PEAK.load(SeqCst) - before;

    // What came out is exactly what went in.
    for disc in ["Disc 1/data.iso", "Disc 2/data.iso"] {
        let mut f = File::open(dest.join(disc)).unwrap();
        for n in 0..ENTRY_MIB {
            let mut got = vec![0u8; MIB];
            f.read_exact(&mut got).unwrap();
            assert!(got == block(n), "{disc}: block {n} differs");
        }
        assert_eq!(f.read(&mut [0u8; 1]).unwrap(), 0, "{disc}: nothing after the last block");
    }
    assert_eq!(std::fs::metadata(dest.join("Disc 2/empty.bin")).unwrap().len(), 0);

    // The game is 64 MiB; a copy buffer is a few MiB at most. Anything near the size of the
    // archive or of one entry means it is being held in memory.
    eprintln!("unpacked {} MiB, peak extra heap {} KiB", zip_len / MIB, extra / 1024);
    let limit = 8 * MIB;
    assert!(extra < limit, "unpacking a {} MiB game peaked at {} MiB of extra heap (limit {} MiB)", zip_len / MIB, extra / MIB, limit / MIB);
}
