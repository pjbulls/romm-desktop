# Vendored argosy-sigil

`vendor/argosy-sigil/` is [argosy-sigil](https://github.com/rommapp/argosy-sigil), unmodified, at the
commit [Argosy](https://github.com/rommapp/argosy-launcher) pins as its `sigil` submodule. It is the
library Argosy uses to decide what a game's save is and to put one back, so using the same code is
what makes saves written here restorable in Argosy and the other way round.

The exact revisions (Sigil, plus the libchdr and tiny-AES-c it bundles) are in `vendor/PINNED`.

## What is copied

Only what `build.rs` compiles: Sigil's `src/` and `include/`, libchdr's five sources with the zlib,
zstd (decompressor only) and lzma it bundles, and tiny-AES-c. Their licence files travel with them.
Nothing is edited; `build.rs` mirrors Sigil's `CMakeLists.txt` (same sources, definitions, include
paths) into one static library, so a plain C compiler is the only extra build requirement.

## Updating

Bump to whatever commit Argosy pins (its `.gitmodules`/gitlink for `sigil`):

```sh
crates/sigil-sys/scripts/update-vendor.sh <commit>
crates/sigil-sys/scripts/check-sigil-layout.sh      # prints the struct sizes/offsets
cargo test -p sigil-sys -p romm-core
```

`check-sigil-layout.sh` compiles a C probe against the vendored `sigil.h`. If any number differs from
the asserts in `src/lib.rs`, the header changed shape: update the Rust structs to match *first*. The
reference hashes in `crates/romm-core/tests/` (`PSP_HASH`, `GC_HASH`, ...) are what Sigil's `collect`
returned for the same fixtures; a Sigil change that alters a unit's wire format changes them, and that
is exactly the signal wanted (a unit-shape change breaks every save already on a server, so Sigil
only does it with a compatible reader).

## What is still mirrored by hand

Sigil decides what a save *is*. Which emulator and core uses which layout, what the server is told, and
which emulator settings to read are Argosy's choices, mirrored here in `crates/romm-core/src/route.rs`
and `sync.rs`. They are small, but they can drift. When bumping the pin, compare against Argosy's
`app/src/main/kotlin/com/nendo/argosy/data/sync/platform/SigilSaveHandler.kt` and
`data/emulator/EmulatorRegistry.kt`:

* `RETROARCH_UNIT_CORES` (route.rs) vs. Argosy's libretro layout table: which cores travel as units.
* The standalone layout ids and the settings read for them (`Card1Type`, `SlotA`, `Slot1_Filename`, ...).
* The `emulator` label rule (`toServerEmulator`): core name for RetroArch, emulator id otherwise.
* Whether Argosy still runs Sigil unmanaged, and how it claims unowned saves (`claimNewSaves`).

## Why `v1.0.0` is not the pin

Sigil has a `v1.0.0` tag, but Argosy pins a later commit that includes identification fixes (Wii disc
ids through ISO/WBFS/WIA/RVZ, 3DS CIA ids, PS2 CSO/ZSO, PSX/Dreamcast cue sheets). Matching Argosy means
matching its pin; move to a tag once Argosy does.

## Licences

Sigil is MPL-2.0 (file-level copyleft), which is compatible with this project's GPL-3.0-or-later
through MPL-2.0 §3.3; the vendored files stay under their own licences (`vendor/argosy-sigil/LICENSE`,
`licenses/`, and each bundled library's). Sigil's `TRADEMARKS.md` asks that bindings be named
`sigil-<language>`-style and not imply endorsement: this crate is `sigil-sys` and a binding, not a fork.
