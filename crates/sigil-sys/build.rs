//! Builds the vendored argosy-sigil (and the libraries it bundles) into one static archive.
//!
//! This mirrors Sigil's own CMakeLists.txt: same source list, same definitions, same include
//! paths. Everything goes into a single archive on purpose: Sigil, libchdr, zlib, zstd, lzma and
//! tiny-AES-c depend on each other, and one archive sidesteps static-link ordering entirely.
//!
//! Only a C compiler is needed (the `cc` crate is already in the tree through `ring`).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn c_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "c"))
        .collect();
    v.sort();
    v
}

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("vendor/argosy-sigil");
    if !root.join("include/sigil.h").exists() {
        panic!(
            "vendored argosy-sigil is missing at {} (see crates/sigil-sys/VENDOR.md)",
            root.display()
        );
    }
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-changed=build.rs");

    let chdr = root.join("third_party/libchdr");
    let zstd = chdr.join("deps/zstd-1.5.6/lib");
    let zlib = chdr.join("deps/zlib-1.3.1");
    let lzma = chdr.join("deps/lzma-24.05");
    let aes = root.join("third_party/tiny-AES-c");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let msvc = env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");

    let mut b = cc::Build::new();
    b.warnings(false);

    // ---- sources, in the order Sigil's CMake lists them ----
    // zstd: only the decompressor (Sigil never compresses), without its amd64 assembly.
    b.files(c_files(&zstd.join("common")));
    b.files(c_files(&zstd.join("decompress")));
    // zlib: core only.
    for f in [
        "adler32", "compress", "crc32", "deflate", "infback", "inffast", "inflate", "inftrees", "trees",
        "uncompr", "zutil",
    ] {
        b.file(zlib.join(format!("{f}.c")));
    }
    // lzma
    for f in [
        "Alloc", "Bra86", "BraIA64", "CpuArch", "Delta", "LzFind", "Lzma86Dec", "LzmaDec", "Sort",
    ] {
        b.file(lzma.join(format!("src/{f}.c")));
    }
    // libchdr
    for f in ["bitstream", "cdrom", "chd", "flac", "huffman"] {
        b.file(chdr.join(format!("src/libchdr_{f}.c")));
    }
    // tiny-AES-c
    b.file(aes.join("aes.c"));
    // Sigil itself: every .c in src/ is part of the library (checked against its CMake list).
    b.files(c_files(&root.join("src")));

    // ---- include paths (the PUBLIC ones of each CMake target) ----
    b.include(root.join("include"));
    b.include(root.join("src"));
    b.include(chdr.join("include"));
    b.include(&zstd).include(zstd.join("common"));
    b.include(&zlib);
    b.include(lzma.join("include"));
    b.include(&aes);

    // ---- definitions: the union of what each CMake target sets ----
    b.define("ZSTD_DISABLE_ASM", "1");
    b.define("_7ZIP_ST", None);
    b.define("FLAC__NO_DLL", None);
    b.define("FLAC__HAS_OGG", "0");
    b.define("AES128", "1").define("CBC", "0").define("CTR", "1").define("ECB", "1");
    b.define("SIGIL_VERSION_STRING", "\"1.0.0\"");
    if target_os == "linux" {
        // glibc gates fseeko/ftello/off_t under strict C99; without these, CHD reads past 2 GiB fail.
        b.define("_POSIX_C_SOURCE", "200809L");
        b.define("_DEFAULT_SOURCE", None);
        b.define("_FILE_OFFSET_BITS", "64");
    }

    if msvc {
        // Every Sigil source sees the MSVC shims for the POSIX names it uses.
        b.flag(format!("/FI{}", root.join("src/sigil_compat.h").display()));
    } else {
        b.std("c99");
        // The windowing toolkit loads its own zlib/zstd dynamically; keep ours out of the
        // executable's dynamic symbol table so the two can never interpose each other.
        b.flag_if_supported("-fvisibility=hidden");
    }

    b.compile("sigil");

    if target_os == "windows" {
        println!("cargo:rustc-link-lib=shell32");
    }
}
