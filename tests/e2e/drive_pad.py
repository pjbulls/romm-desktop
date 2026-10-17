#!/usr/bin/env python3
"""Drives the real app with a *simulated* game controller.

There is no /dev/uinput in CI-like sandboxes, so this injects a fake `navigator.getGamepads()` into the page.
That exercises everything in the app (polling, button mapping, focus movement, the on-screen keyboard,
suspension while a game runs) through the real webview, but not the OS → WebKit gamepad hop.
"""
import os, sys, time, traceback
sys.stdout.reconfigure(line_buffering=True)
from wd import WD

BIN, OUT = sys.argv[1], sys.argv[2]
HOME = os.environ["HOME"]
results = []
BTN = dict(accept=0, back=1, x=2, y=3, lb=4, rb=5, lt=6, rt=7, select=8, start=9, up=12, down=13, left=14, right=15)

def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(f"  {'PASS' if ok else 'FAIL'}  {name}" + (f"   [{detail}]" if detail and not ok else ""), flush=True)
    return ok

d = WD()
def js(s, *a): return d.js(s, *a)
def shot(n): d.screenshot(f"{OUT}/{n}.png")
def count(css): return len(d.find_all(css))
def server_log(): return open(f"{OUT}/server.log").read()
def type_into(css, text, clear=True):
    el = d.wait_css(css); d.click(el)
    if clear: d.keys(el, "\ue009a\ue000"); d.keys(el, "\ue003")
    d.keys(el, text)

INJECT = """
window.__pad = {buttons: Array(17).fill(false), axes: [0,0,0,0], hold: {}, axisHold: {}, polls: 0};
navigator.getGamepads = () => {
  const p = window.__pad; p.polls++;
  const buttons = p.buttons.map((b, i) => {
    const h = p.hold[i] || 0; if (h > 0) p.hold[i] = h - 1;
    const pressed = b || h > 0;
    return {pressed, touched: pressed, value: pressed ? 1 : 0};
  });
  const axes = p.axes.slice();
  for (const k of Object.keys(p.axisHold)) { const a = p.axisHold[k]; if (a.n > 0) { axes[k] = a.v; a.n--; } }
  return [{id: 'Xbox 360 Controller (XInput STANDARD GAMEPAD)', index: 0, connected: true, mapping: 'standard', axes, buttons, timestamp: performance.now()}];
};
document.hasFocus = () => true;   // there is no window manager under Xvfb, so the page never reports focus
"""
# A tap is exactly `frames` polls long however slow WebDriver is (a wall-clock press could outlast the app's
# 380 ms repeat delay on a busy machine and register as a held button).
WAIT_POLLS = """const [kind, key, val, frames] = arguments; const done = arguments[arguments.length - 1];
  const p = window.__pad; const target = p.polls + frames + 3;
  if (kind === 'b') p.hold[key] = frames; else p.axisHold[key] = {v: val, n: frames};
  const t = setInterval(() => { if (p.polls >= target) { clearInterval(t); clearTimeout(bail); done(true); } }, 8);
  const bail = setTimeout(() => { clearInterval(t); p.hold = {}; p.axisHold = {}; done(false); }, 900);   // polling is off (e.g. controller disabled)"""
def press(name, frames=3, **_):
    d.js_async(WAIT_POLLS, "b", BTN[name], 0, frames)
def stick_tap(ax, value, frames=3):
    d.js_async(WAIT_POLLS, "a", ax, value, frames)
def stick(ax, value, secs):          # continuous deflection, for smooth scrolling
    js(f"window.__pad.axes[{ax}] = {value}"); time.sleep(secs); js(f"window.__pad.axes[{ax}] = 0"); time.sleep(0.25)

def focused(): return js("""const a = document.activeElement; if (!a || a === document.body) return null;
  return {tag: a.tagName, testid: a.dataset.testid || null, gameId: a.dataset.gameId || null, cls: a.className || '', text: (a.innerText || a.value || '').slice(0, 60), type: a.type || null}""")
def pad_mode(): return js("return document.documentElement.classList.contains('gamepad-active')")
def hints(): return js("const h = document.querySelector('[data-testid=hints]'); return h ? h.innerText : ''")
def sidebar_active(): return js("const a = document.querySelector('.sidebar .nav.active'); return a ? a.innerText : ''")
def card_ids(): return js("return Array.from(document.querySelectorAll('[data-testid=game-card]')).map(c => Number(c.dataset.gameId))")
def toasts(): return js("return Array.from(document.querySelectorAll('.toast')).map(t => t.innerText).join(' | ')")
def wait_toast(sub, t=40): return d.wait(lambda: sub in toasts() and toasts(), t, f"toast {sub!r}")
def nav_either(pred, limit=40, what="target"):
    """Reach an element by pressing Down then Up, checking at every step, wherever focus happened to land."""
    for key in ("down", "up"):
        for _ in range(limit):
            f = focused()
            if f and pred(f): return True
            press(key)
    raise TimeoutError(f"never reached {what}; last focus {focused()}")
def nav_until(pred, key="down", limit=40, what="target"):
    for _ in range(limit):
        f = focused()
        if f and pred(f): return True
        press(key)
    raise TimeoutError(f"never reached {what}; last focus {focused()}")

try:
    d.start(BIN)
    d.wait_css("[data-testid=server]", 40)
    js(INJECT)
    time.sleep(0.8)   # let the app see the controller at rest first (a button already held when a pad appears is ignored)
    print("== Login screen with a controller")
    press("accept")                       # focus starts on the server field → opens the on-screen keyboard
    d.wait_css("[data-testid=osk]", 5)
    shot("p01-osk")
    check("pressing A on a text field opens the on-screen keyboard", True)
    check("keyboard shows what field it's for", "Server address" in js("return document.querySelector('.osk-label').innerText"))
    check("focus starts on a key", (focused() or {}).get("tag") == "BUTTON" and js("return document.activeElement.dataset.key") == "1")
    check("button hints switch to keyboard mode", "Type" in hints() and "Delete" in hints() and "Shift" in hints(), hints())
    press("right"); press("accept")       # '1' → '2', type it
    press("accept")                       # type '2' again
    press("y"); press("down"); press("accept")   # shift, then a key one row down → uppercase letter
    typed = js("return document.querySelector('[data-testid=osk-preview]').innerText")
    check("D-pad moves between keys and A types them (with shift)", typed.startswith("22") and len(typed) == 3 and typed[2].isupper(), typed)
    check("typing reaches the real input", js("return document.querySelector('[data-testid=server]').value") == typed)
    press("x"); press("x")                # delete two characters
    check("X deletes", js("return document.querySelector('[data-testid=osk-preview]').innerText") == "2")
    press("back")
    d.wait(lambda: d.find("[data-testid=osk]") is None, 5, "keyboard closes")
    check("B closes the keyboard and returns focus to the field", (focused() or {}).get("testid") == "server")
    # the rest of login uses the real keyboard (a controller can't pair in a hurry); then back to the pad
    type_into("[data-testid=server]", "127.0.0.1:18766")
    type_into("[data-testid=code]", "1234-5678")
    d.click(d.find("[data-testid=connect]"))
    d.wait_css(".shell", 25)
    d.wait(lambda: len(card_ids()) >= 48, 20, "library")
    check("keyboard use switches controller mode off", pad_mode() is False)

    print("== Library with a controller")
    press("right")
    check("first press wakes controller mode", pad_mode() is True)
    f = focused()
    check("focus lands on a game card", f and "game-card" in f["cls"], str(f))
    check("button hints are shown for cards", "Details" in hints() and "Platform" in hints(), hints())
    shot("p02-card-focus")
    ids = card_ids()
    cols = js("const c = [...document.querySelectorAll('[data-testid=game-card]')]; return c.filter(x => x.offsetTop === c[0].offsetTop).length")
    press("right"); press("right"); press("down")
    check(f"d-pad moves through the {cols}-column grid", (focused() or {}).get("gameId") == str(ids[cols + 2]), f"{focused()} expected id {ids[cols + 2]}")
    press("left"); press("up")
    check("and back again", (focused() or {}).get("gameId") == str(ids[1]), str(focused()))
    stick_tap(1, 1.0)
    check("left stick moves focus like the d-pad", (focused() or {}).get("gameId") == str(ids[cols + 1]), str(focused()))

    before = js("return document.querySelector('.main').scrollTop")
    stick(3, 1.0, 0.7)
    after = js("return document.querySelector('.main').scrollTop")
    check("right stick scrolls the page", after > before + 50, f"{before} → {after}")
    js("document.querySelector('.main').scrollTo(0, 0)")
    press("rt")
    check("RT pages down", js("return document.querySelector('.main').scrollTop") > 200, str(js("return document.querySelector('.main').scrollTop")))
    check("focus follows to a visible card", js("""const a = document.activeElement; if (!a) return false; const r = a.getBoundingClientRect(), m = document.querySelector('.main').getBoundingClientRect();
      return a.classList.contains('game-card') && r.top >= m.top - 5 && r.bottom <= m.bottom + 5"""))
    press("lt")

    before_total = len(card_ids())
    js("document.querySelector('.main').scrollTo(0, 1e7)"); time.sleep(0.6)
    press("down")
    d.wait(lambda: len(card_ids()) > before_total or len(card_ids()) == 67, 8, "more games to load")
    check("moving past the last loaded row loads more games", len(card_ids()) == 67, str(len(card_ids())))
    js("document.querySelector('.main').scrollTo(0, 0)"); time.sleep(0.3)

    print("== Platforms and search")
    start_label = sidebar_active()
    press("rb"); first = sidebar_active()
    check("RB switches to the next entry", first != start_label and "Installed" in first, f"{start_label!r} → {first!r}")
    press("rb"); second = sidebar_active()
    check("RB again reaches a platform", "GameCube" in second, second)
    d.wait(lambda: len(card_ids()) == 48, 10, "grid")
    d.wait(lambda: (focused() or {}).get("cls", "").find("game-card") >= 0, 6, "focus restored")
    check("focus is restored onto the new grid", True)
    press("lb"); check("LB goes back", "Installed" in sidebar_active())
    press("rb")
    press("y")
    check("Y focuses the search box", (focused() or {}).get("testid") == "search", str(focused()))
    type_into("[data-testid=search]", "melee", clear=False)    # typing with a real keyboard
    d.wait(lambda: len(card_ids()) == 1, 10, "search")
    press("down")
    f = focused()
    check("Down from search enters the results", f and "game-card" in f["cls"] and f["gameId"] == "7", str(f))

    print("== Install and play with a controller")
    press("accept")
    d.wait_css("[data-testid=drawer]", 5)
    d.wait(lambda: (focused() or {}).get("testid") == "install", 8, "focus on Install")
    check("opening a game focuses its main action", True)
    d.wait(lambda: "Close" in hints(), 3, "panel hints")
    check("hints switch to the panel", "Close" in hints() and "Install" in hints(), hints())
    shot("p03-drawer-focus")
    press("right"); press("down")
    check("focus stays inside the open panel", js("return !!document.activeElement.closest('[role=dialog]')"))
    press("up"); press("left")
    d.wait(lambda: (focused() or {}).get("testid") == "install" or js("return !!document.activeElement.closest('[role=dialog]')"), 3, "")
    d.js("document.querySelector('[data-testid=install]').focus()")
    press("accept")                                          # install
    d.wait_css("[data-testid=play]", 45)
    d.wait(lambda: (focused() or {}).get("testid") == "play", 8, "focus on Play")
    check("after installing, focus moves to Play", True)

    press("accept")                                          # play
    d.wait(lambda: "Playing" in js("const p = document.querySelector('[data-testid=play]'); return p ? p.innerText : ''"), 20, "running phase")
    press("back")                                            # would close the panel, if input weren't suspended
    press("accept")
    check("controller input is ignored while the game runs", d.find("[data-testid=drawer]") is not None)
    t = wait_toast("played", 40)
    check("play finished and synced", "Uploaded 1 save" in t and "Uploaded 1 save state" in t, t)
    time.sleep(0.5)
    press("back")
    check("B works again after the game exits", d.find("[data-testid=drawer]") is None)
    check("closing returns focus to the card that opened it", (focused() or {}).get("gameId") == "7", str(focused()))
    press("x")
    check("X dismisses notifications", toasts() == "", toasts())

    print("== Start plays the focused game")
    before_logs = server_log().count("play_sessions")
    press("start")
    t = wait_toast("played", 45)
    check("Start launches an installed game straight from the grid", server_log().count("play_sessions") > before_logs, t)
    time.sleep(0.8)
    press("back")
    d.wait(lambda: d.find("[data-testid=drawer]") is None, 5, "panel closes")

    print("== Menu navigation, emulators page and settings")
    press("select")
    check("Select moves focus to the menu", js("return !!document.activeElement.closest('.sidebar')"))
    nav_until(lambda f: f["testid"] == "nav-emulators", "down", 25, "Emulators & BIOS")
    press("accept")
    d.wait_css("[data-testid=emu-dolphin]", 8)
    check("A opens a page from the menu", True)
    press("right")
    check("Right moves from the menu into the page", js("return !document.activeElement.closest('.sidebar')"))
    before = js("return document.querySelector('.main').scrollTop")
    stick(3, 1.0, 0.7)
    check("right stick scrolls long pages", js("return document.querySelector('.main').scrollTop") > before + 50)
    nav_either(lambda f: f["tag"] == "SELECT", 45, "a platform selector")
    for _ in range(8): press("down")      # past the last control on the page
    check("Down at the end of a page stays on the page instead of jumping into the menu", js("return !document.activeElement.closest('.sidebar')"), str(focused()))
    nav_either(lambda f: f["tag"] == "SELECT", 45, "a platform selector")
    sel_before = js("return document.activeElement.value")
    press("right")
    sel_after = js("return document.activeElement.value")
    check("left/right changes a dropdown", sel_after != sel_before and sel_after != "", f"{sel_before!r} → {sel_after!r}")
    time.sleep(0.4)
    emu = sel_after.split(":")[0]   # `dolphin`, or `retroarch` for a `retroarch:<core>` pick
    cfg = open(f"{HOME}/rommhome/config/config.toml").read()
    check("and the choice is saved", f'= "{emu}"' in cfg, cfg[-200:])
    press("left")

    press("select")
    nav_until(lambda f: f["testid"] == "nav-settings", "down", 25, "Settings")
    press("accept")
    d.wait_css("[data-testid=rom-dir]", 8)
    press("right")
    nav_either(lambda f: f["type"] == "number", 30, "the size-limit field")
    v0 = int(js("return document.activeElement.value"))
    press("right"); press("right"); press("right")
    v1 = int(js("return document.activeElement.value"))
    check("left/right steps a number field", v1 == v0 + 3, f"{v0} → {v1}")
    nav_either(lambda f: f["testid"] == "save-settings", 25, "Save changes")
    press("accept")
    wait_toast("Settings saved", 10)
    check("changes made with the controller are saved", f"max_state_mb = {v1}" in open(f"{HOME}/rommhome/config/config.toml").read())
    shot("p04-settings")
    check("settings shows the controller and its button legend", "Xbox 360 Controller" in js("return document.querySelector('[data-testid=pad-status]').innerText"))

    print("== Opting out")
    nav_either(lambda f: f["testid"] == "pad-enabled", 40, "the controller toggle")
    press("accept")
    time.sleep(0.4)
    check("the toggle turns controller navigation off", js("return localStorage.getItem('romm.gamepad.enabled')") == "off")
    f0 = focused()
    press("down"); press("down")
    check("and then the controller does nothing", focused() == f0 and not pad_mode(), f"{f0} vs {focused()}")
    d.click(d.find("[data-testid=pad-enabled]"))
    check("the mouse can turn it back on", js("return localStorage.getItem('romm.gamepad.enabled')") == "on")
    time.sleep(0.6)    # the polling loop restarts disarmed and must see the controller at rest before it reacts
    press("down")
    check("mouse use leaves controller mode, and the controller can re-enter", pad_mode() is True)
    d.js("window.dispatchEvent(new MouseEvent('mousedown'))")
    check("mouse input hides controller mode", pad_mode() is False)
except Exception as e:
    traceback.print_exc()
    try:
        print("  DEBUG state at failure:", {"focus": focused(), "pad_mode": pad_mode(), "cards": len(card_ids()), "loading_row": bool(d.find(".loading-row")),
              "empty": bool(d.find(".empty")), "sidebar_active": sidebar_active(), "hints": hints()})
    except Exception as _:
        pass
    check("scenario completed without an unexpected error", False, str(e)[:240])
    try: shot("p99-failure")
    except Exception: pass
finally:
    d.quit()

failed = [r for r in results if not r[1]]
print(f"\n{len(results) - len(failed)}/{len(results)} checks passed")
sys.exit(1 if failed else 0)
