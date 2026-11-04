//! A read-only qcow2 reader: just enough to read the guest disk of an xemu HDD image.
//!
//! The format is QEMU's ([specification](https://www.qemu.org/docs/master/interop/qcow2.html)). This
//! reader maps guest offsets through the L1/L2 tables and understands the three ways a cluster can
//! be stored: allocated (plain), zero/unallocated, and zlib-compressed. Anything it cannot read
//! faithfully (backing files, encryption, external data files, zstd compression, extended L2
//! entries, or an image QEMU has flagged corrupt) is refused up front instead of returning guesses.
//!
//! It never writes, and it takes no lock: only read an image while xemu is not running it.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use flate2::read::DeflateDecoder;

use super::{BlockRead, XboxError, XboxResult};

const MAGIC: [u8; 4] = *b"QFI\xfb";
/// Bits 9..=55 of an L1/L2 entry: the host offset.
const OFFSET_MASK: u64 = 0x00ff_ffff_ffff_fe00;
const L2_COMPRESSED: u64 = 1 << 62;
/// v3 only: the cluster reads as zeros whatever its host offset says.
const L2_ZERO: u64 = 1;

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

        let mut dirty = false;
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

        Ok(Qcow2 { r, file_len, version, cluster_bits, size, l1, l2: HashMap::new(), decompressed: HashMap::new(), dirty })
    }

    /// QEMU left its "dirty" flag set: the image was not closed cleanly (lazy refcounts) or is open
    /// in a running emulator. Reads still work, but the data may be mid-update.
    pub fn is_dirty(&self) -> bool {
        self.dirty
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
