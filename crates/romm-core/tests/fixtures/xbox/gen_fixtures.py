#!/usr/bin/env python3
"""Builds the Xbox HDD test fixtures from independent reference implementations.

  * libfatx (via pyfatx, GPL-2.0-or-later) formats the FATX partitions and writes every file.
  * qemu-img converts the raw image to real QEMU-written qcow2 files (several flavours).
  * The manifest is produced by re-reading the finished volume through libfatx, not from the
    inputs, so Rust tests compare against what a reference reader sees.

Nothing here ships in romm-desktop; it exists so the Rust qcow2/FATX code is tested against
implementations it shares no code with. Needs: pip install pyfatx; qemu-img on PATH.
Regenerate:  python3 gen_fixtures.py   (writes next to this script)
"""
import hashlib, json, os, shutil, subprocess, sys, tempfile
import pyfatx
from pyfatx import Fatx, pyfatx_open_helper, fatx_disk_format_partition

HERE = os.path.dirname(os.path.abspath(__file__))
SECTOR, SPC = 512, 32                      # 16 KiB clusters, as on a retail drive
E_OFF, E_SIZE = 0xABE80000, 0x1312D6000    # partition E, from libfatx's retail layout
DISK = 8 * 1024 ** 3
SMALL_OFF, SMALL_SIZE = 0x100000, 32 * 1024 * 1024   # FAT16 sized (2048 clusters)
ATTR_DIR = 1 << 4
FILE_DELETED = 1   # FATX_STATUS_FILE_DELETED in libfatx/fatx.h (END_OF_DIR is 2)


def blob(tag: str, n: int) -> bytes:
    out, i = b"", 0
    while len(out) < n:
        out += hashlib.sha256(f"{tag}:{i}".encode()).digest(); i += 1
    return out[:n]


def utf16(s: str) -> bytes:
    return b"\xff\xfe" + s.encode("utf-16-le")


# ---- raw FATX helpers: libfatx cannot grow a directory past one cluster (it fails at entry 257), but real
# ---- disks have such directories, so the generator extends one by hand. libfatx's *reader* then verifies it.
import struct

def _geom(img, offset, size):
    with open(img, "rb") as f:
        f.seek(offset); sb = f.read(16)
    assert sb[:4] == b"FATX"
    _vid, spc, root = struct.unpack("<III", sb[4:16])
    bpc = spc * SECTOR
    n = size // bpc + 1
    fat16 = n < 0xFFF0
    fat_size = -(-n * (2 if fat16 else 4) // 4096) * 4096
    return dict(bpc=bpc, n=n, fat16=fat16, fat_off=offset + 4096, area=offset + 4096 + fat_size, root=root)

def _fat_get(f, g, c):
    es = 2 if g["fat16"] else 4
    f.seek(g["fat_off"] + c * es); return int.from_bytes(f.read(es), "little")

def _fat_set(f, g, c, v):
    es = 2 if g["fat16"] else 4
    f.seek(g["fat_off"] + c * es); f.write(v.to_bytes(es, "little"))

def _chain(f, g, c):
    out = []
    while True:
        out.append(c); nxt = _fat_get(f, g, c)
        if (g["fat16"] and nxt >= 0xFFF8) or (not g["fat16"] and nxt >= 0xFFFFFFF8): return out
        c = nxt

def _find_dir(f, g, parts):
    cur = g["root"]
    for part in parts:
        hit = None
        for cl in _chain(f, g, cur):
            f.seek(g["area"] + (cl - 1) * g["bpc"]); data = f.read(g["bpc"])
            for i in range(0, len(data), 64):
                e = data[i:i + 64]
                if e[0] in (0x00, 0xFF): break
                if e[0] != 0xE5 and e[2:2 + e[0]].decode("latin1") == part and e[1] & 0x10:
                    hit = struct.unpack("<I", e[44:48])[0]
        assert hit, part
        cur = hit
    return cur

def extend_dir_raw(img, offset, size, parts, names):
    """Appends zero-length file entries to a directory, growing it by one cluster."""
    g = _geom(img, offset, size)
    with open(img, "r+b") as f:
        first = _find_dir(f, g, parts); last = _chain(f, g, first)[-1]
        free = next(c for c in range(2, g["n"]) if _fat_get(f, g, c) == 0)
        eoc = 0xFFFF if g["fat16"] else 0xFFFFFFFF
        _fat_set(f, g, last, free); _fat_set(f, g, free, eoc)
        buf = bytearray(b"\xff" * g["bpc"])
        date, time = ((2024 - 2000) << 9) | (6 << 5) | 15, (12 << 11) | (30 << 5)
        for k, name in enumerate(names):
            nb = name.encode("latin1")
            e = bytes([len(nb), 0]) + nb.ljust(42, b"\xff") + struct.pack("<II6H", 0, 0, time, date, time, date, time, date)
            buf[k * 64:(k + 1) * 64] = e
        f.seek(g["area"] + (free - 1) * g["bpc"]); f.write(buf)


def populate(fs: Fatx):
    mk = lambda p: fs.mkdir(p)
    put = lambda p, b: fs.write(p, b)
    mk("/UDATA"); mk("/TDATA")
    put("/UDATA/readme.txt", b"not a title folder")           # stray file at the UDATA root
    mk("/UDATA/notatitle"); put("/UDATA/notatitle/x.bin", b"x")  # dir whose name is not a title id

    t1 = "/UDATA/4d530004"                                      # lower-case dir name, like hakuX
    mk(t1)
    put(t1 + "/TitleMeta.xbx", utf16("TitleName=Fixture One\r\n"))
    put(t1 + "/TitleImage.xbx", blob("img1", 4096))
    mk(t1 + "/a1b2c3d4e5f6")
    put(t1 + "/a1b2c3d4e5f6/SaveMeta.xbx", utf16("Name=Slot 1\r\nNoCopy=0\r\n"))
    put(t1 + "/a1b2c3d4e5f6/saveimage.xbx", blob("simg", 4096))
    put(t1 + "/a1b2c3d4e5f6/career.sav", blob("career", 100_000))   # spans 7 clusters
    put(t1 + "/a1b2c3d4e5f6/empty.bin", b"")
    put(t1 + "/a1b2c3d4e5f6/exact_cluster.bin", blob("exact", 16384))
    # libfatx refuses 42-character names (the on-disk field holds 42), so the reference-built fixture stops at 41;
    # the Rust tests cover the full 42 with a synthetic volume.
    put(t1 + "/a1b2c3d4e5f6/" + "abcdefghijklmnopqrstuvwxyz0123456789ABCD.x"[:41], blob("long", 77))
    mk(t1 + "/deep"); mk(t1 + "/deep/er"); mk(t1 + "/deep/er/est")
    put(t1 + "/deep/er/est/leaf.txt", b"leaf")

    t2 = "/UDATA/41430001"                                      # upper-case dir name
    mk(t2)
    put(t2 + "/TitleMeta.xbx", utf16("TitleName=Fixture Two\r\n"))
    mk(t2 + "/many")
    for i in range(256):                                        # fills exactly one 16 KiB directory cluster
        put(t2 + f"/many/f{i:03d}.dat", blob(f"m{i}", 10 + i) if i < 8 else b"")   # empty files cost no cluster
    for n in ("gone1.bin", "keep.bin", "gone2.bin"):
        put(t2 + "/" + n, blob(n, 5000))
    fs.unlink(t2 + "/gone1.bin"); fs.unlink(t2 + "/gone2.bin")  # leave deleted dirents behind

    t3 = "/UDATA/54430003"
    mk(t3); put(t3 + "/TitleMeta.xbx", utf16("TitleName=Fixture Three\r\n"))
    mk("/UDATA/55550001")                                       # title with an empty folder

    mk("/TDATA/4d530004"); put("/TDATA/4d530004/progress.dat", blob("tdata", 5000))


def listdir(fs: Fatx, path: str):
    """pyfatx's Fatx.listdir() stops at the first deleted entry (it breaks on any non-zero status, including
    FATX_STATUS_FILE_DELETED). libfatx itself, and its fatxfs driver, skip deleted entries and carry on, so
    this loop does the same."""
    from pyfatx import ffi, fatx_open_dir, fatx_read_dir, fatx_next_dir_entry, fatx_close_dir
    d = ffi.new("struct fatx_dir *"); assert fatx_open_dir(fs.fs, fs._sanitize_path(path), d) == 0
    dirent = ffi.new("struct fatx_dirent *"); attr = ffi.new("struct fatx_attr *"); nxt = ffi.new("struct fatx_dirent **")
    out = []
    while True:
        s = fatx_read_dir(fs.fs, d, dirent, attr, nxt)
        if s == 0:
            out.append(fs._create_attr(attr))
        elif s != FILE_DELETED:
            break                                   # end of directory (or an error)
        if fatx_next_dir_entry(fs.fs, d) != 0:
            break
    assert fatx_close_dir(fs.fs, d) == 0
    return out


def snapshot(fs: Fatx, root: str):
    """What libfatx reads back: {relpath: (size, md5)} plus directory list."""
    files, dirs = {}, []
    def walk(path):
        attrs = listdir(fs, path)
        dn = [a.filename for a in attrs if a.is_directory]; fn = [a.filename for a in attrs if a.is_file]
        yield path, dn, fn
        for d in dn:
            yield from walk(os.path.join(path, d))
    for dirpath, dnames, fnames in walk(root):
        rel = dirpath[len(root):].strip("/")
        for d in dnames:
            dirs.append((rel + "/" + d).strip("/"))
        for f in fnames:
            data = fs.read(dirpath.rstrip("/") + "/" + f)
            files[(rel + "/" + f).strip("/")] = {"size": len(data), "md5": hashlib.md5(data).hexdigest()}
    return files, sorted(dirs)


def build_raw(path, offset, size, populate_fn):
    with open(path, "wb") as f:
        f.truncate(DISK if size > SMALL_SIZE else offset + size)
    fs = pyfatx_open_helper()
    assert fatx_disk_format_partition(fs, path.encode(), offset, size, SECTOR, SPC) == 0, "format failed"
    fs2 = Fatx(path, offset=offset, size=size)
    populate_fn(fs2)
    del fs2


def region_manifest(path, offset, size):
    fs = Fatx(path, offset=offset, size=size)
    top = [a.filename for a in listdir(fs, "/UDATA")]
    titles = {}
    for a in listdir(fs, "/UDATA"):
        if a.is_directory and len(a.filename) == 8 and all(c in "0123456789abcdefABCDEF" for c in a.filename):
            files, dirs = snapshot(fs, "/UDATA/" + a.filename)
            titles[a.filename.upper()] = {"dir_name": a.filename, "files": files, "dirs": dirs}
    tfiles, _ = snapshot(fs, "/TDATA")
    return {"udata_entries": sorted(top), "titles": titles, "tdata_files": tfiles}


def verify_intent(m):
    """The manifest comes from libfatx; this checks it against what populate() meant to write, so a reader
    bug in the oracle cannot quietly produce a wrong expectation."""
    t = m["e"]["titles"]
    assert sorted(t) == ["41430001", "4D530004", "54430003", "55550001"], sorted(t)
    t2 = t["41430001"]["files"]
    assert "keep.bin" in t2 and "gone1.bin" not in t2 and "gone2.bin" not in t2, "deleted-entry handling"
    assert sum(k.startswith("many/") for k in t2) == 300, "directory growth"
    t1 = t["4D530004"]["files"]
    assert t1["a1b2c3d4e5f6/career.sav"]["size"] == 100_000
    assert t1["a1b2c3d4e5f6/empty.bin"]["size"] == 0 and t1["a1b2c3d4e5f6/exact_cluster.bin"]["size"] == 16384
    assert t1["a1b2c3d4e5f6/" + "abcdefghijklmnopqrstuvwxyz0123456789ABCD.x"[:41]]["size"] == 77
    assert t1["deep/er/est/leaf.txt"]["size"] == 4
    assert m["e"]["titles"]["55550001"]["files"] == {}
    assert "notatitle" in m["e"]["udata_entries"] and "readme.txt" in m["e"]["udata_entries"]
    assert list(m["e"]["tdata_files"]) == ["4d530004/progress.dat"]


def qemu(raw, out, *opts):
    subprocess.run(["qemu-img", "convert", "-f", "raw", "-O", "qcow2", *opts, raw, out], check=True)


def main():
    with tempfile.TemporaryDirectory() as tmp:
        build_all(tmp)


def build_all(tmp):
    raw = os.path.join(tmp, "e.img")
    build_raw(raw, E_OFF, E_SIZE, populate)
    extend_dir_raw(raw, E_OFF, E_SIZE, ["UDATA", "41430001", "many"], [f"f{i:03d}.dat" for i in range(256, 300)])
    manifest = {"spc": SPC, "e": {"offset": E_OFF, "size": E_SIZE, **region_manifest(raw, E_OFF, E_SIZE)}}
    variants = {
        "hdd-v3.qcow2": [],                                           # QEMU default: v3, 64 KiB clusters
        "hdd-v2.qcow2": ["-o", "compat=0.10"],                        # legacy v2 (no zero flag, no feature bits)
        "hdd-compressed.qcow2": ["-c"],                               # zlib-compressed clusters
        "hdd-4k.qcow2": ["-o", "cluster_size=4096"],                  # small clusters: many L2 tables
    }
    for name, opts in variants.items():
        qemu(raw, os.path.join(HERE, name), *opts)
    # An internal snapshot, as xemu's Machine > Snapshots makes: afterwards every cluster and L2
    # table is shared between the snapshot and the running disk, so none may be written in place.
    snap = os.path.join(HERE, "hdd-snapshot.qcow2")
    shutil.copy(os.path.join(HERE, "hdd-v3.qcow2"), snap)
    subprocess.run(["qemu-img", "snapshot", "-c", "before-save", snap], check=True)

    sraw = os.path.join(tmp, "s.img")
    def small(fs):
        fs.mkdir("/UDATA"); fs.mkdir("/UDATA/aabbccdd")
        fs.write("/UDATA/aabbccdd/TitleMeta.xbx", utf16("TitleName=Small\r\n"))
        fs.write("/UDATA/aabbccdd/data.bin", blob("small", 40_000))
        fs.mkdir("/TDATA")
    build_raw(sraw, SMALL_OFF, SMALL_SIZE, small)
    manifest["small"] = {"offset": SMALL_OFF, "size": SMALL_SIZE, **region_manifest(sraw, SMALL_OFF, SMALL_SIZE)}
    qemu(sraw, os.path.join(HERE, "small-fat16.qcow2"))

    verify_intent(manifest)
    with open(os.path.join(HERE, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=1, sort_keys=True)
    for n in sorted(os.listdir(HERE)):
        if n.endswith(".qcow2"):
            print(f"{n:24} {os.path.getsize(os.path.join(HERE, n)):>9} bytes")
    print("titles:", sorted(manifest["e"]["titles"]), "| UDATA entries:", manifest["e"]["udata_entries"])


if __name__ == "__main__":
    main()
