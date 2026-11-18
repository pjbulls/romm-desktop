//! A qcow2 reader, and a careful writer, for the guest disk of an xemu HDD image.
//!
//! The format is QEMU's ([specification](https://www.qemu.org/docs/master/interop/qcow2.html)). This
//! reader maps guest offsets through the L1/L2 tables and understands the three ways a cluster can
//! be stored: allocated (plain), zero/unallocated, and zlib-compressed. Anything it cannot read
//! faithfully (backing files, encryption, external data files, zstd compression, extended L2
//! entries, or an image QEMU has flagged corrupt) is refused up front instead of returning guesses.
//!
//! Writing is opt-in (`begin`, then `commit` or `rollback`) and journaled: before any byte of the
//! file is overwritten its old contents are recorded (and synced, when a journal file is given), so
//! a failure rolls the image back exactly, and a crash is undone by [`recover_journal`] the next
//! time the image is opened. The writer handles what an xemu image needs: data clusters in place,
//! new clusters appended to the file (the stock image is sparse), and copy-on-write for clusters
//! and L2 tables an internal snapshot shares. Refcounts are only ever raised or lowered by one, and
//! never to zero, so the worst a mistake can do is leak space, never free a cluster that is in use.
//!
//! Neither reading nor writing takes a lock: only open an image while xemu is not running it.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use flate2::read::DeflateDecoder;

use super::{BlockRead, BlockWrite, XboxError, XboxResult};

const MAGIC: [u8; 4] = *b"QFI\xfb";
/// Bits 9..=55 of an L1/L2 entry: the host offset.
const OFFSET_MASK: u64 = 0x00ff_ffff_ffff_fe00;
const L2_COMPRESSED: u64 = 1 << 62;
/// v3 only: the cluster reads as zeros whatever its host offset says.
const L2_ZERO: u64 = 1;
/// Set on an L1/L2 entry whose cluster has exactly one reference, so it may be written in place.
const OFLAG_COPIED: u64 = 1 << 63;
/// Refcount-table entries hold a cluster offset in bits 9..=63.
const RT_OFFSET_MASK: u64 = !0x1ff;
const JOURNAL_MAGIC: &[u8; 8] = b"RMUNDO01";
/// What the journal remembers of the image's header, so a stale journal is never applied to another
/// image. Nothing here ever rewrites the header, so it is the same before and after a crash.
const JOURNAL_HEADER_ID: usize = 72;

const INCOMPAT_DIRTY: u64 = 1 << 0;
const INCOMPAT_CORRUPT: u64 = 1 << 1;
const INCOMPAT_EXTERNAL_DATA: u64 = 1 << 2;
const INCOMPAT_COMPRESSION_TYPE: u64 = 1 << 3;
const INCOMPAT_EXTENDED_L2: u64 = 1 << 4;

const MAX_CACHED_L2: usize = 256;
const MAX_CACHED_CLUSTERS: usize = 16;

fn err(msg: impl Into<String>) -> XboxError {
    XboxError::Qcow2(msg.into())
}
fn unsupported(msg: impl Into<String>) -> XboxError {
    XboxError::Unsupported(msg.into())
}
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}
fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b[..8].try_into().unwrap())
}

pub struct Qcow2<R> {
    r: R,
    file_len: u64,
    version: u32,
    cluster_bits: u32,
    size: u64,
    l1: Vec<u64>,
    l2: HashMap<u64, Vec<u64>>,
    decompressed: HashMap<u64, Vec<u8>>,
    dirty: bool,
    l1_off: u64,
    /// Where the refcount table is, and its entries once a transaction has loaded them.
    rt_off: u64,
    rt_entries: u64,
    rt: Vec<u64>,
    /// Why this image must not be written, if it must not (checked by `begin`).
    cannot_write: Option<String>,
    tx: Option<Tx>,
}

/// The open write transaction: what to put back to undo it.
struct Tx {
    orig_len: u64,
    undo: Vec<(u64, Vec<u8>)>,
    journal: Option<(File, PathBuf)>,
}

/// What a writable backing store needs beyond `Read + Write + Seek`.
pub trait BackingFile: Read + Write + Seek {
    fn set_len(&mut self, len: u64) -> std::io::Result<()>;
    fn sync(&mut self) -> std::io::Result<()>;
}

impl BackingFile for File {
    fn set_len(&mut self, len: u64) -> std::io::Result<()> {
        File::set_len(self, len)
    }
    fn sync(&mut self) -> std::io::Result<()> {
        self.sync_all()
    }
}

impl BackingFile for Cursor<Vec<u8>> {
    fn set_len(&mut self, len: u64) -> std::io::Result<()> {
        self.get_mut().resize(len as usize, 0);
        Ok(())
    }
    fn sync(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<R: Read + Seek> Qcow2<R> {
    pub fn new(mut r: R) -> XboxResult<Self> {
        let file_len = r.seek(SeekFrom::End(0))?;
        if file_len < 72 {
            return Err(err("file too small to be a qcow2 image"));
        }
        r.seek(SeekFrom::Start(0))?;
        let mut h = [0u8; 72];
        r.read_exact(&mut h)?;
        if h[..4] != MAGIC {
            return Err(err("not a qcow2 image (bad magic)"));
        }
        let version = be32(&h[4..]);
        if version != 2 && version != 3 {
            return Err(unsupported(format!("qcow2 version {version}")));
        }
        if be64(&h[8..]) != 0 {
            return Err(unsupported("image has a backing file"));
        }
        let cluster_bits = be32(&h[20..]);
        if !(9..=21).contains(&cluster_bits) {
            return Err(err(format!("cluster size 2^{cluster_bits} is outside what QEMU allows")));
        }
        let size = be64(&h[24..]);
        if be32(&h[32..]) != 0 {
            return Err(unsupported("image is encrypted"));
        }
        let l1_size = be32(&h[36..]) as u64;
        let l1_off = be64(&h[40..]);
        let rt_off = be64(&h[48..]);
        let rt_clusters = be32(&h[56..]) as u64;

        let mut dirty = false;
        let mut refcount_order = 4u32;
        let mut autoclear = 0u64;
        if version == 3 {
            let mut ext = [0u8; 32];
            r.read_exact(&mut ext)?;
            let incompat = be64(&ext[0..]);
            let known = INCOMPAT_DIRTY | INCOMPAT_CORRUPT;
            if incompat & INCOMPAT_CORRUPT != 0 {
                return Err(err("image is marked corrupt by QEMU"));
            }
            if incompat & INCOMPAT_EXTERNAL_DATA != 0 {
                return Err(unsupported("external data file"));
            }
            if incompat & INCOMPAT_COMPRESSION_TYPE != 0 {
                return Err(unsupported("non-zlib compression"));
            }
            if incompat & INCOMPAT_EXTENDED_L2 != 0 {
                return Err(unsupported("extended L2 entries"));
            }
            if incompat & !(known | INCOMPAT_EXTERNAL_DATA | INCOMPAT_COMPRESSION_TYPE | INCOMPAT_EXTENDED_L2) != 0 {
                return Err(unsupported(format!("unknown incompatible features {incompat:#x}")));
            }
            dirty = incompat & INCOMPAT_DIRTY != 0;
            autoclear = be64(&ext[16..]);
            refcount_order = be32(&ext[24..]);
        }

        let cluster_size = 1u64 << cluster_bits;
        let l2_entries = cluster_size / 8;
        let needed = size.div_ceil(cluster_size).div_ceil(l2_entries);
        if l1_size < needed {
            return Err(err("L1 table too small for the image size"));
        }
        if l1_off % 8 != 0 || l1_off.checked_add(l1_size * 8).is_none_or(|e| e > file_len) {
            return Err(err("L1 table lies outside the file"));
        }
        r.seek(SeekFrom::Start(l1_off))?;
        let mut raw = vec![0u8; (l1_size * 8) as usize];
        r.read_exact(&mut raw)?;
        let l1 = raw.chunks_exact(8).map(be64).collect();

        let rt_entries = rt_clusters * cluster_size / 8;
        let cannot_write = if refcount_order != 4 {
            Some(format!("it uses {}-bit refcounts (only 16-bit ones can be written)", 1u64 << refcount_order))
        } else if autoclear != 0 {
            Some("it has autoclear features (persistent bitmaps or a raw external data file)".to_string())
        } else if rt_off % cluster_size != 0 || rt_off == 0 || rt_clusters == 0 || rt_off.checked_add(rt_clusters * cluster_size).is_none_or(|e| e > file_len) {
            Some("its refcount table lies outside the file".to_string())
        } else {
            None
        };

        Ok(Qcow2 {
            r,
            file_len,
            version,
            cluster_bits,
            size,
            l1,
            l2: HashMap::new(),
            decompressed: HashMap::new(),
            dirty,
            l1_off,
            rt_off,
            rt_entries,
            rt: Vec::new(),
            cannot_write,
            tx: None,
        })
    }

    /// QEMU left its "dirty" flag set: the image was not closed cleanly (lazy refcounts) or is open
    /// in a running emulator. Reads still work, but the data may be mid-update.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Why a write transaction would be refused right now, if it would be.
    pub fn write_blocker(&self) -> Option<String> {
        if let Some(why) = &self.cannot_write {
            return Some(format!("can't be written to: {why}"));
        }
        self.dirty.then(|| "is marked as in use (xemu is running, or it did not close cleanly)".to_string())
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn cluster_size(&self) -> u64 {
        1 << self.cluster_bits
    }

    fn l2_table(&mut self, l1_index: usize) -> XboxResult<Option<&Vec<u64>>> {
        let entry = self.l1[l1_index];
        let off = entry & OFFSET_MASK;
        if off == 0 {
            return Ok(None);
        }
        if !self.l2.contains_key(&off) {
            let cs = self.cluster_size();
            if off % cs != 0 || off + cs > self.file_len {
                return Err(err("L2 table lies outside the file or is misaligned"));
            }
            if self.l2.len() >= MAX_CACHED_L2 {
                self.l2.clear();
            }
            self.r.seek(SeekFrom::Start(off))?;
            let mut raw = vec![0u8; cs as usize];
            self.r.read_exact(&mut raw)?;
            self.l2.insert(off, raw.chunks_exact(8).map(be64).collect());
        }
        Ok(self.l2.get(&off))
    }

    fn read_compressed(&mut self, cluster_index: u64, entry: u64) -> XboxResult<&Vec<u8>> {
        if !self.decompressed.contains_key(&cluster_index) {
            let cb = self.cluster_bits;
            let shift = 62 - (cb - 8);
            let coff = entry & ((1u64 << shift) - 1);
            let sectors = ((entry >> shift) & ((1u64 << (cb - 8)) - 1)) + 1;
            let csize = sectors * 512 - (coff & 511);
            if coff.checked_add(csize).is_none_or(|e| e > self.file_len) {
                return Err(err("compressed cluster lies outside the file"));
            }
            self.r.seek(SeekFrom::Start(coff))?;
            let mut raw = vec![0u8; csize as usize];
            self.r.read_exact(&mut raw)?;
            let mut out = vec![0u8; self.cluster_size() as usize];
            DeflateDecoder::new(&raw[..])
                .read_exact(&mut out)
                .map_err(|e| err(format!("bad compressed cluster: {e}")))?;
            if self.decompressed.len() >= MAX_CACHED_CLUSTERS {
                self.decompressed.clear();
            }
            self.decompressed.insert(cluster_index, out);
        }
        Ok(&self.decompressed[&cluster_index])
    }

    fn read_in_cluster(&mut self, cluster_index: u64, in_cluster: u64, out: &mut [u8]) -> XboxResult<()> {
        let l2_bits = self.cluster_bits - 3;
        let l1_index = (cluster_index >> l2_bits) as usize;
        let l2_index = (cluster_index & ((1u64 << l2_bits) - 1)) as usize;
        let entry = match self.l1.get(l1_index) {
            None => 0,
            Some(_) => self.l2_table(l1_index)?.map_or(0, |t| t[l2_index]),
        };
        if entry & L2_COMPRESSED != 0 {
            let data = self.read_compressed(cluster_index, entry)?;
            out.copy_from_slice(&data[in_cluster as usize..in_cluster as usize + out.len()]);
            return Ok(());
        }
        let host = entry & OFFSET_MASK;
        let zero = (self.version == 3 && entry & L2_ZERO != 0) || host == 0;
        if zero {
            out.fill(0);
            return Ok(());
        }
        let cs = self.cluster_size();
        if host % cs != 0 || host + cs > self.file_len {
            return Err(err("data cluster lies outside the file or is misaligned"));
        }
        self.r.seek(SeekFrom::Start(host + in_cluster))?;
        self.r.read_exact(out)?;
        Ok(())
    }
}

impl<R: Read + Seek> BlockRead for Qcow2<R> {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&mut self, mut off: u64, mut buf: &mut [u8]) -> XboxResult<()> {
        if off.checked_add(buf.len() as u64).is_none_or(|e| e > self.size) {
            return Err(err("read past the end of the disk"));
        }
        let cs = self.cluster_size();
        while !buf.is_empty() {
            let in_cluster = off & (cs - 1);
            let n = buf.len().min((cs - in_cluster) as usize);
            let (head, rest) = buf.split_at_mut(n);
            self.read_in_cluster(off >> self.cluster_bits, in_cluster, head)?;
            off += n as u64;
            buf = rest;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

/// Where the undo journal of `image` lives: beside it, so the next open finds it.
pub fn journal_path(image: &Path) -> PathBuf {
    let mut p = image.as_os_str().to_owned();
    p.push(".romm-undo");
    PathBuf::from(p)
}

/// Puts an image back the way a crashed (or never-finished) write found it. Returns whether there
/// was anything to undo. A journal that does not belong to this image, or that was never given a
/// chance to protect anything, is discarded rather than applied.
pub fn recover_journal(image: &Path) -> XboxResult<bool> {
    let jp = journal_path(image);
    let data = match std::fs::read(&jp) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let head = 8 + 8 + JOURNAL_HEADER_ID;
    // Cut short before it was complete: it had not yet allowed a single byte of the image to change.
    if data.len() < head || &data[..8] != JOURNAL_MAGIC {
        std::fs::remove_file(&jp)?;
        return Ok(false);
    }
    let orig_len = u64::from_le_bytes(data[8..16].try_into().unwrap());
    if let Some(why) = super::lock::in_use(image) {
        return Err(err(format!(
            "an interrupted write to {} has to be undone first, but the image is in use: {why}. Close it and try again",
            image.display()
        )));
    }
    let mut f = super::lock::open_exclusive(image)?;
    let mut header = [0u8; JOURNAL_HEADER_ID];
    f.read_exact(&mut header)?;
    if header[..] != data[16..head] || f.metadata()?.len() < orig_len {
        return Err(err(format!(
            "{} is left over from another image; the image was not changed. Delete it only if you are sure it is stale",
            jp.display()
        )));
    }
    // Records, until one is cut short (its image write never happened: the journal is synced first).
    let mut records = Vec::new();
    let mut at = head;
    while at + 12 <= data.len() {
        let off = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let len = u32::from_le_bytes(data[at + 8..at + 12].try_into().unwrap()) as usize;
        if at + 12 + len > data.len() {
            break;
        }
        records.push((off, &data[at + 12..at + 12 + len]));
        at += 12 + len;
    }
    for (off, old) in records.iter().rev() {
        f.seek(SeekFrom::Start(*off))?;
        f.write_all(old)?;
    }
    f.set_len(orig_len)?;
    f.sync_all()?;
    std::fs::remove_file(&jp)?;
    Ok(true)
}

impl<R: BackingFile> Qcow2<R> {
    /// Starts a write transaction. `journal`, when given, is a new file the undo records are also
    /// written to (and synced), so [`recover_journal`] can undo a crash. Nothing is changed yet.
    pub fn begin(&mut self, journal: Option<&Path>) -> XboxResult<()> {
        if let Some(why) = &self.cannot_write {
            return Err(unsupported(format!("this image can't be written to: {why}")));
        }
        if self.dirty {
            return Err(err("image is marked as in use (xemu is running, or it did not close cleanly)"));
        }
        if self.tx.is_some() {
            return Err(err("a write transaction is already open"));
        }
        self.r.seek(SeekFrom::Start(self.rt_off))?;
        let mut raw = vec![0u8; (self.rt_entries * 8) as usize];
        self.r.read_exact(&mut raw)?;
        self.rt = raw.chunks_exact(8).map(be64).collect();

        let journal = match journal {
            None => None,
            Some(p) => {
                let mut header = [0u8; JOURNAL_HEADER_ID];
                self.r.seek(SeekFrom::Start(0))?;
                self.r.read_exact(&mut header)?;
                let mut f = OpenOptions::new().write(true).create_new(true).open(p)?;
                f.write_all(JOURNAL_MAGIC)?;
                f.write_all(&self.file_len.to_le_bytes())?;
                f.write_all(&header)?;
                f.sync_all()?;
                Some((f, p.to_path_buf()))
            }
        };
        self.tx = Some(Tx { orig_len: self.file_len, undo: Vec::new(), journal });
        Ok(())
    }

    /// Makes everything since `begin` permanent.
    pub fn commit(&mut self) -> XboxResult<()> {
        let Some(tx) = self.tx.take() else { return Ok(()) };
        self.r.flush()?;
        self.r.sync()?;
        if let Some((f, p)) = tx.journal {
            drop(f); // closed before it is deleted, which some systems insist on
            std::fs::remove_file(p)?;
        }
        Ok(())
    }

    /// Undoes everything since `begin`, exactly.
    pub fn rollback(&mut self) -> XboxResult<()> {
        let Some(tx) = self.tx.take() else { return Ok(()) };
        for (off, old) in tx.undo.iter().rev() {
            self.r.seek(SeekFrom::Start(*off))?;
            self.r.write_all(old)?;
        }
        self.r.set_len(tx.orig_len)?;
        self.file_len = tx.orig_len;
        self.r.flush()?;
        self.r.sync()?;
        if let Some((f, p)) = tx.journal {
            drop(f); // closed before it is deleted, which some systems insist on
            std::fs::remove_file(p)?;
        }
        // What was cached is what was undone.
        self.l2.clear();
        self.decompressed.clear();
        self.r.seek(SeekFrom::Start(self.l1_off))?;
        let mut raw = vec![0u8; self.l1.len() * 8];
        self.r.read_exact(&mut raw)?;
        self.l1 = raw.chunks_exact(8).map(be64).collect();
        Ok(())
    }

    /// Every change to the file goes through here: the old bytes are recorded first.
    fn raw_write(&mut self, off: u64, data: &[u8]) -> XboxResult<()> {
        let orig_len = self.tx.as_ref().ok_or_else(|| err("write outside a transaction"))?.orig_len;
        if off < orig_len && !data.is_empty() {
            let keep = (orig_len - off).min(data.len() as u64) as usize;
            let mut old = vec![0u8; keep];
            self.r.seek(SeekFrom::Start(off))?;
            self.r.read_exact(&mut old)?;
            let tx = self.tx.as_mut().unwrap();
            if let Some((f, _)) = tx.journal.as_mut() {
                f.write_all(&off.to_le_bytes())?;
                f.write_all(&(keep as u32).to_le_bytes())?;
                f.write_all(&old)?;
                f.sync_data()?;
            }
            tx.undo.push((off, old));
        }
        let end = off + data.len() as u64;
        if end > self.file_len {
            self.r.set_len(end)?;
            self.file_len = end;
        }
        self.r.seek(SeekFrom::Start(off))?;
        self.r.write_all(data)?;
        Ok(())
    }

    fn extend_to(&mut self, len: u64) -> XboxResult<()> {
        if len > self.file_len {
            self.r.set_len(len)?;
            self.file_len = len;
        }
        Ok(())
    }

    // ---- refcounts ------------------------------------------------------------------------

    fn refcounts_per_block(&self) -> u64 {
        self.cluster_size() / 2
    }

    fn refcount_block(&self, ci: u64) -> XboxResult<Option<u64>> {
        let bi = (ci / self.refcounts_per_block()) as usize;
        let Some(&e) = self.rt.get(bi) else { return Ok(None) };
        let blk = e & RT_OFFSET_MASK;
        if blk == 0 {
            return Ok(None);
        }
        if blk % self.cluster_size() != 0 || blk + self.cluster_size() > self.file_len {
            return Err(err("refcount block lies outside the file or is misaligned"));
        }
        Ok(Some(blk))
    }

    fn refcount_get(&mut self, ci: u64) -> XboxResult<u16> {
        let Some(blk) = self.refcount_block(ci)? else { return Ok(0) };
        let mut b = [0u8; 2];
        self.r.seek(SeekFrom::Start(blk + (ci % self.refcounts_per_block()) * 2))?;
        self.r.read_exact(&mut b)?;
        Ok(u16::from_be_bytes(b))
    }

    fn refcount_set(&mut self, ci: u64, v: u16) -> XboxResult<()> {
        let blk = self.refcount_block(ci)?.ok_or_else(|| err("no refcount block for that cluster"))?;
        self.raw_write(blk + (ci % self.refcounts_per_block()) * 2, &v.to_be_bytes())
    }

    /// Gives up one reference to `host`, only while another one remains. A cluster this would free
    /// is left alone: a leaked cluster costs space, a wrongly freed one costs data.
    fn release_ref(&mut self, host: u64) -> XboxResult<()> {
        let ci = host >> self.cluster_bits;
        let rc = self.refcount_get(ci)?;
        if rc >= 2 {
            self.refcount_set(ci, rc - 1)?;
        }
        Ok(())
    }

    /// Gives back the one reference a compressed cluster holds on each host cluster its bytes touch
    /// (several compressed clusters are packed into one, each adding one). Unlike a plain cluster
    /// this may take a count to zero: nothing else can be using a cluster whose only reference was
    /// this compressed data, and QEMU frees it the same way.
    fn release_compressed(&mut self, entry: u64) -> XboxResult<()> {
        let cb = self.cluster_bits;
        let shift = 62 - (cb - 8);
        let coff = entry & ((1u64 << shift) - 1);
        let sectors = ((entry >> shift) & ((1u64 << (cb - 8)) - 1)) + 1;
        let csize = sectors * 512 - (coff & 511);
        for ci in (coff >> cb)..=((coff + csize - 1) >> cb) {
            let rc = self.refcount_get(ci)?;
            if rc >= 1 {
                self.refcount_set(ci, rc - 1)?;
            }
        }
        Ok(())
    }

    /// A new zero-filled cluster at the end of the file, with a refcount of one.
    fn alloc_cluster(&mut self) -> XboxResult<u64> {
        let cs = self.cluster_size();
        loop {
            let host = self.file_len.div_ceil(cs) * cs;
            let ci = host >> self.cluster_bits;
            let bi = (ci / self.refcounts_per_block()) as usize;
            if bi >= self.rt.len() {
                return Err(unsupported("the image's refcount table is full, so it can't grow"));
            }
            self.extend_to(host + cs)?;
            if self.rt[bi] & RT_OFFSET_MASK == 0 {
                // No refcount block covers this part of the file yet. The new block goes in this very
                // cluster (QEMU does the same), so it needs no second allocation to describe itself.
                self.raw_write(self.rt_off + bi as u64 * 8, &host.to_be_bytes())?;
                self.rt[bi] = host;
                self.refcount_set(ci, 1)?;
                continue;
            }
            if self.refcount_get(ci)? != 0 {
                return Err(err("a cluster past the end of the file is already counted as in use"));
            }
            self.refcount_set(ci, 1)?;
            return Ok(host);
        }
    }

    // ---- tables ---------------------------------------------------------------------------

    fn l2_cached(&mut self, off: u64) -> XboxResult<&mut Vec<u64>> {
        if !self.l2.contains_key(&off) {
            let cs = self.cluster_size();
            if off % cs != 0 || off + cs > self.file_len {
                return Err(err("L2 table lies outside the file or is misaligned"));
            }
            if self.l2.len() >= MAX_CACHED_L2 {
                self.l2.clear();
            }
            self.r.seek(SeekFrom::Start(off))?;
            let mut raw = vec![0u8; cs as usize];
            self.r.read_exact(&mut raw)?;
            self.l2.insert(off, raw.chunks_exact(8).map(be64).collect());
        }
        Ok(self.l2.get_mut(&off).unwrap())
    }

    /// The offset of an L2 table this image alone owns for `l1_index`: made if there is none, and
    /// copied if a snapshot shares it.
    fn private_l2(&mut self, l1_index: usize) -> XboxResult<u64> {
        let entry = self.l1[l1_index];
        let old = entry & OFFSET_MASK;
        if old != 0 && entry & OFLAG_COPIED != 0 {
            return Ok(old);
        }
        let new = self.alloc_cluster()?;
        let mut table = vec![0u64; (self.cluster_size() / 8) as usize];
        if old != 0 {
            // Every cluster it names is also named by the snapshot's copy, which is why none of them
            // may be written in place through this one.
            table = self.l2_cached(old)?.clone();
            for e in &mut table {
                *e &= !OFLAG_COPIED;
            }
            let bytes: Vec<u8> = table.iter().flat_map(|e| e.to_be_bytes()).collect();
            self.raw_write(new, &bytes)?;
        }
        let l1_entry = new | OFLAG_COPIED;
        self.raw_write(self.l1_off + l1_index as u64 * 8, &l1_entry.to_be_bytes())?;
        self.l1[l1_index] = l1_entry;
        self.l2.insert(new, table);
        if old != 0 {
            self.release_ref(old)?;
        }
        Ok(new)
    }

    fn write_in_cluster(&mut self, ci: u64, in_cluster: u64, data: &[u8]) -> XboxResult<()> {
        let l2_bits = self.cluster_bits - 3;
        let l1_index = (ci >> l2_bits) as usize;
        let l2_index = (ci & ((1u64 << l2_bits) - 1)) as usize;
        if l1_index >= self.l1.len() {
            return Err(err("write past the end of the L1 table"));
        }
        let l2_off = self.private_l2(l1_index)?;
        let entry = self.l2_cached(l2_off)?[l2_index];
        let host = entry & OFFSET_MASK;
        let plain = entry & L2_COMPRESSED == 0;
        let zero = plain && self.version == 3 && entry & L2_ZERO != 0;
        let owned = plain && host != 0 && entry & OFLAG_COPIED != 0;

        // A cluster this image alone has, holding real data: write straight into it.
        if owned && !zero {
            return self.raw_write(host + in_cluster, data);
        }

        // Anything else gets the whole cluster written afresh: what it reads as now, with the new
        // bytes laid over it (compressed, shared with a snapshot, zero, or not there at all).
        let cs = self.cluster_size() as usize;
        let mut buf = vec![0u8; cs];
        self.read_in_cluster(ci, 0, &mut buf)?;
        buf[in_cluster as usize..in_cluster as usize + data.len()].copy_from_slice(data);

        let target = if owned && zero {
            // A zero cluster that was given space and is ours: reuse that space.
            host
        } else {
            self.alloc_cluster()?
        };
        self.raw_write(target, &buf)?;
        let new_entry = target | OFLAG_COPIED;
        self.raw_write(l2_off + l2_index as u64 * 8, &new_entry.to_be_bytes())?;
        self.l2_cached(l2_off)?[l2_index] = new_entry;
        if !plain {
            self.release_compressed(entry)?;
        } else if host != 0 && !owned {
            // It was shared with a snapshot, which keeps its reference.
            self.release_ref(host)?;
        }
        Ok(())
    }
}

impl<R: BackingFile> BlockWrite for Qcow2<R> {
    fn write_at(&mut self, mut off: u64, mut data: &[u8]) -> XboxResult<()> {
        if off.checked_add(data.len() as u64).is_none_or(|e| e > self.size) {
            return Err(err("write past the end of the disk"));
        }
        let cs = self.cluster_size();
        while !data.is_empty() {
            let in_cluster = off & (cs - 1);
            let n = data.len().min((cs - in_cluster) as usize);
            let (head, rest) = data.split_at(n);
            self.write_in_cluster(off >> self.cluster_bits, in_cluster, head)?;
            off += n as u64;
            data = rest;
        }
        Ok(())
    }
}
