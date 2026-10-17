#!/bin/bash
export RUST_BACKTRACE=0
ROMM=${ROMM:-$(dirname "$0")/../../target/debug/romm}
mkdir -p /tmp/smoke; SRV=127.0.0.1:18765
setsid python3 "$(dirname "$0")/mock_romm.py" >/tmp/smoke/server.out 2>&1 &
SP=$!; sleep 1
dev() { # dev A|B  -> switch "machine"
  export HOME=/tmp/smoke/$1 ROMM_DESKTOP_HOME=/tmp/smoke/$1/rommhome FAKE_DEVICE=$1
  export XDG_DATA_HOME=$HOME/.local/share XDG_CONFIG_HOME=$HOME/.config PATH=$(dirname "$0")/fake-emulators:/usr/bin:/bin
}
card() { cat "/tmp/smoke/$1/.local/share/dolphin-emu/GC/USA/Card A.raw" 2>/dev/null || echo '<none>'; }
hr() { echo; echo "━━ $*"; }
rm -rf /tmp/smoke/A /tmp/smoke/B; mkdir -p /tmp/smoke/A /tmp/smoke/B

dev A
hr "A: wrong pairing code"; $ROMM login --server $SRV --code 00000000 2>&1 | head -3
hr "A: login"; $ROMM login --server $SRV --code 12345678 --device-name "Device A"
hr "A: platforms"; $ROMM platforms
hr "A: install"; $ROMM install 7 2>&1 | tr '\r' '\n' | tail -1
hr "A: emulators (Dolphin data dir on a fresh machine)"; $ROMM emulators 2>&1 | sed -n 2,3p
hr "A: first play"; $ROMM play 7
echo "   A card: $(card A)"

dev B
hr "B: login + install"; $ROMM login --server $SRV --code 12345678 --device-name "Device B" >/dev/null && $ROMM install 7 2>&1 | tr '\r' '\n' | tail -1
hr "B: play  (should receive A's card BEFORE the emulator starts)"; $ROMM play 7
echo "   B card: $(card B)"

dev A
hr "A: sync  (should pick up B's card)"; $ROMM sync 7
echo "   A card: $(card A)"

dev B
hr "B: plays again, card content changes (uploads B2)"; FAKE_DEVICE=B2 $ROMM play 7 | grep -v "^Syncing"

dev A
echo "offline-A-progress" > "/tmp/smoke/A/.local/share/dolphin-emu/GC/USA/Card A.raw"
hr "A: edited its card offline, then syncs → CONFLICT (default: keep both)"; $ROMM sync 7
echo "   A card now: $(card A)"
echo "   A backups: $(grep -rl 'offline-A-progress' /tmp/smoke/A/rommhome/data/backups 2>/dev/null | head -1 | sed 's#/tmp/smoke/A/rommhome/data/##')"

hr "BIOS: psx needs DuckStation (fake binary found on PATH)"
dev A; $ROMM bios psx 2>&1 | tail -3
echo "   $(ls /tmp/smoke/A/.local/share/duckstation/bios/ 2>&1)"

hr "BIOS: platform with no firmware on the server -> clear error"; $ROMM bios ngc 2>&1 | tail -1

echo; echo "━━ server-side view"; grep -E "DEVICE|NEGOTIATE|UPLOAD|COMPLETE" /tmp/smoke/server.log | cut -c1-170
kill $SP 2>/dev/null
