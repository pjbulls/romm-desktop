//! Original-Xbox saves, read out of an xemu hard-disk image.
//!
//! xemu keeps the console's disk in one multi-GB qcow2 file; a game's save is the folder
//! `E:\UDATA\<title id>` inside the FATX filesystem on it. Argosy handles the same platform on
//! Android (hakuX keeps that folder as plain files) and sends it as `<TITLE ID>.zip` holding
//! `<TITLE ID>/...`, so that is the unit here: the same zip, rooted at the upper-case title id
//! (what Sigil calls `save_id`), UDATA only. RomM's `content_hash` of a zip covers entry names and
//! contents and nothing else, which is why two clients can agree on a save they stored differently.
//!
//! Reading only, for now. Nothing in this module writes to the image, and it takes no lock, so
//! callers must only open an image xemu is not running.

pub mod fatx;
pub mod qcow2;

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Cursor, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use fatx::{DirEntry, Fatx};
use qcow2::Qcow2;

pub type XboxResult<T> = std::result::Result<T, XboxError>;

#[derive(Error, Debug)]
pub enum XboxError {
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("qcow2 image: {0}")]
    Qcow2(String),
    #[error("unsupported qcow2 image: {0}")]
    Unsupported(String),
    #[error("Xbox filesystem: {0}")]
    Fatx(String),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
}

/// A readable disk: what the FATX reader needs from qcow2 (or from memory, in tests).
pub trait BlockRead {
    fn size(&self) -> u64;
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> XboxResult<()>;
}

impl<T: BlockRead + ?Sized> BlockRead for &mut T {
    fn size(&self) -> u64 {
        (**self).size()
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> XboxResult<()> {
        (**self).read_at(off, buf)
    }
}

/// A disk held in memory, for tests and tools.
pub struct MemDisk(pub Vec<u8>);

impl BlockRead for MemDisk {
    fn size(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> XboxResult<()> {
        let end = off.checked_add(buf.len() as u64).filter(|&e| e <= self.0.len() as u64);
        let Some(end) = end else { return Err(XboxError::Fatx("read past the end of the disk".into())) };
        buf.copy_from_slice(&self.0[off as usize..end as usize]);
        Ok(())
    }
}

/// File name of the hard-disk image xemu's own downloads and first-run setup use.
pub const DEFAULT_HDD_NAME: &str = "xbox_hdd.qcow2";

/// Where xemu keeps its hard-disk image: `hdd_path` under `[sys.files]` in the `xemu.toml` inside
/// `data_root`, else `xbox_hdd.qcow2` beside it. xemu resolves a relative path against that same
/// folder (portable installs write bare file names), so that is what is done here. The file need
/// not exist; whoever opens it reports that.
pub fn xemu_hdd_path(data_root: &Path) -> PathBuf {
    let configured = std::fs::read_to_string(data_root.join("xemu.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|t| t.get("sys")?.get("files")?.get("hdd_path")?.as_str().map(str::to_owned))
        .filter(|p| !p.trim().is_empty());
    match configured {
        Some(p) => data_root.join(p),
        None => data_root.join(DEFAULT_HDD_NAME),
    }
}

/// Partition E on a retail layout: where user data and games live. Offset and length are the
/// ones libfatx uses; the length matters because it fixes where the file area starts.
pub const E_OFFSET: u64 = 0xABE8_0000;
pub const E_SIZE: u64 = 0x1_312D_6000;

/// Refuse to build an archive bigger than this: no real save is, and a corrupt directory entry
/// claiming gigabytes should fail rather than exhaust memory.
pub const MAX_EXPORT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DEPTH: usize = 32;

/// A title folder found in `E:\UDATA`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleDir {
    /// Eight hex digits, upper-case: Sigil's `save_id`, and the zip's root folder.
    pub save_id: String,
    /// The folder's name as stored on the disk (any case).
    pub dir_name: String,
}

pub fn is_title_id(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub struct XboxHdd<D: BlockRead> {
    fs: Fatx<D>,
}

pub type FileHdd = XboxHdd<Qcow2<BufReader<File>>>;

impl FileHdd {
    /// Opens an xemu qcow2 image and its E: partition.
    pub fn open(path: &Path) -> XboxResult<Self> {
        let disk = Qcow2::new(BufReader::new(File::open(path)?))?;
        Self::open_e(disk)
    }

    /// Like [`open`], for a partition at an explicit place (non-retail layouts, test images).
    pub fn open_region(path: &Path, offset: u64, size: u64) -> XboxResult<Self> {
        let disk = Qcow2::new(BufReader::new(File::open(path)?))?;
        Ok(XboxHdd { fs: Fatx::open(disk, offset, size)? })
    }
}

impl<D: BlockRead> XboxHdd<D> {
    /// Opens partition E. Its length is not stored on disk, so the retail length is tried first and,
    /// if the root directory it produces is not a console's (no UDATA or TDATA), the rest of the disk.
    /// A size that parses but looks empty (a freshly formatted disk) is kept as a last resort.
    pub fn open_e(mut disk: D) -> XboxResult<Self> {
        let rest = disk.size().saturating_sub(E_OFFSET) & !511;
        let mut fallback = None;
        let mut last_err = None;
        for size in [E_SIZE, rest] {
            // Probe through a borrow, so a failed attempt does not consume the disk.
            let root = Fatx::open(&mut disk, E_OFFSET, size).and_then(|mut fs| {
                let root = fs.root_cluster();
                fs.read_dir(root)
            });
            match root {
                Ok(es) if es.iter().any(|e| e.name.eq_ignore_ascii_case("UDATA") || e.name.eq_ignore_ascii_case("TDATA")) => {
                    return Ok(XboxHdd { fs: Fatx::open(disk, E_OFFSET, size)? });
                }
                Ok(_) => {
                    fallback.get_or_insert(size);
                }
                Err(e) => last_err = Some(e),
            }
        }
        if let Some(size) = fallback {
            return Ok(XboxHdd { fs: Fatx::open(disk, E_OFFSET, size)? });
        }
        Err(last_err.unwrap_or_else(|| XboxError::Fatx("no usable E: partition".into())))
    }

    pub fn fatx(&mut self) -> &mut Fatx<D> {
        &mut self.fs
    }

    /// The disk underneath (for its own state, such as qcow2's dirty flag).
    pub fn device(&self) -> &D {
        self.fs.device()
    }

    fn udata(&mut self) -> XboxResult<Option<DirEntry>> {
        let root = self.fs.root_cluster();
        Ok(self.fs.find(root, "UDATA")?.filter(DirEntry::is_dir))
    }

    /// Every `E:\UDATA\<8 hex digits>` folder, in on-disk order.
    pub fn titles(&mut self) -> XboxResult<Vec<TitleDir>> {
        let Some(udata) = self.udata()? else { return Ok(Vec::new()) };
        Ok(self
            .fs
            .read_dir(udata.first_cluster)?
            .into_iter()
            .filter(|e| e.is_dir() && is_title_id(&e.name))
            .map(|e| TitleDir { save_id: e.name.to_ascii_uppercase(), dir_name: e.name })
            .collect())
    }

    /// The save for one title as Argosy's zip: `<SAVE_ID>/...`, every file and sub-folder of
    /// `UDATA\<save id>`. `None` when the title has no folder or no files, as Argosy refuses an
    /// empty archive too. TDATA is not included (Argosy's unit does not carry it either).
    pub fn export_zip(&mut self, save_id: &str) -> XboxResult<Option<Vec<u8>>> {
        if !is_title_id(save_id) {
            return Err(XboxError::Fatx(format!("{save_id:?} is not an 8-digit title id")));
        }
        let root_name = save_id.to_ascii_uppercase();
        let Some(udata) = self.udata()? else { return Ok(None) };
        let Some(dir) = self.fs.find(udata.first_cluster, save_id)?.filter(DirEntry::is_dir) else { return Ok(None) };

        let mut nodes = Vec::new();
        let mut budget = MAX_EXPORT_BYTES;
        let mut seen = HashSet::from([dir.first_cluster]);
        self.collect(dir.first_cluster, &root_name, 0, &mut seen, &mut nodes, &mut budget)?;
        if !nodes.iter().any(|n| matches!(n, Node::File(..))) {
            return Ok(None);
        }

        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        for node in nodes {
            match node {
                Node::Dir(path) => zw.add_directory(path, opts)?,
                Node::File(path, data) => {
                    zw.start_file(path, opts)?;
                    zw.write_all(&data)?;
                }
            }
        }
        Ok(Some(zw.finish()?.into_inner()))
    }

    fn collect(
        &mut self,
        dir_first: u32,
        prefix: &str,
        depth: usize,
        seen: &mut HashSet<u32>,
        out: &mut Vec<Node>,
        budget: &mut u64,
    ) -> XboxResult<()> {
        if depth > MAX_DEPTH {
            return Err(XboxError::Fatx("folders nested too deeply".into()));
        }
        let mut entries = self.fs.read_dir(dir_first)?;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for e in entries {
            if e.name.is_empty() || e.name == "." || e.name == ".." || e.name.contains(['/', '\\', '\0']) {
                return Err(XboxError::Fatx(format!("unsafe file name {:?}", e.name)));
            }
            let path = format!("{prefix}/{}", e.name);
            if e.is_dir() {
                if !seen.insert(e.first_cluster) {
                    return Err(XboxError::Fatx("folder structure loops back on itself".into()));
                }
                out.push(Node::Dir(path.clone()));
                self.collect(e.first_cluster, &path, depth + 1, seen, out, budget)?;
            } else {
                *budget = budget
                    .checked_sub(e.size as u64)
                    .ok_or_else(|| XboxError::Fatx(format!("save is larger than {} MiB", MAX_EXPORT_BYTES >> 20)))?;
                let data = self.fs.read_file(&e)?;
                out.push(Node::File(path, data));
            }
        }
        Ok(())
    }
}

enum Node {
    Dir(String),
    File(String, Vec<u8>),
}
