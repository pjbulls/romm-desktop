//! Save units: what a game's save *is*, on disk and on the wire.
//!
//! RomM stores one file per save and Argosy decides what that file holds through
//! [argosy-sigil](https://github.com/rommapp/argosy-sigil). To sync transparently with Argosy this
//! tool has to produce, and accept, exactly the same bytes, so it asks the same library instead of
//! guessing: PSP is a zip of the game's save folders each under its own name, a PS1 game is one
//! per-game raw card cut out of a shared card, a Game Boy cart with a clock is a zip of `.srm` and
//! `.rtc`, and so on. Sigil never touches the disk itself; this module lists the save root, opens
//! members and writes/removes files for it, exactly as the reference Python binding does.
//!
//! Everything here is synchronous and holds raw pointers only for the duration of one call, so no
//! future in the crate ever holds a non-`Send` value across an `.await`.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{Cursor, Read};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::ptr;

use serde::{Deserialize, Serialize};
use sigil_sys as ffi;

use crate::saves;

/// RomM's `Save.content_hash`: the md5 of a plain file, or for a zip the md5 of the sorted
/// `<entry name>:<entry md5>` lines (directories skipped). Entry names are part of the hash, so a
/// stray `SAVEDATA/` prefix or a manifest entry makes a save unpairable with Argosy's.
pub fn content_hash(bytes: &[u8]) -> String {
    zip_content_hash(bytes).unwrap_or_else(|| saves::md5_bytes(bytes))
}

fn zip_content_hash(bytes: &[u8]) -> Option<String> {
    use md5::{Digest, Md5};
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    let mut entries: Vec<(String, String)> = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).ok()?;
        let name = f.name().to_string();
        if name.ends_with('/') {
            continue;
        }
        let mut h = Md5::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        entries.push((name, hex::encode(h.finalize())));
    }
    // RomM sorts by entry name (Python `sorted` on str = code point order = UTF-8 byte order).
    entries.sort();
    let lines: Vec<String> = entries.into_iter().map(|(n, h)| format!("{n}:{h}")).collect();
    Some(saves::md5_bytes(lines.join("\n").as_bytes()))
}

// ---------------------------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------------------------

/// What Sigil learned about a game from its ROM, kept so the ROM isn't read again on every sync.
/// Argosy persists the same fields (`title_id`, `save_id`, cart features) per game.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// The file this was read from; a different launch path means the ROM changed.
    #[serde(default)]
    pub source: String,
    /// Sigil's platform slug (`gamecube`, `psp`, ...).
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub raw_serial: String,
    #[serde(default)]
    pub save_id: String,
    #[serde(default)]
    pub features: u32,
    #[serde(default)]
    pub n64_header: String,
    #[serde(default)]
    pub n64_md5: String,
    #[serde(default)]
    pub n64_md5_n64: String,
}

/// Systems whose saves Sigil finds by the game's id (a disc id, a title id): with no id there is no
/// way to tell which of the emulator's files are this game's. Cartridge systems go by file name.
pub fn needs_game_id(sigil_platform: &str) -> bool {
    matches!(sigil_platform, "psp" | "psx" | "ps2" | "gamecube" | "wii" | "3ds" | "psvita" | "wiiu" | "ps3" | "switch" | "xbox360")
}

impl Identity {
    pub fn has_id(&self) -> bool {
        !self.title_id.is_empty() || !self.save_id.is_empty()
    }
}

/// RomM's platform slug → the one Sigil takes. Sigil already knows most RomM names (`ngc`, `dc`,
/// `sfam`, ...); `new-nintendo-3ds` it reads as "unknown", so it's normalised here.
pub fn platform_slug(romm_slug: &str) -> String {
    match romm_slug {
        "new-nintendo-3ds" | "n3ds" => "3ds",
        "ngc" | "gc" => "gamecube",
        "dc" => "dreamcast",
        "sfam" => "snes",
        "vita" => "psvita",
        other => other,
    }
    .to_string()
}

fn put(dst: &mut [c_char], s: &str) {
    let n = s.len().min(dst.len() - 1);
    for (d, b) in dst.iter_mut().zip(s.as_bytes()[..n].iter()) {
        *d = *b as c_char;
    }
    dst[n] = 0;
}

fn get(src: &[c_char]) -> String {
    // Every Sigil buffer is NUL-terminated; ours are zero-initialised first.
    unsafe { CStr::from_ptr(src.as_ptr()) }.to_string_lossy().into_owned()
}

#[cfg(unix)]
fn path_cstring(p: &Path) -> Option<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(p.as_os_str().as_bytes()).ok()
}
#[cfg(not(unix))]
fn path_cstring(p: &Path) -> Option<CString> {
    CString::new(p.to_str()?).ok()
}

fn platform_enum(slug: &str) -> ffi::sigil_platform {
    match CString::new(slug) {
        Ok(c) if !slug.is_empty() => unsafe { ffi::sigil_platform_from_slug(c.as_ptr()) },
        _ => ffi::SIGIL_PLATFORM_AUTO,
    }
}

/// Reads the game's ids out of the ROM. Never fails: a ROM Sigil can't read (or a system with no
/// ids, like SNES) gives an identity with no ids, which Sigil's layouts treat as "use the file
/// name", the same fallback Argosy takes.
pub fn identify(rom: &Path, romm_platform: &str) -> Identity {
    let platform = platform_slug(romm_platform);
    let mut id = Identity { source: rom.to_string_lossy().into_owned(), platform: platform.clone(), ..Default::default() };
    let Some(path) = path_cstring(&resolve_playlist(rom)) else { return id };

    let mut opts: ffi::sigil_options = unsafe { std::mem::zeroed() };
    opts.struct_version = 1; // SIGIL_OPTIONS_V1
    opts.flags = 1; // SIGIL_FLAG_FILENAME_FALLBACK
    let mut out: ffi::sigil_result = unsafe { std::mem::zeroed() };
    out.struct_version = ffi::SIGIL_RESULT_V4;
    let rc = unsafe { ffi::sigil_extract_from_path(path.as_ptr(), platform_enum(&platform), &opts, &mut out) };
    if rc != ffi::SIGIL_OK {
        tracing::debug!("sigil could not identify {}: {}", rom.display(), strerror(rc));
        return id;
    }
    id.title_id = get(&out.title_id);
    id.raw_serial = get(&out.raw_serial);
    id.save_id = get(&out.save_id);
    id.features = out.features;
    id.n64_header = get(&out.n64_header);
    id.n64_md5 = get(&out.n64_md5);
    id.n64_md5_n64 = get(&out.n64_md5_n64);
    id
}

/// Whether `p` is a `.m3u` playlist.
pub fn is_playlist(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("m3u"))
}

/// A `.m3u` playlist names the discs of one game, in order; this is its first disc, when that file
/// exists. Anything else (not a playlist, unreadable, empty, or naming a disc that is not there)
/// comes back unchanged, so a caller can tell by comparing.
///
/// Playlists in the wild are not tidy: a byte-order mark, Windows line ends and `\` separators all
/// occur, and a stray byte that is not UTF-8 (in a comment, say) must not discard the whole file.
pub fn resolve_playlist(p: &Path) -> PathBuf {
    if !is_playlist(p) {
        return p.to_path_buf();
    }
    let Ok(bytes) = std::fs::read(p) else { return p.to_path_buf() };
    let text = String::from_utf8_lossy(&bytes);
    let first = text.trim_start_matches('\u{feff}').lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#'));
    let Some(line) = first else { return p.to_path_buf() };
    let dir = p.parent().unwrap_or(Path::new(""));
    let as_written = dir.join(line);
    if as_written.is_file() {
        return as_written;
    }
    // A playlist made on Windows names `Sub\Disc 1.iso`; only convert when the name has no such
    // character of its own (a backslash is legal in a Unix file name, so as-written wins above).
    let portable = dir.join(line.replace('\\', "/"));
    if portable.is_file() {
        return portable;
    }
    p.to_path_buf()
}

// ---------------------------------------------------------------------------------------------
// Routes and results
// ---------------------------------------------------------------------------------------------

/// Where one game's saves live for one emulator, in Sigil's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Sigil layout id: a libretro core's name, or a standalone id such as `ppsspp_standalone`.
    pub layout: String,
    /// Sigil platform slug.
    pub platform: String,
    /// The folder the emulator keeps saves in; Sigil cuts it down to the emulator's own base.
    pub root: PathBuf,
    /// Emulator settings the layout's file names depend on (`Card1Type`, `Slot1_Filename`, ...).
    pub options: Vec<(String, String)>,
}

impl Route {
    /// Key of this route's sync state. Argosy keys it the same way (platform, layout, root).
    pub fn state_key(&self) -> String {
        format!("{}|{}|{}", self.platform, self.layout, self.root.to_string_lossy())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// One raw file.
    Single,
    /// A flat zip, members at the root.
    Multi,
    /// A zip of the game's save folder(s), `<folder>/<file>`.
    Folder,
}

/// One game's saves gathered into the file that travels to RomM.
#[derive(Debug, Clone)]
pub struct Unit {
    /// The file name Sigil gives the unit; only its extension matters on upload.
    pub artifact: String,
    pub shape: Shape,
    pub data: Vec<u8>,
    /// RomM's `content_hash` of `data`.
    pub content_hash: String,
    /// Hash of the saves themselves, unmoved by timestamps and placement.
    pub identity_hash: String,
    /// Differs from what the last synced state saw.
    pub changed: bool,
    /// Store this once the unit has reached the server (or the server already had it).
    pub state: Vec<u8>,
    /// The emulator overwrote what the last restore wrote (a core does when it unloads).
    pub restore_again: bool,
}

impl Unit {
    /// Extension of the file RomM stores: `zip` for a zip unit, else the member's own.
    pub fn extension(&self) -> String {
        Path::new(&self.artifact).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Restored {
    pub state: Vec<u8>,
    /// The unit ended in the hardcore marker old Argosy builds appended; it is left out of the saves.
    pub hardcore_marker: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    NotFound,
    Conflict,
    NoSpace,
    Damaged,
    Ambiguous,
    NoTarget,
    Exists,
    Region,
    Uncollected,
    UnsupportedFormat,
    Io,
    Other,
}

/// Why Sigil refused. `problem` names the file, save or candidates at fault.
#[derive(Debug, Clone)]
pub struct UnitError {
    pub code: i32,
    pub kind: ErrorKind,
    pub message: String,
    pub problem: String,
    pub blocks_short: u32,
}

impl UnitError {
    fn from_code(code: c_int, problem: String, blocks_short: u32) -> Self {
        let kind = match code {
            ffi::SIGIL_ERR_NOT_FOUND => ErrorKind::NotFound,
            ffi::SIGIL_ERR_CONFLICT => ErrorKind::Conflict,
            ffi::SIGIL_ERR_NO_SPACE => ErrorKind::NoSpace,
            ffi::SIGIL_ERR_DAMAGED => ErrorKind::Damaged,
            ffi::SIGIL_ERR_AMBIGUOUS => ErrorKind::Ambiguous,
            ffi::SIGIL_ERR_NO_TARGET => ErrorKind::NoTarget,
            ffi::SIGIL_ERR_EXISTS => ErrorKind::Exists,
            ffi::SIGIL_ERR_REGION => ErrorKind::Region,
            ffi::SIGIL_ERR_UNCOLLECTED => ErrorKind::Uncollected,
            ffi::SIGIL_ERR_UNSUPPORTED_FORMAT => ErrorKind::UnsupportedFormat,
            ffi::SIGIL_ERR_IO => ErrorKind::Io,
            _ => ErrorKind::Other,
        };
        UnitError { code, kind, message: strerror(code), problem, blocks_short }
    }

    pub fn is_not_found(&self) -> bool {
        self.kind == ErrorKind::NotFound
    }
}

impl std::fmt::Display for UnitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = self.problem.replace('\n', ", ");
        match self.kind {
            ErrorKind::NoSpace if self.blocks_short > 0 => {
                write!(f, "the save doesn't fit on the memory card ({} blocks short): {p}", self.blocks_short)
            }
            ErrorKind::NoSpace => write!(f, "the save doesn't fit on the memory card: {p}"),
            ErrorKind::Ambiguous => {
                write!(f, "can't tell which file is the emulator's memory card (set it in the emulator's settings): {p}")
            }
            ErrorKind::Damaged => write!(f, "a save file is damaged, so nothing was changed: {p}"),
            ErrorKind::NoTarget => write!(f, "the emulator has no place for part of this save: {p}"),
            ErrorKind::Exists => write!(f, "restoring would overwrite another game's save: {p}"),
            _ if self.problem.is_empty() => write!(f, "{}", self.message),
            _ => write!(f, "{}: {p}", self.message),
        }
    }
}

impl std::error::Error for UnitError {}

impl From<UnitError> for crate::error::Error {
    fn from(e: UnitError) -> Self {
        crate::error::Error::msg(e.to_string())
    }
}

fn strerror(code: c_int) -> String {
    let p = unsafe { ffi::sigil_strerror(code) };
    if p.is_null() {
        return format!("sigil error {code}");
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Sigil's own version string, for diagnostics.
pub fn sigil_version() -> String {
    let p = unsafe { ffi::sigil_version() };
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------------------------
// The save root: base, listing, files
// ---------------------------------------------------------------------------------------------

const SUBDIR_LIST_DEPTH: u32 = 12;
const BASE_SEARCH_DEPTH: u32 = 5;
const PATH_BUF: usize = 4096;

fn layout_subdirs(layout: &CStr) -> Vec<String> {
    let mut out: [*const c_char; 16] = [ptr::null(); 16];
    let n = unsafe { ffi::sigil_save_layout_subdirs(layout.as_ptr(), out.as_mut_ptr(), out.len()) };
    out.iter().take(n.min(16)).filter(|p| !p.is_null()).map(|p| unsafe { CStr::from_ptr(*p) }.to_string_lossy().into_owned()).collect()
}

fn layout_top(layout: &CStr) -> Option<String> {
    let p = unsafe { ffi::sigil_save_layout_top(layout.as_ptr()) };
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// The emulator's base folder for `path` (a PSP memory stick's folder above `PSP/`, Azahar's above
/// `sdmc/`, ...) when `path` lies inside it, else `path` itself; plus the profile folder it lies in.
fn save_base(layout: &CStr, path: &Path) -> (PathBuf, Option<String>) {
    let Some(p) = path_cstring(path) else { return (path.to_path_buf(), None) };
    let mut base = vec![0 as c_char; PATH_BUF];
    let mut profile = vec![0 as c_char; ffi::SIGIL_PROFILE_ID_MAX];
    let rc = unsafe {
        ffi::sigil_save_base(layout.as_ptr(), p.as_ptr(), base.as_mut_ptr(), base.len(), profile.as_mut_ptr(), profile.len())
    };
    if rc != ffi::SIGIL_OK {
        return (path.to_path_buf(), None);
    }
    let prof = get(&profile);
    (PathBuf::from(get(&base)), (!prof.is_empty()).then_some(prof))
}

fn is_dir(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

/// Root-relative folders under `root` that hold the layout's top folder (a card-less emulator tree).
fn bases_below(root: &Path, top: &str) -> Vec<String> {
    fn walk(dir: &Path, rel: &str, depth: u32, top: &str, out: &mut Vec<String>) {
        if depth == 0 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut dirs: Vec<_> = rd.flatten().filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)).collect();
        dirs.sort_by_key(|e| e.file_name());
        for e in dirs {
            let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
            if name == top {
                if !rel.is_empty() {
                    out.push(rel.to_string());
                }
            } else {
                let next = if rel.is_empty() { name } else { format!("{rel}/{name}") };
                walk(&e.path(), &next, depth - 1, top, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", BASE_SEARCH_DEPTH, top, &mut out);
    out
}

fn list_recursive(dir: &Path, rel: &str, depth: u32, out: &mut Vec<String>) {
    if depth == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
        let rel_path = format!("{rel}/{name}");
        let Ok(meta) = std::fs::metadata(e.path()) else { continue };
        if meta.is_file() {
            out.push(rel_path);
        } else if meta.is_dir() {
            list_recursive(&e.path(), &rel_path, depth - 1, out);
        }
    }
}

/// Root-relative paths of the files directly in `root` plus everything under the layout's subfolders.
fn list_save_root(root: &Path, layout: &CStr) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| std::fs::metadata(e.path()).map(|m| m.is_file()).unwrap_or(false))
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect();
        names.sort();
        out.extend(names);
    }
    let top = layout_top(layout);
    let mut bases = vec![String::new()];
    if let Some(top) = &top {
        if !is_dir(&root.join(top)) {
            let found = bases_below(root, top);
            if !found.is_empty() {
                bases = found;
            }
        }
    }
    for base in &bases {
        for sub in layout_subdirs(layout) {
            let rel = if base.is_empty() { sub.clone() } else { format!("{base}/{sub}") };
            list_recursive(&root.join(&rel), &rel, SUBDIR_LIST_DEPTH, &mut out);
        }
    }
    out
}

struct FsCtx {
    root: PathBuf,
}

unsafe extern "C" fn open_cb(ctx: *mut c_void, rel: *const c_char) -> *mut ffi::sigil_io {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let ctx = &*(ctx as *const FsCtx);
        let rel = CStr::from_ptr(rel).to_str().ok()?;
        let path = path_cstring(&ctx.root.join(rel))?;
        Some(ffi::sigil_io_open_file(path.as_ptr()))
    }));
    match r {
        Ok(Some(io)) => io,
        _ => ptr::null_mut(),
    }
}

unsafe extern "C" fn write_cb(ctx: *mut c_void, rel: *const c_char, data: *const u8, len: usize) -> c_int {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let ctx = &*(ctx as *const FsCtx);
        let rel = CStr::from_ptr(rel).to_str().ok()?;
        let bytes: &[u8] = if len == 0 { &[] } else { std::slice::from_raw_parts(data, len) };
        saves::write_atomic(&ctx.root.join(rel), bytes).ok()
    }));
    if matches!(r, Ok(Some(()))) {
        0
    } else {
        -1
    }
}

unsafe extern "C" fn remove_cb(ctx: *mut c_void, rel: *const c_char) -> c_int {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let ctx = &*(ctx as *const FsCtx);
        let rel = CStr::from_ptr(rel).to_str().ok()?;
        let path = ctx.root.join(rel.trim_end_matches('/'));
        let res = if rel.ends_with('/') { std::fs::remove_dir(&path) } else { std::fs::remove_file(&path) };
        res.ok()
    }));
    if matches!(r, Ok(Some(()))) {
        0
    } else {
        -1
    }
}

// ---------------------------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------------------------

/// Owns every buffer a `sigil_sync_request` points at, for the length of one call.
struct Call {
    _layout: CString,
    _platform: CString,
    _content: CString,
    _root: CString,
    _profile: Option<CString>,
    _opt_strings: Vec<CString>,
    opts: Vec<ffi::sigil_save_option>,
    _listing: Vec<CString>,
    listing_ptrs: Vec<*const c_char>,
    _claimed: Vec<CString>,
    claimed_ptrs: Vec<*const c_char>,
    result: Box<ffi::sigil_result>,
    fs: Box<FsCtx>,
    state: Vec<u8>,
    layout_ptr: *const c_char,
    platform_ptr: *const c_char,
    content_ptr: *const c_char,
    root_ptr: *const c_char,
    profile_ptr: *const c_char,
}

impl Call {
    fn new(route: &Route, id: &Identity, content_name: &str, state: Option<&[u8]>, claimed: &[String]) -> Option<Call> {
        let layout = CString::new(route.layout.as_str()).ok()?;
        let platform = CString::new(route.platform.as_str()).ok()?;
        let content = CString::new(content_name).ok()?;

        let (root, implied_profile) = save_base(&layout, &route.root);
        let root_c = path_cstring(&root)?;
        let listing: Vec<CString> = list_save_root(&root, &layout).into_iter().filter_map(|p| CString::new(p).ok()).collect();
        // Sigil rejects a NULL `listing` even when it is empty (a root with no files yet), so the
        // array always carries a trailing NULL and is never empty; `listing_count` stays the real count.
        let mut listing_ptrs: Vec<*const c_char> = listing.iter().map(|c| c.as_ptr()).collect();
        listing_ptrs.push(ptr::null());

        let mut opt_strings = Vec::new();
        let mut opts = Vec::new();
        for (k, v) in &route.options {
            let (k, v) = (CString::new(k.as_str()).ok()?, CString::new(v.as_str()).ok()?);
            opts.push(ffi::sigil_save_option { key: k.as_ptr(), value: v.as_ptr() });
            opt_strings.push(k);
            opt_strings.push(v);
        }

        let claimed_c: Vec<CString> = claimed.iter().filter_map(|n| CString::new(n.as_str()).ok()).collect();
        let claimed_ptrs: Vec<*const c_char> = claimed_c.iter().map(|c| c.as_ptr()).collect();

        let mut result: Box<ffi::sigil_result> = Box::new(unsafe { std::mem::zeroed() });
        result.struct_version = ffi::SIGIL_RESULT_V4;
        put(&mut result.title_id, &id.title_id);
        put(&mut result.raw_serial, &id.raw_serial);
        put(&mut result.save_id, &id.save_id);
        result.platform = platform_enum(&route.platform);
        result.features = id.features;
        put(&mut result.n64_header, &id.n64_header);
        put(&mut result.n64_md5, &id.n64_md5);
        put(&mut result.n64_md5_n64, &id.n64_md5_n64);

        let profile = implied_profile.and_then(|p| CString::new(p).ok());
        let fs = Box::new(FsCtx { root });
        Some(Call {
            layout_ptr: layout.as_ptr(),
            platform_ptr: platform.as_ptr(),
            content_ptr: content.as_ptr(),
            root_ptr: root_c.as_ptr(),
            profile_ptr: profile.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
            _layout: layout,
            _platform: platform,
            _content: content,
            _root: root_c,
            _profile: profile,
            _opt_strings: opt_strings,
            _claimed: claimed_c,
            claimed_ptrs,
            opts,
            _listing: listing,
            listing_ptrs,
            result,
            fs,
            state: state.map(<[u8]>::to_vec).unwrap_or_default(),
        })
    }

    /// Valid only while `self` is alive and not moved out of (every pointer targets a heap buffer
    /// owned by a field, so moving `Call` itself is fine; mutating the buffers is not).
    fn request(&self) -> ffi::sigil_sync_request {
        let mut r: ffi::sigil_sync_request = unsafe { std::mem::zeroed() };
        r.struct_version = ffi::SIGIL_SYNC_REQUEST_V1;
        r.save.struct_version = ffi::SIGIL_SAVE_REQUEST_V1;
        r.save.layout = self.layout_ptr;
        r.save.platform = self.platform_ptr;
        r.save.content_path = self.content_ptr;
        r.save.result = &*self.result;
        r.save.features = self.result.features;
        r.save.options = if self.opts.is_empty() { ptr::null() } else { self.opts.as_ptr() };
        r.save.option_count = self.opts.len();
        r.save.listing = self.listing_ptrs.as_ptr();
        r.save.listing_count = self.listing_ptrs.len() - 1;
        r.save.open = Some(open_cb);
        r.save.open_ctx = &*self.fs as *const FsCtx as *mut c_void;
        r.save.root_path = self.root_ptr;
        r.save.profile = self.profile_ptr;
        r.mode = ffi::SIGIL_SYNC_UNMANAGED;
        r.state = if self.state.is_empty() { ptr::null() } else { self.state.as_ptr() };
        r.state_len = self.state.len();
        r.claimed = if self.claimed_ptrs.is_empty() { ptr::null() } else { self.claimed_ptrs.as_ptr() };
        r.claimed_count = self.claimed_ptrs.len();
        r.write = Some(write_cb);
        r.write_ctx = &*self.fs as *const FsCtx as *mut c_void;
        r.remove = Some(remove_cb);
        r
    }
}

/// Owns a `sigil_sync_result` and frees it on drop, so every early return is leak-free.
struct Out(*mut ffi::sigil_sync_result);
impl Drop for Out {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { ffi::sigil_sync_result_free(self.0) };
        }
    }
}

fn bytes(p: *const u8, len: usize) -> Vec<u8> {
    if p.is_null() || len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(p, len) }.to_vec()
    }
}

fn error_from(rc: c_int, out: &Out) -> UnitError {
    let (problem, blocks) = if out.0.is_null() {
        (String::new(), 0)
    } else {
        let r = unsafe { &*out.0 };
        (get(&r.problem), r.blocks_short)
    };
    UnitError::from_code(rc, problem, blocks)
}

/// Systems whose shared volumes hold saves no rule can give a game (Sega CD's backup RAM,
/// Dreamcast's VMU, Saturn's): the saves carry no game id. Until claimed they are `unowned`.
pub fn has_unowned_saves(sigil_platform: &str) -> bool {
    matches!(sigil_platform, "segacd" | "dreamcast" | "saturn")
}

/// What a collect found: the game's unit, and the names of saves on the same shared volume that
/// Sigil could give to no game.
#[derive(Debug, Clone, Default)]
pub struct Collected {
    pub unit: Option<Unit>,
    /// Real saves of a game Sigil couldn't identify. A client claims one for the game just played by
    /// passing its name back in `claimed`; the state Sigil returns remembers the owner from then on.
    pub unowned: Vec<String>,
}

/// Gathers one game's saves into the unit that travels to RomM. `Ok(None)` when the game has none.
///
/// `content_name` is the file name of what the emulator loads (Sigil names RetroArch-style saves
/// after its stem). `state` is what the last successful sync stored for this route.
pub fn collect(route: &Route, id: &Identity, content_name: &str, state: Option<&[u8]>) -> Result<Option<Unit>, UnitError> {
    Ok(collect_claiming(route, id, content_name, state, &[])?.unit)
}

/// `collect`, also taking ownership of the `claimed` unowned saves for this game, and reporting the
/// unowned ones that remain.
pub fn collect_claiming(route: &Route, id: &Identity, content_name: &str, state: Option<&[u8]>, claimed: &[String]) -> Result<Collected, UnitError> {
    if !is_dir(&route.root) {
        return Ok(Collected::default());
    }
    let Some(call) = Call::new(route, id, content_name, state, claimed) else { return Ok(Collected::default()) };
    let req = call.request();
    let mut out = Out(ptr::null_mut());
    let rc = unsafe { ffi::sigil_collect(&req, &mut out.0) };
    if rc != ffi::SIGIL_OK {
        let e = error_from(rc, &out);
        return if e.is_not_found() { Ok(Collected::default()) } else { Err(e) };
    }
    let r = unsafe { &*out.0 };
    let unowned: Vec<String> = if r.unowned.is_null() { Vec::new() } else { (0..r.unowned_count).map(|i| get(unsafe { &*r.unowned.add(i) })).collect() };
    if r.restore_again != 0 {
        // Argosy only notes this too: the emulator overwrote what the last restore wrote.
        tracing::warn!("{}: the emulator overwrote what was just restored", route.layout);
    }
    let data = bytes(r.data, r.len);
    if data.is_empty() {
        return Ok(Collected { unit: None, unowned });
    }
    let shape = match r.shape {
        ffi::SIGIL_SAVE_SHAPE_MULTI => Shape::Multi,
        ffi::SIGIL_SAVE_SHAPE_FOLDER => Shape::Folder,
        _ => Shape::Single,
    };
    let unit = Unit {
        artifact: get(&r.artifact),
        shape,
        content_hash: get(&r.content_hash),
        identity_hash: get(&r.identity_hash),
        changed: r.changed != 0,
        state: bytes(r.state, r.state_len),
        restore_again: r.restore_again != 0,
        data,
    };
    Ok(Collected { unit: Some(unit), unowned })
}

/// Puts a unit (from RomM, from any client) back into the emulator's files, replacing only this
/// game's saves. Sigil reads the result back to verify before returning.
pub fn restore(route: &Route, id: &Identity, content_name: &str, unit: &[u8], state: Option<&[u8]>) -> Result<Restored, UnitError> {
    let Some(call) = Call::new(route, id, content_name, state, &[]) else {
        return Err(UnitError::from_code(ffi::SIGIL_ERR_INVALID_ARG, "unusable path or name".into(), 0));
    };
    let mut req = call.request();
    req.overwrite_local = 1;
    let mut out = Out(ptr::null_mut());
    let rc = unsafe { ffi::sigil_restore(&req, unit.as_ptr(), unit.len(), &mut out.0) };
    if rc != ffi::SIGIL_OK {
        return Err(error_from(rc, &out));
    }
    let r = unsafe { &*out.0 };
    Ok(Restored { state: bytes(r.state, r.state_len), hardcore_marker: r.hardcore_marker != 0 })
}

/// When the files holding this game's saves were last written: the newest modification time among
/// the files Sigil names as the game's (shared cards included). RomM compares it with its own
/// timestamps, so it has to be a real one, not "now".
pub fn newest_mtime(route: &Route, id: &Identity, content_name: &str) -> Option<std::time::SystemTime> {
    if !is_dir(&route.root) {
        return None;
    }
    let call = Call::new(route, id, content_name, None, &[])?;
    let req = call.request();
    let mut unit: *mut ffi::sigil_save_unit = ptr::null_mut();
    let rc = unsafe { ffi::sigil_save_resolve(&req.save, &mut unit) };
    if rc != ffi::SIGIL_OK || unit.is_null() {
        return None;
    }
    let mut newest = None;
    unsafe {
        let u = &*unit;
        for i in 0..u.member_count {
            let m = &*u.members.add(i);
            if m.present == 0 {
                continue;
            }
            let Ok(meta) = std::fs::metadata(call.fs.root.join(get(&m.path))) else { continue };
            if let Ok(t) = meta.modified() {
                newest = newest.max(Some(t));
            }
        }
        ffi::sigil_save_unit_free(unit);
    }
    newest
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        for (n, d) in entries {
            if n.ends_with('/') {
                w.add_directory(*n, o).unwrap();
            } else {
                w.start_file(*n, o).unwrap();
                w.write_all(d).unwrap();
            }
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn plain_files_hash_as_md5() {
        assert_eq!(content_hash(b"hello"), "5d41402abc4b2a76b9719d911017c592");
    }

    #[test]
    fn zips_hash_by_entry_names_and_contents_not_bytes() {
        let a = zip_of(&[("G/DATA.BIN", b"aaa"), ("G/PARAM.SFO", b"bbb")]);
        // Different entry order and an extra directory entry: same content hash.
        let b = zip_of(&[("G/", b""), ("G/PARAM.SFO", b"bbb"), ("G/DATA.BIN", b"aaa")]);
        assert_ne!(a, b);
        assert_eq!(content_hash(&a), content_hash(&b));
        // The entry name is part of the hash: a stray prefix changes it.
        let prefixed = zip_of(&[("SAVEDATA/G/DATA.BIN", b"aaa"), ("SAVEDATA/G/PARAM.SFO", b"bbb")]);
        assert_ne!(content_hash(&a), content_hash(&prefixed));
        // And it is not the md5 of the zip's bytes.
        assert_ne!(content_hash(&a), saves::md5_bytes(&a));
    }

    #[test]
    fn hash_matches_romms_algorithm_for_a_known_zip() {
        // md5("DATA.BIN:<md5 of 'aaa'>\nPARAM.SFO:<md5 of 'bbb'>"), computed with RomM's
        // hash_zip_contents in Python.
        let z = zip_of(&[("PARAM.SFO", b"bbb"), ("DATA.BIN", b"aaa")]);
        let lines = format!("DATA.BIN:{}\nPARAM.SFO:{}", saves::md5_bytes(b"aaa"), saves::md5_bytes(b"bbb"));
        assert_eq!(content_hash(&z), saves::md5_bytes(lines.as_bytes()));
    }

    #[test]
    fn only_systems_that_find_saves_by_game_id_need_one() {
        for p in ["psp", "psx", "ps2", "gamecube", "wii", "3ds", "xbox360"] {
            assert!(needs_game_id(p), "{p}");
        }
        for p in ["gb", "gbc", "n64", "dreamcast", "segacd", "snes"] {
            assert!(!needs_game_id(p), "{p}");
        }
        assert!(!Identity::default().has_id());
        assert!(Identity { save_id: "X".into(), ..Default::default() }.has_id());
    }

    #[test]
    fn romm_platform_slugs_are_normalised() {
        assert_eq!(platform_slug("new-nintendo-3ds"), "3ds");
        assert_eq!(platform_slug("ngc"), "gamecube");
        assert_eq!(platform_slug("dc"), "dreamcast");
        assert_eq!(platform_slug("psp"), "psp");
    }

    #[test]
    fn identity_survives_the_state_file() {
        let id = Identity { source: "/r/a.iso".into(), platform: "psp".into(), title_id: "ULUS10064".into(), save_id: "ULUS10064".into(), ..Default::default() };
        let back: Identity = serde_json::from_str(&serde_json::to_string(&id).unwrap()).unwrap();
        assert_eq!(id, back);
        // An old state file has none of these fields.
        assert_eq!(serde_json::from_str::<Identity>("{}").unwrap(), Identity::default());
    }

    #[test]
    fn m3u_identifies_from_its_first_disc() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("disc1.chd"), "x").unwrap();
        std::fs::write(t.path().join("game.m3u"), "# comment\n\ndisc1.chd\ndisc2.chd\n").unwrap();
        assert_eq!(resolve_playlist(&t.path().join("game.m3u")), t.path().join("disc1.chd"));
        // A playlist naming a missing disc falls back to the playlist itself.
        std::fs::write(t.path().join("bad.m3u"), "gone.chd\n").unwrap();
        assert_eq!(resolve_playlist(&t.path().join("bad.m3u")), t.path().join("bad.m3u"));
    }

    fn disc_dir(files: &[&str]) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        for f in files {
            let p = t.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        t
    }

    /// The playlist's own order decides which disc is first, not the file names' order.
    #[test]
    fn the_playlists_order_decides_the_first_disc() {
        let t = disc_dir(&["MyGame (Disk 1).iso", "MyGame (Disk 2).iso"]);
        std::fs::write(t.path().join("a.m3u"), "MyGame (Disk 2).iso\nMyGame (Disk 1).iso\n").unwrap();
        assert_eq!(resolve_playlist(&t.path().join("a.m3u")), t.path().join("MyGame (Disk 2).iso"));
    }

    #[test]
    fn untidy_playlists_still_resolve() {
        let t = disc_dir(&["Disk 1.iso", "Sub/Disk 1.iso"]);
        let m3u = t.path().join("g.m3u");
        // A byte-order mark, Windows line ends, a comment and blank lines first.
        std::fs::write(&m3u, "\u{feff}#EXTM3U\r\n\r\n# the first disc\r\nDisk 1.iso\r\nDisk 2.iso\r\n").unwrap();
        assert_eq!(resolve_playlist(&m3u), t.path().join("Disk 1.iso"));
        // A playlist made on Windows.
        std::fs::write(&m3u, "Sub\\Disk 1.iso\r\n").unwrap();
        assert_eq!(resolve_playlist(&m3u), t.path().join("Sub/Disk 1.iso"));
        // A stray byte that is not UTF-8 in a comment does not discard the playlist.
        std::fs::write(&m3u, b"# made by \xff\xfe\nDisk 1.iso\n").unwrap();
        assert_eq!(resolve_playlist(&m3u), t.path().join("Disk 1.iso"));
        // Names of the disc in another case, as written, are the file system's business: as-written wins.
        std::fs::write(&m3u, "Disk 1.iso").unwrap();
        assert_eq!(resolve_playlist(&m3u), t.path().join("Disk 1.iso"));
    }

    #[test]
    fn a_playlist_that_names_nothing_usable_comes_back_unchanged() {
        let t = disc_dir(&["Disk 1.iso"]);
        let m3u = t.path().join("g.m3u");
        for body in ["", "# only a comment\n", "gone.iso\n"] {
            std::fs::write(&m3u, body).unwrap();
            assert_eq!(resolve_playlist(&m3u), m3u, "{body:?}");
        }
        // And something that is not a playlist at all is left alone, even if it is a file.
        assert_eq!(resolve_playlist(&t.path().join("Disk 1.iso")), t.path().join("Disk 1.iso"));
        assert_eq!(resolve_playlist(&t.path().join("missing.m3u")), t.path().join("missing.m3u"));
        assert!(is_playlist(Path::new("A.M3U")) && !is_playlist(Path::new("a.iso")));
    }
}
