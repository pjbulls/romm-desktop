#!/usr/bin/env python3
"""Drives the real RomM Desktop app to check per-system emulator choices: each system's dropdown
offers only what can play it, picks persist (UI and config file), and the backend refuses the rest.

Run with:  DRIVER=drive_choices.py tests/e2e/run.sh
The fake server has GameCube, PlayStation and PSP. Dolphin and DuckStation are "installed"
(fake binaries on PATH); PPSSPP and RetroArch are not.
"""
import os, sys, time, tomllib, traceback
sys.stdout.reconfigure(line_buffering=True)
from wd import WD

BIN, OUT = sys.argv[1], sys.argv[2]
CONFIG = os.environ["ROMM_DESKTOP_HOME"] + "/config/config.toml"
results = []

def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(f"  {'PASS' if ok else 'FAIL'}  {name}" + (f"   [{detail}]" if detail and not ok else ""), flush=True)
    return ok

def shot(n): d.screenshot(f"{OUT}/{n}.png")
def type_into(css, text):
    el = d.wait_css(css)
    d.click(el)
    d.keys(el, "\ue009a\ue000"); d.keys(el, "\ue003")
    d.keys(el, text)

FIND = "[...document.querySelectorAll('select')].find(x => x.getAttribute('aria-label') === arguments[0])"
def select_info(platform):
    """The dropdown for a system: its current value and every [value, text] option, or None."""
    return d.js(f"const s = {FIND}; return s ? {{value: s.value, options: [...s.options].map(o => [o.value, o.textContent])}} : null", f"Emulator for {platform}")

def choose(platform, value):
    """Picks an option the way the app's own controller code does (native setter + change event)."""
    d.js(f"const s = {FIND}; Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(s, arguments[1]); s.dispatchEvent(new Event('change', {{bubbles: true}}));", f"Emulator for {platform}", value)

def config():
    return tomllib.load(open(CONFIG, "rb")) if os.path.exists(CONFIG) else {}
def saved(table, slug): return config().get(table, {}).get(slug)

def invoke(command, args):
    """Calls a backend command directly; returns ('ok', None) or ('err', message)."""
    r = d.js_async("const done = arguments[arguments.length - 1];"
                   f"window.__TAURI_INTERNALS__.invoke('{command}', {args}).then(() => done(['ok', null]), e => done(['err', String(e)]));")
    return tuple(r)

def open_emulators():
    d.click(d.find("[data-testid=nav-emulators]"))
    d.wait_css("[data-testid=emu-dolphin]")
    d.wait(lambda: select_info("GameCube"), 10, "platform dropdowns")

d = WD()
try:
    d.start(BIN)
    d.wait_css("[data-testid=server]", 30)
    type_into("[data-testid=server]", "127.0.0.1:18766")
    type_into("[data-testid=code]", "1234-5678")
    d.click(d.find("[data-testid=connect]"))
    d.wait_css(".shell", 20)

    print("== Each system offers only what can play it")
    open_emulators()
    gc, psp, ps1 = select_info("GameCube"), select_info("PlayStation Portable"), select_info("PlayStation")
    check("GameCube: Automatic + Dolphin only", gc["options"] == [["", "Automatic (Dolphin)"], ["dolphin", "Dolphin"]], str(gc))
    check("PSP: Automatic + PPSSPP only, flagged not installed",
          psp["options"] == [["", "Automatic (PPSSPP)"], ["ppsspp", "PPSSPP (not installed)"]], str(psp))
    check("PlayStation: DuckStation first, then RetroArch's PS1 cores",
          [o[0] for o in ps1["options"]] == ["", "duckstation", "retroarch:pcsx_rearmed", "retroarch:swanstation", "retroarch:mednafen_psx"],
          str(ps1))
    check("PlayStation: labels, with uninstalled RetroArch cores flagged",
          [o[1] for o in ps1["options"]] == ["Automatic (DuckStation)", "DuckStation", "RetroArch · PCSX-ReARMed (not installed)",
                                              "RetroArch · SwanStation (not installed)", "RetroArch · Beetle PSX (not installed)"], str(ps1))
    texts = " | ".join(o[1] for info in (gc, psp, ps1) for o in info["options"])
    check("no Xbox/3DS emulator appears anywhere", "xemu" not in texts and "Azahar" not in texts, texts)
    check("GameCube offers no PlayStation/PSP emulator or RetroArch core",
          not any(w in " ".join(o[1] for o in gc["options"]) for w in ("DuckStation", "PPSSPP", "RetroArch")))
    check("nothing is selected until the user picks", gc["value"] == ps1["value"] == psp["value"] == "")
    d.js(f"const s = {FIND}; s.scrollIntoView({{block: 'center'}});", "Emulator for PlayStation")
    shot("12-choices")

    print("== Picking a RetroArch core")
    choose("PlayStation", "retroarch:swanstation")
    # Read straight away: the pick must show at once, not snap back to the old choice while the
    # platform list is refetched from the server (a regression this check exists to catch).
    check("the pick shows immediately, with no snap-back", select_info("PlayStation")["value"] == "retroarch:swanstation")
    d.wait(lambda: saved("platform_emulators", "psx") == "retroarch", 10, "emulator saved to config.toml")
    check("config records RetroArch + the chosen core",
          (saved("platform_emulators", "psx"), saved("retroarch_cores", "psx")) == ("retroarch", "swanstation"), str(config()))
    time.sleep(1.0)  # let the platform list refetch finish
    check("dropdown still shows the saved choice after the refetch", select_info("PlayStation")["value"] == "retroarch:swanstation")
    check("other systems are untouched", saved("platform_emulators", "ngc") is None and saved("platform_emulators", "psp") is None)
    check("Automatic still describes what it would pick", select_info("PlayStation")["options"][0][1] == "Automatic (DuckStation)")

    d.click(d.find("[data-testid=nav-settings]")); d.wait_css("[data-testid=sync-states]")
    open_emulators()
    check("choice survives leaving and reopening the page", select_info("PlayStation")["value"] == "retroarch:swanstation")

    print("== Switching to a standalone emulator drops the core")
    choose("PlayStation", "duckstation")
    check("switching shows immediately too", select_info("PlayStation")["value"] == "duckstation")
    d.wait(lambda: saved("platform_emulators", "psx") == "duckstation", 10, "switch to DuckStation")
    check("no stale RetroArch core is left behind", saved("retroarch_cores", "psx") is None, str(config()))
    time.sleep(1.0)
    check("dropdown shows DuckStation after the refetch", select_info("PlayStation")["value"] == "duckstation")

    print("== Back to Automatic")
    choose("PlayStation", "")
    check("Automatic shows immediately", select_info("PlayStation")["value"] == "")
    d.wait(lambda: saved("platform_emulators", "psx") is None, 10, "Automatic clears the choice")
    check("config no longer mentions the system", saved("platform_emulators", "psx") is None and saved("retroarch_cores", "psx") is None, str(config()))
    time.sleep(1.0)
    check("dropdown is still on Automatic after the refetch", select_info("PlayStation")["value"] == "")

    print("== The backend refuses choices that can't play the system")
    for slug, bad in [("psx", "xemu"), ("psx", "retroarch:mupen64plus_next"), ("ngc", "retroarch"), ("psp", "duckstation"), ("psx", "dolphin:gambatte")]:
        status, msg = invoke("set_platform_choice", f"{{slug: '{slug}', choice: '{bad}'}}")
        check(f"refuses {bad!r} for {slug}", status == "err" and "can't play" in msg, f"{status} {msg}")
    check("refusals changed nothing", "platform_emulators" not in config() or not config()["platform_emulators"], str(config()))
    status, msg = invoke("set_platform_choice", "{slug: 'psx', choice: 'retroarch:pcsx_rearmed'}")
    check("a valid choice is still accepted", status == "ok" and saved("retroarch_cores", "psx") == "pcsx_rearmed", f"{status} {msg}")
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
