#!/bin/bash
export RUST_BACKTRACE=0
D=$(cd "$(dirname "$0")" && pwd)
ROMM=${ROMM:-$D/../../target/debug/romm}
mkdir -p /tmp/smoke; SRV=127.0.0.1:18765
cp $D/mock_romm.py /tmp/smoke/mock_romm.py; rm -rf /tmp/smoke/fake-emulators; cp -r $D/fake-emulators /tmp/smoke/fake-emulators
setsid python3 /tmp/smoke/mock_romm.py >/tmp/smoke/server.out 2>&1 & SP=$!; sleep 1
dev() { export HOME=/tmp/smoke/$1 ROMM_DESKTOP_HOME=/tmp/smoke/$1/rommhome FAKE_DEVICE=${2:-$1}
        export XDG_DATA_HOME=$HOME/.local/share XDG_CONFIG_HOME=$HOME/.config PATH=/tmp/smoke/fake-emulators:/usr/bin:/bin; }
slot() { cat "/tmp/smoke/$1/.local/share/dolphin-emu/StateSaves/GALE01.s01" 2>/dev/null || echo '<none>'; }
hr() { echo; echo "━━ $*"; }
rm -rf /tmp/smoke/A /tmp/smoke/B; mkdir -p /tmp/smoke/A /tmp/smoke/B
for d in A B; do dev $d; $ROMM login --server $SRV --code 12345678 --device-name "Device $d" >/dev/null; $ROMM install 7 >/dev/null 2>&1; done

dev A; hr "A: play → creates save state slot 1"; $ROMM play 7 | grep -E "state|launch"
hr "A: romm states 7"; $ROMM states 7
dev B; hr "B: play → should receive A's state BEFORE launch"; $ROMM play 7 | grep -E "state|launch"
echo "   B slot now: $(slot B)"
dev B; hr "B: 'romm states 7'"; $ROMM states 7

hr "Both devices change the state while apart"
dev A; echo "A-edited-offline" > /tmp/smoke/A/.local/share/dolphin-emu/StateSaves/GALE01.s01
dev B; FAKE_DEVICE=B2 $ROMM play 7 2>&1 | grep -E "state (up|down)" ; echo "   server now holds: $(grep 'STATE-UPLOAD' /tmp/smoke/server.log | tail -1 | grep -o "b'[^']*'")"
dev A; hr "A: states 7 shows the conflict"; $ROMM states 7
hr "A: sync --no-states leaves states alone"; $ROMM sync 7 --no-states | grep -c state || true
hr "A: sync (default keep-both)"; $ROMM sync 7 | grep -E "state"
echo "   A slot now: $(slot A)"
echo "   A backup of its edit: $(grep -rl 'A-edited-offline' /tmp/smoke/A/rommhome/data/backups 2>/dev/null | head -1 | sed 's#.*/backups/#backups/#')"
echo; echo "━━ server-side log"; grep STATE-UPLOAD /tmp/smoke/server.log | cut -c1-120
kill $SP 2>/dev/null
