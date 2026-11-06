//! Fixtures shared by the integration tests: real-format save files, so Sigil (which validates
//! what it reads, as Argosy's does) accepts them.
#![allow(dead_code)]

use std::path::Path;

/// RomM's `content_hash` of the PSP unit for `psp_memory_stick`, as returned by argosy-sigil's
/// `collect` in the Argosy-pinned build (the reference run), recomputed with RomM's own algorithm.
pub const PSP_HASH: &str = "fa53bdf2019e7a46ab7b2b16b22cf712";
/// Same, for `dolphin_gc_folder` (Melee's two saves).
pub const GC_HASH: &str = "3ca1d87afcdb9dd507f5c31aba7a45ab";

/// A minimal PSF (`PARAM.SFO`) with UTF-8 string entries, laid out like the console writes it.
pub fn sfo(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut keys = Vec::new();
    let mut key_offsets = Vec::new();
    for (k, _) in entries {
        key_offsets.push(keys.len() as u16);
        keys.extend_from_slice(k.as_bytes());
        keys.push(0);
    }
    while keys.len() % 4 != 0 {
        keys.push(0);
    }
    let mut data = Vec::new();
    let mut data_offsets = Vec::new();
    let mut lens = Vec::new();
    for (_, v) in entries {
        data_offsets.push(data.len() as u32);
        data.extend_from_slice(v.as_bytes());
        data.push(0);
        lens.push(v.len() as u32 + 1);
    }
    let mut index = Vec::new();
    for i in 0..entries.len() {
        index.extend_from_slice(&key_offsets[i].to_le_bytes());
        index.extend_from_slice(&0x0204u16.to_le_bytes());
        index.extend_from_slice(&lens[i].to_le_bytes());
        index.extend_from_slice(&lens[i].to_le_bytes());
        index.extend_from_slice(&data_offsets[i].to_le_bytes());
    }
    let key_start = 20 + index.len() as u32;
    let data_start = key_start + keys.len() as u32;
    let mut out = b"\0PSF".to_vec();
    for v in [0x101u32, key_start, data_start, entries.len() as u32] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend(index);
    out.extend(keys);
    out.extend(data);
    out
}

/// A PSP save folder's PARAM.SFO: it carries the two keys that make PPSSPP treat it as save data.
pub fn psp_save_sfo() -> Vec<u8> {
    sfo(&[("SAVEDATA_PARAMS", "xxxxxxxx"), ("SAVEDATA_FILE_LIST", "yyyyyyyy"), ("TITLE", "Save")])
}

/// The PARAM.SFO of installed game data (a PSN/minis install), which is *not* a save.
pub fn psp_install_sfo() -> Vec<u8> {
    sfo(&[("TITLE", "Installed game data"), ("DISC_ID", "ULUS10064")])
}

pub fn write(root: &Path, rel: &str, data: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

/// The exact fixture the Argosy/Sigil reference run used (see the PSP oracle in the repo notes):
/// two save folders of one game, an installed-data folder with the same id prefix, and another
/// game's save.
pub fn psp_memory_stick(ms: &Path) {
    write(ms, "PSP/SAVEDATA/ULUS10064DATA00/PARAM.SFO", &psp_save_sfo());
    write(ms, "PSP/SAVEDATA/ULUS10064DATA00/DATA.BIN", &[b'A'; 300]);
    write(ms, "PSP/SAVEDATA/ULUS10064DATA00/ICON0.PNG", &b"png".repeat(50));
    write(ms, "PSP/SAVEDATA/ULUS10064SETTINGS/PARAM.SFO", &psp_save_sfo());
    write(ms, "PSP/SAVEDATA/ULUS10064SETTINGS/OPT.BIN", &[b'B'; 120]);
    write(ms, "PSP/SAVEDATA/ULUS10064/PARAM.SFO", &psp_install_sfo());
    write(ms, "PSP/SAVEDATA/ULUS10064/DATA.BIN", &[b'I'; 500]);
    write(ms, "PSP/SAVEDATA/ULES00001DATA00/PARAM.SFO", &psp_save_sfo());
    write(ms, "PSP/SAVEDATA/ULES00001DATA00/DATA.BIN", &[b'Z'; 100]);
}

pub fn zip_entries(bytes: &[u8]) -> Vec<String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut v: Vec<String> = (0..z.len()).map(|i| z.by_index(i).unwrap().name().to_string()).collect();
    v.sort();
    v
}

// ---- GameCube ---------------------------------------------------------------------------------

/// A `.gci` file: 0x40-byte header (as Dolphin's GCI folder stores it) plus `blocks` × 8 KiB.
pub fn gci(gamecode: &[u8; 4], maker: &[u8; 2], name: &str, blocks: u16, fill: u8) -> Vec<u8> {
    let mut h = vec![0u8; 0x40];
    h[0..4].copy_from_slice(gamecode);
    h[4..6].copy_from_slice(maker);
    h[6] = 0xFF;
    h[8..8 + name.len()].copy_from_slice(name.as_bytes());
    h[0x28..0x2C].copy_from_slice(&0x1234_5678u32.to_be_bytes());
    h[0x34] = 4;
    h[0x36..0x38].copy_from_slice(&5u16.to_be_bytes());
    h[0x38..0x3A].copy_from_slice(&blocks.to_be_bytes());
    h[0x3A..0x3C].copy_from_slice(&0xFFFFu16.to_be_bytes());
    h.extend(std::iter::repeat_n(fill, blocks as usize * 0x2000));
    h
}

/// Dolphin's GCI folder for two games: Melee (GALE) with two saves, and Zelda (GZLE) with one.
pub fn dolphin_gc_folder(root: &Path) {
    write(root, "GC/USA/Card A/01-GALE-gzle.gci", &gci(b"GALE", b"01", "melee_save", 3, 0x11));
    write(root, "GC/USA/Card A/01-GALE-opts.gci", &gci(b"GALE", b"01", "melee_opts", 1, 0x22));
    write(root, "GC/USA/Card A/01-GZLE-zelda.gci", &gci(b"GZLE", b"01", "zelda_save", 2, 0x33));
}

// ---- PlayStation ------------------------------------------------------------------------------

/// A raw PS1 memory card holding `saves` = (directory name, block count), laid out in order.
pub fn ps1_card(saves: &[(&str, usize)]) -> Vec<u8> {
    let mut card = vec![0u8; 128 * 1024];
    card[0..2].copy_from_slice(b"MC");
    for frame in 1..16 {
        card[frame * 128] = 0xA0;
        card[frame * 128 + 8..frame * 128 + 10].copy_from_slice(&[0xFF, 0xFF]);
    }
    let mut block = 1usize;
    for (name, blocks) in saves {
        for i in 0..*blocks {
            let fr = (block + i) * 128;
            let state: u32 = if i == 0 { 0x51 } else if i == blocks - 1 { 0x53 } else { 0x52 };
            let link: u16 = if i == blocks - 1 { 0xFFFF } else { (block + i) as u16 };
            card[fr..fr + 4].copy_from_slice(&state.to_le_bytes());
            let size: u32 = if i == 0 { (*blocks as u32) * 8192 } else { 0 };
            card[fr + 4..fr + 8].copy_from_slice(&size.to_le_bytes());
            card[fr + 8..fr + 10].copy_from_slice(&link.to_le_bytes());
            if i == 0 {
                card[fr + 10..fr + 10 + name.len()].copy_from_slice(name.as_bytes());
            }
        }
        block += blocks;
    }
    // Data frames carry a recognisable payload, so two cards differ.
    for f in 16..1024usize {
        card[f * 128] = ((f * 7) & 0xFF) as u8;
    }
    card
}

// ---- Wii and 3DS ------------------------------------------------------------------------------

/// Wii Sports' (RSBE) save data under Dolphin's user folder.
pub fn wii_title(root: &Path) {
    write(root, "Wii/title/00010000/52534245/data/rs_save.dat", &[b's'; 64]);
    write(root, "Wii/title/00010000/52534245/data/banner.bin", &[b'b'; 32]);
}

const N3DS_SD: &str = "sdmc/Nintendo 3DS/00000000000000000000000000000000/00000000000000000000000000000000/";

/// One 3DS title's save, metadata and extdata, its installed content (not a save), and another game's save.
pub fn azahar_sdmc(root: &Path) {
    write(root, &format!("{N3DS_SD}title/00040000/0017c400/data/00000001/save.bin"), &[b'3'; 128]);
    write(root, &format!("{N3DS_SD}title/00040000/0017c400/data/00000001.metadata"), &[b'm'; 16]);
    write(root, &format!("{N3DS_SD}extdata/00000000/000017c4/user.bin"), &[b'e'; 64]);
    write(root, &format!("{N3DS_SD}title/00040000/0017c400/content/00000000.app"), &b"installed".repeat(9));
    write(root, &format!("{N3DS_SD}title/00040000/00033500/data/00000001/other.bin"), &[b'o'; 8]);
}

// ---- Disc images -------------------------------------------------------------------------------

fn both_u32(v: u32) -> [u8; 8] {
    let mut o = [0u8; 8];
    o[..4].copy_from_slice(&v.to_le_bytes());
    o[4..].copy_from_slice(&v.to_be_bytes());
    o
}

fn both_u16(v: u16) -> [u8; 4] {
    let mut o = [0u8; 4];
    o[..2].copy_from_slice(&v.to_le_bytes());
    o[2..].copy_from_slice(&v.to_be_bytes());
    o
}

/// One ISO 9660 directory record.
fn iso_record(extent: u32, size: u32, is_dir: bool, name: &[u8]) -> Vec<u8> {
    let mut len = 33 + name.len();
    if len % 2 == 1 {
        len += 1;
    }
    let mut r = vec![0u8; len];
    r[0] = len as u8;
    r[2..10].copy_from_slice(&both_u32(extent));
    r[10..18].copy_from_slice(&both_u32(size));
    r[18..25].copy_from_slice(&[126, 1, 1, 0, 0, 0, 0]);
    r[25] = if is_dir { 2 } else { 0 };
    r[28..32].copy_from_slice(&both_u16(1));
    r[32] = name.len() as u8;
    r[33..33 + name.len()].copy_from_slice(name);
    r
}

/// A minimal but valid ISO 9660 image with `files` in its root directory: enough for a real
/// container parser to read, which is what identifying a game from its disc needs.
pub fn iso9660(files: &[(&str, &[u8])]) -> Vec<u8> {
    const S: usize = 2048;
    let root_extent = 18u32;
    // Directory records: ".", "..", then each file (data follows the directory, a sector apiece or more).
    let mut dir = iso_record(root_extent, S as u32, true, &[0]);
    dir.extend(iso_record(root_extent, S as u32, true, &[1]));
    let mut next = root_extent + 1;
    let mut placed = Vec::new();
    for (name, data) in files {
        dir.extend(iso_record(next, data.len() as u32, false, format!("{name};1").as_bytes()));
        placed.push((next, *data));
        next += (data.len().div_ceil(S)).max(1) as u32;
    }
    assert!(dir.len() <= S, "the root directory must fit one sector");

    let total = next as usize;
    let mut img = vec![0u8; total * S];
    let pvd = &mut img[16 * S..17 * S];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    pvd[8..40].fill(b' ');
    pvd[40..72].fill(b' ');
    pvd[40..44].copy_from_slice(b"TEST");
    pvd[80..88].copy_from_slice(&both_u32(total as u32));
    pvd[120..124].copy_from_slice(&both_u16(1));
    pvd[124..128].copy_from_slice(&both_u16(1));
    pvd[128..132].copy_from_slice(&both_u16(S as u16));
    pvd[132..140].copy_from_slice(&both_u32(10));
    pvd[156..190].copy_from_slice(&iso_record(root_extent, S as u32, true, &[0])[..34]);
    pvd[881] = 1;
    let term = &mut img[17 * S..18 * S];
    term[0] = 0xFF;
    term[1..6].copy_from_slice(b"CD001");
    term[6] = 1;
    img[18 * S..18 * S + dir.len()].copy_from_slice(&dir);
    for (extent, data) in placed {
        let at = extent as usize * S;
        img[at..at + data.len()].copy_from_slice(data);
    }
    img
}

/// A PSP UMD as far as identification goes: `UMD_DATA.BIN` at the root names the disc.
pub fn psp_disc(serial_with_dash: &str) -> Vec<u8> {
    iso9660(&[("UMD_DATA.BIN", format!("{serial_with_dash}|0000000000000000|0001|G").as_bytes())])
}

// ---- Dreamcast --------------------------------------------------------------------------------

/// What a claimed `MYGAME_SAV` (1 KiB of 0x07) travels as, from the reference run through argosy-sigil.
pub const VMU_CLAIMED_HASH: &str = "d03449138a9bbd350f60966f3cce1fc6";

/// A 128 KiB Dreamcast VMU holding `saves` (name, data), laid out as the console does: 256 blocks of
/// 512 bytes, root block 255, FAT 254, directory 253 down to 241, user blocks 0..200.
pub fn vmu(saves: &[(&str, &[u8])]) -> Vec<u8> {
    const B: usize = 512;
    let stamp = [0x20u8, 0x26, 0x10, 0x29, 0x12, 0, 0, 0];
    let mut img = vec![0u8; 256 * B];
    let put16 = |img: &mut Vec<u8>, at: usize, v: u16| img[at..at + 2].copy_from_slice(&v.to_le_bytes());
    let r = 255 * B;
    img[r..r + 16].fill(0x55);
    img[r + 0x30..r + 0x38].copy_from_slice(&stamp);
    put16(&mut img, r + 0x46, 254); // FAT block
    put16(&mut img, r + 0x48, 1); // FAT size
    put16(&mut img, r + 0x4A, 253); // directory block
    put16(&mut img, r + 0x4C, 13); // directory size
    put16(&mut img, r + 0x4E, 0);
    put16(&mut img, r + 0x50, 200); // user blocks

    let mut fat = [0xFFFCu16; 256];
    fat[255] = 0xFFFA;
    fat[254] = 0xFFFA;
    for b in (242..=253).rev() {
        fat[b] = (b - 1) as u16;
    }
    fat[241] = 0xFFFA;
    let mut next = 0usize;
    let mut entries = Vec::new();
    for (name, data) in saves {
        let n = data.len().div_ceil(B);
        let blocks: Vec<usize> = (next..next + n).collect();
        next += n;
        for (i, b) in blocks.iter().enumerate() {
            fat[*b] = if i + 1 < n { blocks[i + 1] as u16 } else { 0xFFFA };
            let chunk = &data[i * B..((i + 1) * B).min(data.len())];
            img[b * B..b * B + chunk.len()].copy_from_slice(chunk);
        }
        entries.push((*name, blocks[0], n));
    }
    for (i, v) in fat.iter().enumerate() {
        put16(&mut img, 254 * B + i * 2, *v);
    }
    for (i, (name, first, n)) in entries.iter().enumerate() {
        let o = 253 * B + i * 32;
        img[o] = 0x33;
        put16(&mut img, o + 2, *first as u16);
        img[o + 4..o + 4 + name.len()].copy_from_slice(name.as_bytes());
        img[o + 16..o + 24].copy_from_slice(&stamp);
        put16(&mut img, o + 0x18, *n as u16);
    }
    img
}

/// The shared VMU Flycast keeps, holding a save of the game played and one of another game.
pub fn vmu_with_both() -> Vec<u8> {
    vmu(&[("MYGAME_SAV", &[7u8; 1024]), ("OTHER_SAV", &[9u8; 512])])
}

/// The same VMU before the game was played: only the other game's save.
pub fn vmu_before_play() -> Vec<u8> {
    vmu(&[("OTHER_SAV", &[9u8; 512])])
}

// ---------------------------------------------------------------------------------------------
// QEMU as the referee for xemu disk images. The tests that need it skip (loudly) where
// `qemu-img` is not installed; where it is, it judges the image independently of this crate.
// ---------------------------------------------------------------------------------------------

pub mod qemu {
    use std::path::Path;
    use std::process::Command;

    pub fn available() -> bool {
        Command::new("qemu-img").arg("--version").output().is_ok_and(|o| o.status.success())
    }

    /// `qemu-img check`: every refcount, L1 and L2 entry consistent, nothing leaked or corrupt.
    pub fn check(img: &Path) -> Result<(), String> {
        let out = Command::new("qemu-img").args(["check", "-f", "qcow2"]).arg(img).output().map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        if out.status.success() && text.contains("No errors were found") {
            Ok(())
        } else {
            Err(format!("qemu-img check ({}):\n{text}", out.status))
        }
    }

    /// The guest disk as a sparse raw file, read by QEMU.
    pub fn to_raw(img: &Path, raw: &Path) {
        let st = Command::new("qemu-img").args(["convert", "-f", "qcow2", "-O", "raw"]).arg(img).arg(raw).status().unwrap();
        assert!(st.success(), "qemu-img convert failed");
    }

    /// Whether QEMU reads the two disks as identical (a raw model against a qcow2 image).
    pub fn same_disk(raw: &Path, img: &Path) -> Result<(), String> {
        let out = Command::new("qemu-img").args(["compare", "-f", "raw", "-F", "qcow2"]).arg(raw).arg(img).output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("qemu-img compare ({}): {}", out.status, String::from_utf8_lossy(&out.stdout)))
        }
    }
}

/// A file that starts failing after a set number of writes, to stop a transaction at every point.
pub struct Flaky {
    pub f: std::fs::File,
    pub left: std::rc::Rc<std::cell::Cell<i64>>,
}
impl Flaky {
    fn spend(&self) -> std::io::Result<()> {
        let n = self.left.get();
        if n == 0 {
            return Err(std::io::Error::other("injected failure"));
        }
        if n > 0 {
            self.left.set(n - 1);
        }
        Ok(())
    }
}
impl std::io::Read for Flaky {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        self.f.read(b)
    }
}
impl std::io::Write for Flaky {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.spend()?;
        self.f.write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.f.flush()
    }
}
impl std::io::Seek for Flaky {
    fn seek(&mut self, p: std::io::SeekFrom) -> std::io::Result<u64> {
        self.f.seek(p)
    }
}
impl romm_core::xbox::qcow2::BackingFile for Flaky {
    fn set_len(&mut self, len: u64) -> std::io::Result<()> {
        self.spend()?;
        self.f.set_len(len)
    }
    fn sync(&mut self) -> std::io::Result<()> {
        self.f.sync_all()
    }
}
