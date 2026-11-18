#!/bin/sh
# Re-vendors argosy-sigil (and the libraries it bundles) into vendor/argosy-sigil.
#
#   scripts/update-vendor.sh <commit> [<git url or local path>]
#
# Use the commit Argosy pins (its `sigil` submodule), so saves match what Argosy writes. Only the
# files build.rs compiles are copied, unmodified. Afterwards run scripts/check-sigil-layout.sh and
# paste any changed numbers into src/lib.rs's layout tests (a change means sigil.h changed shape).
set -eu

commit=${1:?usage: update-vendor.sh <commit> [<git url or path>]}
repo=${2:-https://github.com/rommapp/argosy-sigil}
here=$(cd "$(dirname "$0")/.." && pwd)
dest="$here/vendor/argosy-sigil"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git clone -q "$repo" "$work/sigil"
git -C "$work/sigil" checkout -q "$commit"
git -C "$work/sigil" submodule update --init --recursive -q
S="$work/sigil"
L="$S/third_party/libchdr"
T="$S/third_party/tiny-AES-c"

rm -rf "$dest"
mkdir -p "$dest/src" "$dest/include" "$dest/licenses"
cp "$S"/src/*.c "$S"/src/*.h "$dest/src/"
cp "$S/include/sigil.h" "$dest/include/"
cp "$S/LICENSE" "$S/TRADEMARKS.md" "$dest/"
cp "$S"/licenses/* "$dest/licenses/"

D="$dest/third_party/libchdr"
mkdir -p "$D/src" "$D/include" "$D/deps/lzma-24.05/src" "$D/deps/lzma-24.05/include" \
         "$D/deps/zlib-1.3.1" "$D/deps/zstd-1.5.6/lib/common" "$D/deps/zstd-1.5.6/lib/decompress"
cp "$L/LICENSE.txt" "$D/"
cp "$L"/src/*.c "$L"/src/*.h "$D/src/" 2>/dev/null || cp "$L"/src/*.c "$D/src/"
cp -r "$L/include/." "$D/include/"
cp "$L/deps/lzma-24.05/LICENSE" "$D/deps/lzma-24.05/"
cp "$L"/deps/lzma-24.05/src/*.c "$D/deps/lzma-24.05/src/"
cp -r "$L/deps/lzma-24.05/include/." "$D/deps/lzma-24.05/include/"
cp "$L/deps/zlib-1.3.1/LICENSE" "$L"/deps/zlib-1.3.1/*.h "$D/deps/zlib-1.3.1/"
for f in adler32 compress crc32 deflate infback inffast inflate inftrees trees uncompr zutil; do
    cp "$L/deps/zlib-1.3.1/$f.c" "$D/deps/zlib-1.3.1/"
done
cp "$L/deps/zstd-1.5.6/LICENSE" "$D/deps/zstd-1.5.6/"
cp "$L"/deps/zstd-1.5.6/lib/*.h "$D/deps/zstd-1.5.6/lib/"
cp "$L"/deps/zstd-1.5.6/lib/common/* "$D/deps/zstd-1.5.6/lib/common/"
cp "$L"/deps/zstd-1.5.6/lib/decompress/* "$D/deps/zstd-1.5.6/lib/decompress/"
find "$dest" -name '*.S' -delete   # ZSTD_DISABLE_ASM: the amd64 assembly is never built
mkdir -p "$dest/third_party/tiny-AES-c"
cp "$T/aes.c" "$T/aes.h" "$dest/third_party/tiny-AES-c/"
cp "$T/unlicense.txt" "$dest/third_party/tiny-AES-c/" 2>/dev/null || true

{
    echo "argosy-sigil $(git -C "$S" rev-parse HEAD)"
    git -C "$S" submodule status --recursive | sed 's/^[ +-]//' | awk '{print $2 " " $1}'
} > "$here/vendor/PINNED"
echo "vendored:"; cat "$here/vendor/PINNED"
