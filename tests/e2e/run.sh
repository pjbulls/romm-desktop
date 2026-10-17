#!/bin/bash
# End-to-end test: drives the real app through WebKitWebDriver under Xvfb against a fake RomM server.
# Needs: xvfb, webkit2gtk-driver, python3 + pillow. Usage: tests/e2e/run.sh [path/to/romm-desktop]
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
export BIN=${1:-$ROOT/target/debug/romm-desktop} ROOT OUT=${OUT:-/tmp/e2e}
rm -rf "$OUT"; mkdir -p "$OUT/home"
export HOME=$OUT/home ROMM_DESKTOP_HOME=$OUT/home/rommhome
export XDG_DATA_HOME=$HOME/.local/share XDG_CONFIG_HOME=$HOME/.config
export PATH=$ROOT/tests/e2e/fake-emulators:/usr/bin:/bin
export TAURI_WEBVIEW_AUTOMATION=true WEBKIT_DISABLE_COMPOSITING_MODE=1 WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1 NO_AT_BRIDGE=1 RUST_BACKTRACE=0
exec xvfb-run -a -s "-screen 0 1440x900x24" bash -c '
  python3 "$ROOT/tests/e2e/mock_romm.py" 18766 "$OUT/server.log" & MOCK=$!
  WebKitWebDriver --port=4444 > "$OUT/webdriver.log" 2>&1 & WDP=$!
  sleep 2
  python3 "$ROOT/tests/e2e/${DRIVER:-drive.py}" "$BIN" "$OUT"; RC=$?
  kill $MOCK $WDP 2>/dev/null; exit $RC'
