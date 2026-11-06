//! Original-Xbox saves, read out of an xemu hard-disk image.
//!
//! xemu keeps the console's disk in one multi-GB qcow2 file; a game's save is the folder
//! `E:\UDATA\<title id>` inside the FATX filesystem on it. Argosy handles the same platform on
//! Android (hakuX keeps that folder as plain files) and sends it as `<TITLE ID>.zip` holding
//! `<TITLE ID>/...`, so that is the unit here: the same zip, rooted at the upper-case title id
//! (what Sigil calls `save_id`), UDATA only. RomM's `content_hash` of a zip covers entry names and
//! contents and nothing else, which is why two clients can agree on a save they stored differently.
//!
//! Reading is the default. Writing a save back ([`import_save`]) is journaled end to end (see
//! `qcow2`), touches nothing but `UDATA\<title id>` and the bookkeeping it needs, and reads the
//! folder back and compares it with the zip before it is made permanent. Nothing takes a lock, so
//! callers must only open an image xemu is not running.

pub mod fatx;
pub mod lock;
pub mod qcow2;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use fatx::{DirEntry, Fatx, FatxTime};
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

/// A disk that can also be written (qcow2 with a transaction open, or memory in tests).
pub trait BlockWrite: BlockRead {
    fn write_at(&mut self, off: u64, data: &[u8]) -> XboxResult<()>;
}

impl<T: BlockWrite + ?Sized> BlockWrite for &mut T {
    fn write_at(&mut self, off: u64, data: &[u8]) -> XboxResult<()> {
        (**self).write_at(off, data)
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

impl BlockWrite for MemDisk {
    fn write_at(&mut self, off: u64, data: &[u8]) -> XboxResult<()> {
        let end = off.checked_add(data.len() as u64).filter(|&e| e <= self.0.len() as u64);
        let Some(end) = end else { return Err(XboxError::Fatx("write past the end of the disk".into())) };
        self.0[off as usize..end as usize].copy_from_slice(data);
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
        // An interrupted write is undone before anything is read: its half-written folder must
        // never be mistaken for a save.
        qcow2::recover_journal(path)?;
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

    /// A partition at an explicit place on an already opened disk (non-retail layouts, test images).
    pub fn with_region(disk: D, offset: u64, size: u64) -> XboxResult<Self> {
        Ok(XboxHdd { fs: Fatx::open(disk, offset, size)? })
    }

    pub fn fatx(&mut self) -> &mut Fatx<D> {
        &mut self.fs
    }

    /// The disk underneath (for its own state, such as qcow2's dirty flag).
    pub fn device(&self) -> &D {
        self.fs.device()
    }

    pub fn device_mut(&mut self) -> &mut D {
        self.fs.device_mut()
    }

    pub fn into_device(self) -> D {
        self.fs.into_device()
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

// ---------------------------------------------------------------------------------------------
// Putting a save back
// ---------------------------------------------------------------------------------------------

/// Most entries a save zip may hold; no real save has more than a few dozen.
const MAX_ZIP_ENTRIES: usize = 20_000;

/// What an import did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportSummary {
    pub files: usize,
    pub dirs: usize,
    pub bytes: u64,
}

/// One entry of a save zip, below the title folder.
struct SaveNode {
    /// Path under the title folder, `/`-separated.
    rel: String,
    /// `None` for a folder.
    data: Option<Vec<u8>>,
}

/// Reads and vets a save zip (Argosy's shape: everything under `<TITLE ID>/`) before anything is
/// written. Anything the console could not hold, or that tries to reach outside the title folder,
/// is refused outright. Parents come before the things in them.
fn parse_save_zip(save_id: &str, bytes: &[u8]) -> XboxResult<Vec<SaveNode>> {
    let bad = |m: String| XboxError::Fatx(format!("the save zip can't be restored: {m}"));
    let mut z = ZipArchive::new(Cursor::new(bytes))?;
    if z.len() > MAX_ZIP_ENTRIES {
        return Err(bad(format!("it has {} entries", z.len())));
    }
    let mut nodes: BTreeMap<String, SaveNode> = BTreeMap::new();
    let mut budget = MAX_EXPORT_BYTES;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let raw = f.name().to_string();
        if raw.contains('\\') || raw.starts_with('/') || raw.contains('\0') {
            return Err(bad(format!("entry {raw:?} has an unsafe path")));
        }
        let is_dir = f.is_dir() || raw.ends_with('/');
        let comps: Vec<&str> = raw.trim_end_matches('/').split('/').collect();
        if !comps[0].eq_ignore_ascii_case(save_id) {
            return Err(bad(format!("entry {raw:?} is not inside the folder {}", save_id.to_ascii_uppercase())));
        }
        if comps.len() == 1 {
            if !is_dir {
                return Err(bad(format!("{raw:?} is a file where the title folder should be")));
            }
            continue;
        }
        let rel_comps = &comps[1..];
        if rel_comps.len() > MAX_DEPTH {
            return Err(bad(format!("{raw:?} is nested too deeply")));
        }
        for c in rel_comps {
            fatx::check_name(c).map_err(|e| bad(e.to_string()))?;
        }
        let key = |n: usize| rel_comps[..n].join("/").to_ascii_lowercase();
        // The folders above it exist whether or not the zip lists them.
        for n in 1..rel_comps.len() {
            match nodes.get(&key(n)) {
                Some(SaveNode { data: Some(_), .. }) => return Err(bad(format!("{raw:?} is inside a file"))),
                Some(_) => {}
                None => {
                    nodes.insert(key(n), SaveNode { rel: rel_comps[..n].join("/"), data: None });
                }
            }
        }
        let k = key(rel_comps.len());
        if is_dir {
            match nodes.get(&k) {
                Some(SaveNode { data: Some(_), .. }) => return Err(bad(format!("{raw:?} is both a file and a folder"))),
                Some(_) => {}
                None => {
                    nodes.insert(k, SaveNode { rel: rel_comps.join("/"), data: None });
                }
            }
        } else {
            if nodes.contains_key(&k) {
                return Err(bad(format!("{raw:?} appears twice (names are not case-sensitive)")));
            }
            if f.size() > budget || f.size() > u32::MAX as u64 {
                return Err(bad(format!("it is larger than {} MiB", MAX_EXPORT_BYTES >> 20)));
            }
            let mut data = Vec::with_capacity(f.size() as usize);
            f.by_ref().take(budget + 1).read_to_end(&mut data)?;
            if data.len() as u64 > budget {
                return Err(bad(format!("it is larger than {} MiB", MAX_EXPORT_BYTES >> 20)));
            }
            budget -= data.len() as u64;
            nodes.insert(k, SaveNode { rel: rel_comps.join("/"), data: Some(data) });
        }
    }
    if !nodes.values().any(|n| n.data.is_some()) {
        return Err(bad("it holds no files".into()));
    }
    Ok(nodes.into_values().collect())
}

impl<D: BlockRead + BlockWrite> XboxHdd<D> {
    /// Replaces `E:\UDATA\<save id>` with the contents of `zip` (Argosy's shape). The folder and
    /// everything in it is swapped as one: what the zip does not hold is removed. It then reads the
    /// result back, and fails if it is not exactly the zip. Nothing is permanent until the caller
    /// commits the disk's transaction, so a failure here is for the caller to roll back.
    pub fn import_zip(&mut self, save_id: &str, zip: &[u8]) -> XboxResult<ImportSummary> {
        if !is_title_id(save_id) {
            return Err(XboxError::Fatx(format!("{save_id:?} is not an 8-digit title id")));
        }
        let nodes = parse_save_zip(save_id, zip)?;
        let bpc = self.fs.bytes_per_cluster();
        let per_dir = bpc / 64;

        // Room, counting what the folder being replaced gives back. Directory growth is allowed for.
        let udata_existing = self.udata()?;
        let existing = match &udata_existing {
            Some(u) => self.fs.find(u.first_cluster, save_id)?,
            None => None,
        };
        let freed = match &existing {
            Some(e) => self.fs.clusters_used(e)?,
            None => 0,
        };
        let mut children: HashMap<String, u64> = HashMap::new();
        let mut need = 2u64; // the title folder and UDATA, if it must be made
        for n in &nodes {
            let parent = n.rel.rsplit_once('/').map_or("", |(p, _)| p).to_ascii_lowercase();
            *children.entry(parent).or_default() += 1;
            need += match &n.data {
                None => 1,
                Some(d) => (d.len() as u64).div_ceil(bpc).max(1),
            };
        }
        need += children.values().map(|&c| c.div_ceil(per_dir).saturating_sub(1)).sum::<u64>();
        self.fs.begin_write()?;
        let free = self.fs.free_clusters()? + freed;
        if free < need {
            return Err(XboxError::Fatx(format!(
                "not enough free space on the Xbox hard disk: the save needs {} KiB and {} KiB is free",
                need * bpc / 1024,
                free * bpc / 1024
            )));
        }

        let ts = FatxTime::now();
        let root = self.fs.root_cluster();
        let udata = match udata_existing {
            Some(u) => u.first_cluster,
            None => self.fs.create_dir(root, "UDATA", ts)?,
        };
        // Keep the name the folder has; a new one is named as the console names them.
        let dir_name = match &existing {
            Some(e) => e.name.clone(),
            None => save_id.to_ascii_lowercase(),
        };
        if existing.is_some() {
            self.fs.remove(udata, &dir_name)?;
        }
        let title = self.fs.create_dir(udata, &dir_name, ts)?;

        let mut dirs: HashMap<String, u32> = HashMap::from([(String::new(), title)]);
        let (mut files, mut folders, mut bytes) = (0, 0, 0u64);
        for n in &nodes {
            let (parent, name) = n.rel.rsplit_once('/').unwrap_or(("", &n.rel));
            let parent_first = dirs[&parent.to_ascii_lowercase()];
            match &n.data {
                None => {
                    let c = self.fs.create_dir(parent_first, name, ts)?;
                    dirs.insert(n.rel.to_ascii_lowercase(), c);
                    folders += 1;
                }
                Some(d) => {
                    self.fs.create_file(parent_first, name, d, ts)?;
                    files += 1;
                    bytes += d.len() as u64;
                }
            }
        }
        self.fs.flush_fat()?;

        // Trust nothing: read it back and compare, name for name and byte for byte.
        let root_name = save_id.to_ascii_uppercase();
        let mut got = Vec::new();
        let mut budget = MAX_EXPORT_BYTES;
        self.collect(title, &root_name, 0, &mut HashSet::from([title]), &mut got, &mut budget)?;
        let got: HashMap<String, Option<Vec<u8>>> = got
            .into_iter()
            .map(|n| match n {
                Node::Dir(p) => (p, None),
                Node::File(p, d) => (p, Some(d)),
            })
            .collect();
        let want: HashMap<String, Option<Vec<u8>>> =
            nodes.into_iter().map(|n| (format!("{root_name}/{}", n.rel), n.data)).collect();
        if got != want {
            return Err(XboxError::Fatx("what was written does not read back as the save it was given".into()));
        }
        Ok(ImportSummary { files, dirs: folders, bytes })
    }
}

/// Whether another program (xemu, most likely) seems to have the image open right now. Reading an
/// image in that state can show half-made changes, so callers should not trust what they read.
pub fn image_in_use(image: &Path) -> Option<String> {
    lock::in_use(image)
}

/// Whether [`import_save`] could write to `image` right now: not in use, and nothing in it that
/// the writer can't handle. Cheap, and changes nothing (except undoing an interrupted write).
pub fn check_writable(image: &Path) -> XboxResult<()> {
    if let Some(why) = lock::in_use(image) {
        return Err(XboxError::Fatx(format!("the disk image is in use: {why}")));
    }
    let hdd = FileHdd::open(image)?;
    match hdd.device().write_blocker() {
        Some(why) => Err(XboxError::Fatx(format!("the disk image {why}"))),
        None => Ok(()),
    }
}

/// The title id a save zip is for: its root folder, when that is eight hex digits (upper-cased).
pub fn zip_title_id(zip: &[u8]) -> Option<String> {
    let mut z = ZipArchive::new(Cursor::new(zip)).ok()?;
    let name = z.by_index(0).ok()?.name().to_string();
    let root = name.split('/').next()?;
    is_title_id(root).then(|| root.to_ascii_uppercase())
}

/// Puts `zip` (a save in Argosy's shape) into the E: partition of the xemu image at `image`, as the
/// folder `UDATA\<save id>`. All or nothing: a failure leaves the image exactly as it was, and so
/// does a crash (the next open of the image undoes it). The image must not be open in xemu.
pub fn import_save(image: &Path, save_id: &str, zip: &[u8]) -> XboxResult<ImportSummary> {
    qcow2::recover_journal(image)?;
    // Held for the whole write: xemu is refused the image meanwhile, and this is refused it if
    // xemu already has it.
    let file = lock::open_exclusive(image)?;
    let mut hdd = XboxHdd::open_e(Qcow2::new(file)?)?;
    hdd.device_mut().begin(Some(&qcow2::journal_path(image)))?;
    match hdd.import_zip(save_id, zip) {
        Ok(summary) => {
            hdd.device_mut().commit()?;
            Ok(summary)
        }
        Err(e) => match hdd.device_mut().rollback() {
            Ok(()) => Err(e),
            Err(r) => Err(XboxError::Fatx(format!(
                "{e}; and undoing it failed too ({r}). The image will be put back the next time it is opened"
            ))),
        },
    }
}
