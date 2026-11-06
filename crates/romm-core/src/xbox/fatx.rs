//! A FATX reader and writer (the original Xbox's FAT variant).
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
//!
//! Writing mirrors libfatx: a new cluster is marked end-of-chain at once, new directory clusters are
//! filled with `0xFF` (the console's end-of-directory marker), a new entry that takes the end marker's
//! slot moves the marker one slot on (so stale entries after it stay hidden), and deleting marks the
//! entry `0xE5` and frees its chain. The FAT is held in memory while writing and written back in
//! 4 KiB pages by `flush_fat`. Timestamps use the console's 2000 epoch.

use std::collections::{BTreeSet, HashSet};

use chrono::{Datelike, Local, Timelike};

use super::{BlockRead, BlockWrite, XboxError, XboxResult};

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
    fat_size: u64,
    /// The whole FAT, while a write is in progress.
    fat: Option<FatCache>,
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
        Ok(Fatx {
            dev,
            offset,
            bytes_per_cluster: bpc,
            fat16,
            fat_size,
            fat: None,
            area_offset: offset + SUPERBLOCK + fat_size,
            max_cluster,
            root,
        })
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

    pub fn device_mut(&mut self) -> &mut D {
        &mut self.dev
    }

    pub fn into_device(self) -> D {
        self.dev
    }

    fn fat_entry(&mut self, cluster: u32) -> XboxResult<u32> {
        if let Some(c) = &self.fat {
            let w = if self.fat16 { 2 } else { 4 };
            let i = cluster as usize * w;
            let Some(b) = c.raw.get(i..i + w) else { return Err(err(format!("cluster {cluster} is outside the FAT"))) };
            return Ok(if self.fat16 {
                let v = u16::from_le_bytes(b.try_into().unwrap()) as u32;
                if v >= 0xFFF0 { 0xFFFF_0000 | v } else { v }
            } else {
                u32::from_le_bytes(b.try_into().unwrap())
            });
        }
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

    /// How many clusters are free.
    pub fn free_clusters(&mut self) -> XboxResult<u64> {
        let mut n = 0;
        for c in 1..=self.max_cluster {
            if self.fat_entry(c)? == 0 {
                n += 1;
            }
        }
        Ok(n)
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

/// A date and time as the console stores them: 16-bit FAT-style words counted from the year 2000.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatxTime {
    pub date: u16,
    pub time: u16,
}

impl FatxTime {
    pub fn new(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Self {
        let y = (year - 2000).clamp(0, 127) as u16;
        FatxTime {
            date: (y << 9) | ((month as u16 & 0xF) << 5) | (day as u16 & 0x1F),
            time: ((hour as u16 & 0x1F) << 11) | ((minute as u16 & 0x3F) << 5) | ((second as u16 / 2) & 0x1F),
        }
    }

    /// Local time, as the console's clock is.
    pub fn now() -> Self {
        let n = Local::now();
        FatxTime::new(n.year(), n.month(), n.day(), n.hour(), n.minute(), n.second())
    }
}

struct FatCache {
    raw: Vec<u8>,
    dirty: BTreeSet<usize>,
    /// Where the next search for a free cluster starts.
    rotor: u32,
}

const FAT_PAGE: usize = 4096;

/// A name the console will accept: 1 to 42 printable ASCII characters, none of `\ / : * ? " < > |`.
pub fn check_name(name: &str) -> XboxResult<()> {
    let bad = name.is_empty()
        || name.len() > MAX_NAME
        || name == "."
        || name == ".."
        || name.bytes().any(|b| !(0x20..0x7F).contains(&b) || b"\\/:*?\"<>|".contains(&b));
    if bad {
        return Err(err(format!("{name:?} is not a name the Xbox filesystem allows (1-{MAX_NAME} printable ASCII characters, none of \\ / : * ? \" < > |)")));
    }
    Ok(())
}

/// A directory entry together with where it sits, so it can be changed or deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub cluster: u32,
    pub index: usize,
    pub entry: DirEntry,
}

impl<D: BlockRead + BlockWrite> Fatx<D> {
    fn end_marker(&self) -> u32 {
        if self.fat16 { 0xFFFF } else { 0xFFFF_FFFF }
    }

    /// Loads the FAT; every change until `flush_fat` is made to this copy.
    pub fn begin_write(&mut self) -> XboxResult<()> {
        if self.fat.is_some() {
            return Ok(());
        }
        let mut raw = vec![0u8; self.fat_size as usize];
        self.dev.read_at(self.offset + SUPERBLOCK, &mut raw)?;
        self.fat = Some(FatCache { raw, dirty: BTreeSet::new(), rotor: 1 });
        Ok(())
    }

    /// Writes back the FAT pages that changed.
    pub fn flush_fat(&mut self) -> XboxResult<()> {
        let Some(c) = self.fat.as_mut() else { return Ok(()) };
        let pages: Vec<usize> = std::mem::take(&mut c.dirty).into_iter().collect();
        for p in pages {
            let from = p * FAT_PAGE;
            let to = (from + FAT_PAGE).min(c.raw.len());
            let chunk = c.raw[from..to].to_vec();
            self.dev.write_at(self.offset + SUPERBLOCK + from as u64, &chunk)?;
        }
        Ok(())
    }

    fn set_fat(&mut self, cluster: u32, value: u32) -> XboxResult<()> {
        let w = if self.fat16 { 2 } else { 4 };
        let c = self.fat.as_mut().ok_or_else(|| err("the FAT is not loaded for writing"))?;
        let i = cluster as usize * w;
        if i + w > c.raw.len() {
            return Err(err(format!("cluster {cluster} is outside the FAT")));
        }
        if self.fat16 {
            c.raw[i..i + 2].copy_from_slice(&(value as u16).to_le_bytes());
        } else {
            c.raw[i..i + 4].copy_from_slice(&value.to_le_bytes());
        }
        c.dirty.insert(i / FAT_PAGE);
        Ok(())
    }

    /// Takes a free cluster and marks it end-of-chain.
    fn alloc_cluster(&mut self) -> XboxResult<u32> {
        let start = self.fat.as_ref().ok_or_else(|| err("the FAT is not loaded for writing"))?.rotor.clamp(1, self.max_cluster);
        let mut c = start;
        loop {
            if self.fat_entry(c)? == 0 {
                let end = self.end_marker();
                self.set_fat(c, end)?;
                self.fat.as_mut().unwrap().rotor = if c >= self.max_cluster { 1 } else { c + 1 };
                return Ok(c);
            }
            c = if c >= self.max_cluster { 1 } else { c + 1 };
            if c == start {
                return Err(err("the volume is full"));
            }
        }
    }

    fn write_cluster(&mut self, cluster: u32, buf: &[u8]) -> XboxResult<()> {
        let at = self.area_offset + (cluster as u64 - 1) * self.bytes_per_cluster;
        self.dev.write_at(at, buf)
    }

    /// Frees a whole chain. The chain is validated first, so a damaged one frees nothing.
    fn free_chain(&mut self, first: u32) -> XboxResult<()> {
        for c in self.chain(first)? {
            self.set_fat(c, 0)?;
        }
        Ok(())
    }

    /// Every entry of a directory, with its position.
    pub fn read_dir_located(&mut self, first: u32) -> XboxResult<Vec<Located>> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; self.bytes_per_cluster as usize];
        'dir: for cluster in self.chain(first)? {
            self.read_cluster(cluster, &mut buf)?;
            for (index, raw) in buf.chunks_exact(ENTRY_LEN).enumerate() {
                match raw[0] {
                    0x00 | 0xFF => break 'dir,
                    DELETED => continue,
                    n if n as usize > MAX_NAME => return Err(err(format!("directory entry with a {n}-byte name"))),
                    n => {
                        let n = n as usize;
                        out.push(Located {
                            cluster,
                            index,
                            entry: DirEntry {
                                name: raw[2..2 + n].iter().map(|&b| b as char).collect(),
                                attributes: raw[1],
                                first_cluster: u32::from_le_bytes(raw[44..48].try_into().unwrap()),
                                size: u32::from_le_bytes(raw[48..52].try_into().unwrap()),
                            },
                        });
                    }
                }
            }
        }
        Ok(out)
    }

    fn entry_addr(&self, cluster: u32, index: usize) -> u64 {
        self.area_offset + (cluster as u64 - 1) * self.bytes_per_cluster + (index * ENTRY_LEN) as u64
    }

    /// A slot for a new entry: the first deleted one, else the end-of-directory slot (moving the
    /// marker on), else a new cluster at the end of the directory.
    fn free_slot(&mut self, dir_first: u32) -> XboxResult<(u32, usize)> {
        let per = self.bytes_per_cluster as usize / ENTRY_LEN;
        let chain = self.chain(dir_first)?;
        let mut buf = vec![0u8; self.bytes_per_cluster as usize];
        for (ci, &c) in chain.iter().enumerate() {
            self.read_cluster(c, &mut buf)?;
            for i in 0..per {
                match buf[i * ENTRY_LEN] {
                    DELETED => return Ok((c, i)),
                    0x00 | 0xFF => {
                        if i + 1 < per {
                            self.dev.write_at(self.entry_addr(c, i + 1), &[0xFF])?;
                        } else if let Some(&next) = chain.get(ci + 1) {
                            self.dev.write_at(self.entry_addr(next, 0), &[0xFF])?;
                        }
                        return Ok((c, i));
                    }
                    _ => {}
                }
            }
        }
        // Every slot is in use and nothing ends the directory: it grows by a cluster of end markers.
        let new = self.alloc_cluster()?;
        let ff = vec![0xFFu8; self.bytes_per_cluster as usize];
        self.write_cluster(new, &ff)?;
        let last = *chain.last().unwrap();
        self.set_fat(last, new)?;
        Ok((new, 0))
    }

    fn add_entry(&mut self, dir_first: u32, name: &str, attr: u8, first: u32, size: u32, ts: FatxTime) -> XboxResult<()> {
        check_name(name)?;
        if self.find(dir_first, name)?.is_some() {
            return Err(err(format!("{name:?} already exists in that folder")));
        }
        let (cluster, index) = self.free_slot(dir_first)?;
        let mut raw = [0xFFu8; ENTRY_LEN];
        raw[0] = name.len() as u8;
        raw[1] = attr;
        raw[2..2 + name.len()].copy_from_slice(name.as_bytes());
        raw[44..48].copy_from_slice(&first.to_le_bytes());
        raw[48..52].copy_from_slice(&size.to_le_bytes());
        for at in [52usize, 56, 60] {
            raw[at..at + 2].copy_from_slice(&ts.time.to_le_bytes());
            raw[at + 2..at + 4].copy_from_slice(&ts.date.to_le_bytes());
        }
        self.dev.write_at(self.entry_addr(cluster, index), &raw)
    }

    /// Makes a folder (one cluster of end markers) in `parent_first`; returns its first cluster.
    pub fn create_dir(&mut self, parent_first: u32, name: &str, ts: FatxTime) -> XboxResult<u32> {
        check_name(name)?;
        let c = self.alloc_cluster()?;
        let ff = vec![0xFFu8; self.bytes_per_cluster as usize];
        self.write_cluster(c, &ff)?;
        self.add_entry(parent_first, name, ATTR_DIRECTORY, c, 0, ts)?;
        Ok(c)
    }

    /// Makes a file in `parent_first`. An empty file still gets one cluster, as libfatx gives it.
    pub fn create_file(&mut self, parent_first: u32, name: &str, data: &[u8], ts: FatxTime) -> XboxResult<()> {
        check_name(name)?;
        let size = u32::try_from(data.len()).map_err(|_| err("file is larger than 4 GiB"))?;
        let bpc = self.bytes_per_cluster as usize;
        let n = data.len().div_ceil(bpc).max(1);
        let first = self.alloc_cluster()?;
        let mut prev = first;
        let mut clusters = vec![first];
        for _ in 1..n {
            let c = self.alloc_cluster()?;
            self.set_fat(prev, c)?;
            prev = c;
            clusters.push(c);
        }
        let mut buf = vec![0u8; bpc];
        for (i, &c) in clusters.iter().enumerate() {
            let chunk = &data[(i * bpc).min(data.len())..((i + 1) * bpc).min(data.len())];
            buf.fill(0);
            buf[..chunk.len()].copy_from_slice(chunk);
            self.write_cluster(c, &buf)?;
        }
        self.add_entry(parent_first, name, 0, first, size, ts)
    }

    /// How many clusters an entry and everything under it occupies.
    pub fn clusters_used(&mut self, e: &DirEntry) -> XboxResult<u64> {
        self.clusters_used_at(e, 0)
    }

    fn clusters_used_at(&mut self, e: &DirEntry, depth: usize) -> XboxResult<u64> {
        if depth > 32 {
            return Err(err("folders nested too deeply"));
        }
        if e.first_cluster == 0 {
            return Ok(0);
        }
        let mut n = self.chain(e.first_cluster)?.len() as u64;
        if e.is_dir() {
            for child in self.read_dir(e.first_cluster)? {
                n += self.clusters_used_at(&child, depth + 1)?;
            }
        }
        Ok(n)
    }

    /// Deletes the entry called `name` in `dir_first`, and a folder's contents with it.
    pub fn remove(&mut self, dir_first: u32, name: &str) -> XboxResult<()> {
        self.remove_at(dir_first, name, 0)
    }

    fn remove_at(&mut self, dir_first: u32, name: &str, depth: usize) -> XboxResult<()> {
        if depth > 32 {
            return Err(err("folders nested too deeply"));
        }
        let Some(at) = self.read_dir_located(dir_first)?.into_iter().find(|l| l.entry.name.eq_ignore_ascii_case(name)) else {
            return Ok(());
        };
        let e = at.entry;
        if e.is_dir() {
            for child in self.read_dir(e.first_cluster)? {
                self.remove_at(e.first_cluster, &child.name, depth + 1)?;
            }
        }
        if e.first_cluster != 0 {
            self.free_chain(e.first_cluster)?;
        }
        self.dev.write_at(self.entry_addr(at.cluster, at.index), &[DELETED])
    }
}
