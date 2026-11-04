//! A read-only FATX reader (the original Xbox's FAT variant).
//!
//! Layout, as implemented by libfatx (the reference implementation) and documented by the
//! Xbox homebrew community:
//!
//! ```text
//! partition + 0x0000   superblock (4 KiB): "FATX", volume id, sectors/cluster, root cluster
//! partition + 0x1000   the FAT: one 16-bit (FATX16) or 32-bit (FATX32) entry per cluster
//! then                 the file area. Cluster 1 is its first cluster and the root directory
//! ```
//!
//! FAT16 is used while the FAT has fewer than 0xFFF0 entries, FAT32 otherwise; the FAT is rounded
//! up to 4 KiB. A directory is a chain of clusters of 64-byte entries; a first byte of `0x00` or
//! `0xFF` ends the directory, `0xE5` marks a deleted entry that must be skipped, not stopped at.
//!
//! The partition *size* is not stored on disk but fixes where the file area starts, so callers
//! must supply the right one (see `XboxHdd`).

use std::collections::HashSet;

use super::{BlockRead, XboxError, XboxResult};

const SECTOR: u64 = 512;
const SUPERBLOCK: u64 = 4096;
const ENTRY_LEN: usize = 64;
const MAX_NAME: usize = 42;
pub const ATTR_DIRECTORY: u8 = 0x10;
const DELETED: u8 = 0xE5;

fn err(msg: impl Into<String>) -> XboxError {
    XboxError::Fatx(msg.into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Raw bytes mapped one-to-one to chars (Latin-1), so nothing is lost or invented.
    pub name: String,
    pub attributes: u8,
    pub first_cluster: u32,
    pub size: u32,
}

impl DirEntry {
    pub fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }
}

pub struct Fatx<D> {
    dev: D,
    offset: u64,
    bytes_per_cluster: u64,
    fat16: bool,
    area_offset: u64,
    /// Highest valid data cluster number (clusters are numbered from 1).
    max_cluster: u32,
    root: u32,
}

impl<D: BlockRead> Fatx<D> {
    pub fn open(mut dev: D, offset: u64, size: u64) -> XboxResult<Self> {
        if offset.checked_add(size).is_none_or(|e| e > dev.size()) {
            return Err(err("partition lies outside the disk"));
        }
        let mut sb = [0u8; 16];
        dev.read_at(offset, &mut sb)?;
        if &sb[..4] != b"FATX" {
            return Err(err("no FATX signature"));
        }
        let spc = u32::from_le_bytes(sb[8..12].try_into().unwrap()) as u64;
        let root = u32::from_le_bytes(sb[12..16].try_into().unwrap());
        if spc == 0 || !spc.is_power_of_two() || spc > 1024 {
            return Err(err(format!("invalid sectors per cluster ({spc})")));
        }
        let bpc = spc * SECTOR;
        let entries = size / bpc + 1;
        let fat16 = entries < 0xFFF0;
        let fat_size = (entries * if fat16 { 2 } else { 4 }).div_ceil(4096) * 4096;
        if size < SUPERBLOCK + fat_size + bpc {
            return Err(err("partition too small for its own FAT"));
        }
        let max_cluster = ((size - SUPERBLOCK - fat_size) / bpc).min(u32::MAX as u64 - 16) as u32;
        if root == 0 || root > max_cluster {
            return Err(err(format!("root cluster {root} is outside the volume")));
        }
        Ok(Fatx { dev, offset, bytes_per_cluster: bpc, fat16, area_offset: offset + SUPERBLOCK + fat_size, max_cluster, root })
    }

    pub fn is_fat16(&self) -> bool {
        self.fat16
    }

    pub fn bytes_per_cluster(&self) -> u64 {
        self.bytes_per_cluster
    }

    pub fn root_cluster(&self) -> u32 {
        self.root
    }

    pub fn device(&self) -> &D {
        &self.dev
    }

    pub fn into_device(self) -> D {
        self.dev
    }

    fn fat_entry(&mut self, cluster: u32) -> XboxResult<u32> {
        let at = self.offset + SUPERBLOCK + cluster as u64 * if self.fat16 { 2 } else { 4 };
        if self.fat16 {
            let mut b = [0u8; 2];
            self.dev.read_at(at, &mut b)?;
            let v = u16::from_le_bytes(b) as u32;
            // 0xFFF0.. is reserved / end-of-chain in FAT16; widen to the FAT32 values so one rule serves both.
            Ok(if v >= 0xFFF0 { 0xFFFF_0000 | v } else { v })
        } else {
            let mut b = [0u8; 4];
            self.dev.read_at(at, &mut b)?;
            Ok(u32::from_le_bytes(b))
        }
    }

    /// The clusters of the chain starting at `first`, validated: no loops, no free or reserved
    /// links, nothing outside the volume.
    pub fn chain(&mut self, first: u32) -> XboxResult<Vec<u32>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut c = first;
        loop {
            if c == 0 || c > self.max_cluster {
                return Err(err(format!("cluster {c} is outside the volume")));
            }
            if !seen.insert(c) {
                return Err(err("cluster chain loops back on itself"));
            }
            out.push(c);
            match self.fat_entry(c)? {
                0 => return Err(err(format!("cluster chain runs into free cluster after {c}"))),
                n if n >= 0xFFFF_FFF8 => return Ok(out),
                n if n >= 0xFFFF_FFF0 => return Err(err(format!("cluster chain runs into a reserved marker after {c}"))),
                n => c = n,
            }
        }
    }

    fn read_cluster(&mut self, cluster: u32, buf: &mut [u8]) -> XboxResult<()> {
        let at = self.area_offset + (cluster as u64 - 1) * self.bytes_per_cluster;
        self.dev.read_at(at, buf)
    }

    /// All live entries of the directory whose first cluster is `first`, in on-disk order.
    pub fn read_dir(&mut self, first: u32) -> XboxResult<Vec<DirEntry>> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; self.bytes_per_cluster as usize];
        'dir: for cluster in self.chain(first)? {
            self.read_cluster(cluster, &mut buf)?;
            for raw in buf.chunks_exact(ENTRY_LEN) {
                match raw[0] {
                    0x00 | 0xFF => break 'dir,
                    DELETED => continue,
                    n if n as usize > MAX_NAME => return Err(err(format!("directory entry with a {n}-byte name"))),
                    n => {
                        let n = n as usize;
                        out.push(DirEntry {
                            name: raw[2..2 + n].iter().map(|&b| b as char).collect(),
                            attributes: raw[1],
                            first_cluster: u32::from_le_bytes(raw[44..48].try_into().unwrap()),
                            size: u32::from_le_bytes(raw[48..52].try_into().unwrap()),
                        });
                    }
                }
            }
        }
        Ok(out)
    }

    /// Case-insensitive lookup, as on the console.
    pub fn find(&mut self, dir_first: u32, name: &str) -> XboxResult<Option<DirEntry>> {
        Ok(self.read_dir(dir_first)?.into_iter().find(|e| e.name.eq_ignore_ascii_case(name)))
    }

    pub fn read_file(&mut self, e: &DirEntry) -> XboxResult<Vec<u8>> {
        if e.is_dir() {
            return Err(err(format!("{} is a directory", e.name)));
        }
        let size = e.size as u64;
        if size == 0 {
            return Ok(Vec::new());
        }
        let chain = self.chain(e.first_cluster)?;
        let need = size.div_ceil(self.bytes_per_cluster) as usize;
        if chain.len() < need {
            return Err(err(format!("{} claims {size} bytes but its chain holds {} clusters", e.name, chain.len())));
        }
        let mut out = Vec::with_capacity(size as usize);
        let mut buf = vec![0u8; self.bytes_per_cluster as usize];
        for &c in &chain[..need] {
            self.read_cluster(c, &mut buf)?;
            let take = (size as usize - out.len()).min(buf.len());
            out.extend_from_slice(&buf[..take]);
        }
        Ok(out)
    }
}
