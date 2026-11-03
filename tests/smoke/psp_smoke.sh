#!/bin/bash
# Two machines exchange a PSP save through the real CLI and a mock RomM, and the script checks that
# what travels is exactly what Argosy would upload: a zip of the game's save folders, each under its
# own name, no SAVEDATA/ prefix, no manifest. Exits non-zero if anything is off.
export RUST_BACKTRACE=0
ROMM=${ROMM:-$(dirname "$0")/../../target/debug/romm}
FAKES=$(cd "$(dirname "$0")" && pwd)/fake-emulators
mkdir -p /tmp/smoke; SRV=127.0.0.1:18765
SMOKE_GAME=psp setsid python3 "$(dirname "$0")/mock_romm.py" >/tmp/smoke/server.out 2>&1 &
SP=$!; sleep 1
FAIL=0
ok()  { echo "  ok   $*"; }
bad() { echo "  FAIL $*"; FAIL=1; }
dev() { export HOME=/tmp/smoke/$1 ROMM_DESKTOP_HOME=/tmp/smoke/$1/rommhome FAKE_DEVICE=$1
        export XDG_DATA_HOME=$HOME/.local/share XDG_CONFIG_HOME=$HOME/.config PATH=$FAKES:/usr/bin:/bin; }
dat() { cat "/tmp/smoke/$1/.config/ppsspp/PSP/SAVEDATA/ULUS10064DATA00/DATA.BIN" 2>/dev/null || echo '<none>'; }
hr()  { echo; echo "━━ $*"; }
rm -rf /tmp/smoke/A /tmp/smoke/B; mkdir -p /tmp/smoke/A /tmp/smoke/B

dev A; hr "A: login, install, play (the fake emulator writes a PSP save)"
$ROMM login --server $SRV --code 12345678 --device-name "Device A" >/dev/null
$ROMM install 7 2>&1 | tr '\r' '\n' | tail -1
$ROMM play 7
[ "$(dat A)" = "progress-from-A" ] && ok "A has its save" || bad "A's save: $(dat A)"

hr "what the server received from A"
UP=$(grep "UPLOAD" /tmp/smoke/server.log | tail -1); echo "  $UP" | cut -c1-260
echo "$UP" | grep -q "emu=ppsspp"                                      && ok "labelled with the emulator's id"        || bad "emulator label"
echo "$UP" | grep -q "name=Crisis Core.zip"                            && ok "named <rom>.zip"                         || bad "file name"
echo "$UP" | grep -q "'ULUS10064DATA00/DATA.BIN'"                      && ok "folder entries under their own names"    || bad "entries"
echo "$UP" | grep -q "'ULUS10064SETTINGS/OPT.BIN'"                     && ok "every folder of the game is in it"       || bad "settings folder missing"
echo "$UP" | grep -q "SAVEDATA/\|romm-bundle"                          && bad "found a SAVEDATA/ prefix or a manifest" || ok "no SAVEDATA/ prefix, no manifest"

dev B; hr "B: login, install, play (must receive A's save BEFORE the emulator starts)"
$ROMM login --server $SRV --code 12345678 --device-name "Device B" >/dev/null
$ROMM install 7 2>&1 | tr '\r' '\n' | tail -1
OUT=$($ROMM play 7); echo "$OUT"
echo "$OUT" | grep -q "save at launch: progress-from-A" && ok "B's emulator found A's save at launch" || bad "B did not get A's save"
[ "$(dat B)" = "progress-from-B" ] && ok "B then saved its own progress" || bad "B's save: $(dat B)"

dev A; hr "A: sync (should pick up B's progress)"
$ROMM sync 7
[ "$(dat A)" = "progress-from-B" ] && ok "A received B's save" || bad "A's save after sync: $(dat A)"

hr "A: sync again with nothing changed (must not upload)"
N1=$(grep -c "UPLOAD slot=autosave" /tmp/smoke/server.log); $ROMM sync 7 >/dev/null; N2=$(grep -c "UPLOAD slot=autosave" /tmp/smoke/server.log)
[ "$N1" = "$N2" ] && ok "no upload when nothing changed" || bad "uploaded again ($N1 -> $N2)"

hr "negotiations as the server saw them"
grep -E "NEGOTIATE" /tmp/smoke/server.log | cut -c1-120
kill $SP 2>/dev/null
echo; [ $FAIL = 0 ] && echo "PSP smoke: all checks passed" || echo "PSP smoke: FAILED"
exit $FAIL
