#!/usr/bin/env python3
"""Drives the real RomM Desktop app (via WebKitWebDriver) through the main user journeys."""
import os, sys, time, traceback
sys.stdout.reconfigure(line_buffering=True)
from wd import WD

BIN, OUT = sys.argv[1], sys.argv[2]
HOME = os.environ["HOME"]
results = []

def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(f"  {'PASS' if ok else 'FAIL'}  {name}" + (f"   [{detail}]" if detail and not ok else ""), flush=True)
    return ok

def count(css): return len(d.find_all(css))
def shot(n): d.screenshot(f"{OUT}/{n}.png")
def type_into(css, text, clear=True):
    el = d.wait_css(css)
    d.click(el)
    if clear:
        d.keys(el, "\ue009a\ue000"); d.keys(el, "\ue003")
    d.keys(el, text)
def server_log(): return open(f"{OUT}/server.log").read()
def toast_text(): return " | ".join(d.text(t) for t in d.find_all(".toast"))
def dismiss_toasts():
    for b in d.find_all(".toast .icon-btn"):
        try: d.click(b)
        except RuntimeError: pass
def wait_toast(sub, timeout=30): return d.wait(lambda: sub in toast_text() and toast_text(), timeout, f"toast containing {sub!r}")

d = WD()
try:
    d.start(BIN)
    print("== Login")
    d.wait_css("[data-testid=server]", 30)
    shot("01-login")
    check("login screen shown", d.find(".login") is not None)
    check("Connect disabled until the form is filled", d.enabled(d.find("[data-testid=connect]")) is False)

    type_into("[data-testid=server]", "127.0.0.1:18766")
    type_into("[data-testid=code]", "0000-0000")
    d.click(d.find("[data-testid=connect]"))
    err = d.wait_css("[data-testid=login-error]", 15)
    check("wrong pairing code shows a plain-language error", "wasn't recognised" in d.text(err) and "http://" not in d.text(err), d.text(err))
    shot("02-login-error")

    type_into("[data-testid=code]", "1234-5678")
    d.click(d.find("[data-testid=connect]"))
    d.wait_css(".shell", 20)
    check("valid code connects", True)
    check("device was registered with RomM", "DEVICE" in server_log() and "romm-desktop" in server_log())

    print("== Library")
    d.wait(lambda: count("[data-testid=game-card]") >= 48, 20, "first page of games")
    check("first page has 48 games", count("[data-testid=game-card]") == 48, str(count("[data-testid=game-card]")))
    side = d.text(d.find(".sidebar"))
    check("sidebar lists the platforms", all(p in side for p in ("GameCube", "PlayStation", "PlayStation Portable")), side[:200])
    check("platforms without an installed emulator are flagged", d.find(".sidebar .dot-warn") is not None)
    d.wait(lambda: d.js("return document.querySelectorAll('.cover img').length") > 0, 15, "cover art")
    check("cover art loads", True)
    d.js("document.querySelector('.main').scrollTo(0, 1e7)")
    d.wait(lambda: count("[data-testid=game-card]") >= 67, 15, "infinite scroll")
    check("scrolling loads the rest (67 games)", count("[data-testid=game-card]") == 67, str(count("[data-testid=game-card]")))
    d.js("document.querySelector('.main').scrollTo(0, 0)")
    time.sleep(0.5); shot("03-library")

    type_into("[data-testid=search]", "melee", clear=False)
    d.wait(lambda: count("[data-testid=game-card]") == 1, 10, "search results")
    check("search narrows to Melee", "Melee" in d.text(d.find("[data-testid=game-card]")))

    print("== Install and play")
    d.click(d.find("[data-testid=game-card]"))
    d.wait_css("[data-testid=drawer]")
    d.wait(lambda: d.find(".emu-line"), 10, "emulator line")
    time.sleep(0.4); shot("04-drawer")
    check("drawer shows the emulator that will be used", "Plays in Dolphin" in d.text(d.find(".emu-line")), d.text(d.find(".emu-line")))
    check("Install offered, Play not yet", d.find("[data-testid=install]") is not None and d.find("[data-testid=play]") is None)

    d.click(d.find("[data-testid=install]"))
    try:
        d.wait(lambda: d.find(".progress.wide"), 4, "progress bar"); shot("05-installing")
        check("download progress is shown", True)
    except TimeoutError:
        check("download progress is shown", False, "progress bar never appeared")
    d.wait_css("[data-testid=play]", 40)
    check("install completes and Play appears", True)
    rom = f"{HOME}/Games/RomM/ngc/Super Smash Bros. Melee.iso"
    check("ROM file is on disk with the right size", os.path.exists(rom) and os.path.getsize(rom) == 2_000_000, rom)
    check("empty save/state lists before first play", "No save file yet" in d.text(d.find(".drawer")))

    d.click(d.find("[data-testid=play]"))
    t = wait_toast("played", 40)
    shot("06-after-play")
    check("play session reports uploaded save", "Uploaded 1 save" in t, t)
    check("play session reports uploaded save state", "Uploaded 1 save state" in t, t)
    log = server_log()
    check("server received the memory card bundle", "SAVE-UPLOAD slot=autosave" in log)
    check("server received the save state", "STATE-UPLOAD GALE01.s01 emu=dolphin" in log)
    check("playtime was reported", "play_sessions" in log and "duration_ms" in log)
    d.wait(lambda: count("[data-testid=saves] li") == 1 and count("[data-testid=states] li") == 1, 10, "saves and states listed")
    check("save shown as in sync", "In sync" in d.text(d.find("[data-testid=saves]")), d.text(d.find("[data-testid=saves]")))
    check("state shown as in sync", "In sync" in d.text(d.find("[data-testid=states]")), d.text(d.find("[data-testid=states]")))
    check("state file name is listed", "GALE01.s01" in d.text(d.find("[data-testid=states]")))

    print("== Navigation")
    d.js("document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape'})); window.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape'}))")
    d.wait(lambda: d.find("[data-testid=drawer]") is None, 5, "drawer closes")
    check("Escape closes the drawer", True)
    d.click(d.find("[data-testid=nav-installed]"))
    d.wait(lambda: count("[data-testid=game-card]") == 1, 10, "installed list")
    check("Installed lists the game with its badge", d.find(".badge-installed") is not None)
    d.wait(lambda: d.js("return document.querySelectorAll('.cover img').length") > 0, 10, "installed cover")
    check("Installed shows cover art (offline-capable)", True)
    shot("07-installed")

    print("== Emulators & BIOS")
    d.click(d.find("[data-testid=nav-emulators]"))
    d.wait_css("[data-testid=emu-dolphin]")
    dolphin, ppsspp = d.text(d.find("[data-testid=emu-dolphin]")), d.text(d.find("[data-testid=emu-ppsspp]"))
    check("Dolphin detected on PATH", "Found" in dolphin and "dolphin-emu" in dolphin, dolphin[:120])
    check("PPSSPP reported as not found, with guidance", "Not found" in ppsspp, ppsspp[:120])
    shot("08-emulators")
    dismiss_toasts(); time.sleep(0.3)
    d.click(d.find("[data-testid=bios-psx]"))
    d.wait(lambda: "scph5501.bin" in d.text(d.find(".page")), 15, "BIOS result")
    bios = f"{HOME}/.local/share/duckstation/bios/scph5501.bin"
    check("BIOS downloaded into DuckStation's folder", os.path.exists(bios) and open(bios, "rb").read() == b"FAKE-PS1-BIOS-IMAGE", bios)
    shot("09-bios")

    print("== Settings")
    dismiss_toasts()
    d.click(d.find("[data-testid=nav-settings]"))
    d.wait_css("[data-testid=sync-states]")
    d.click(d.find("[data-testid=sync-states]"))
    d.click(d.find("[data-testid=save-settings]"))
    wait_toast("Settings saved", 10)
    cfg = open(f"{HOME}/rommhome/config/config.toml").read()
    check("turning state sync off is persisted", "sync_states = false" in cfg, cfg[:300])
    shot("10-settings")
    d.click(d.find("[data-testid=sync-states]")); d.click(d.find("[data-testid=save-settings]"))
    d.wait(lambda: "sync_states = true" in open(f"{HOME}/rommhome/config/config.toml").read(), 10, "re-enable")
    check("state sync can be turned back on", True)

    print("== Sync all")
    dismiss_toasts()
    d.click(d.find("[data-testid=sync-all]"))
    t = wait_toast("Synced 1 game", 30)
    check("Sync all syncs the installed game", True)
    shot("11-sync-all")

    print("== Disconnect")
    d.click(d.find(".link-btn"))
    d.wait_css(".login", 10)
    check("Disconnect returns to the login screen", True)
    check("token file removed", not os.path.exists(f"{HOME}/rommhome/config/token"))
except Exception as e:
    traceback.print_exc()
    check("scenario completed without an unexpected error", False, str(e)[:200])
    try: shot("99-failure")
    except Exception: pass
finally:
    d.quit()

failed = [r for r in results if not r[1]]
print(f"\n{len(results) - len(failed)}/{len(results)} checks passed")
sys.exit(1 if failed else 0)
