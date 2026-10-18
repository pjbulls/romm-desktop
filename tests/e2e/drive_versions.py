#!/usr/bin/env python3
"""Drives the real RomM Desktop app through a game that exists in several versions.

Run with:  DRIVER=drive_versions.py tests/e2e/run.sh

The fake server has Spyro the Dragon as three files (USA 103, Europe 113, Japan 123), one game, as
RomM would list it: a request with group_by_meta_id=true returns only the USA copy (the server's
pick), which carries the other two in sibling_roms. The server counts 5 files on PlayStation; the
app should show 3 games.
"""
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
def server_log(): return open(f"{OUT}/server.log").read()
def roms_requests(): return [l for l in server_log().splitlines() if l.startswith("GET /api/roms?")]
def type_into(css, text, clear=True):
    el = d.wait_css(css)
    d.click(el)
    if clear:
        d.keys(el, "\ue009a\ue000"); d.keys(el, "\ue003")
    d.keys(el, text)

def nav_with(text, exclude=None):
    """The sidebar button whose label contains `text` (and not `exclude`)."""
    for b in d.find_all(".platform-list .nav"):
        t = d.text(b)
        if text in t and not (exclude and exclude in t):
            return b
    return None

def sidebar_count(text, exclude=None):
    return d.js("const b = [...document.querySelectorAll('.platform-list .nav')].find(b => b.innerText.includes(arguments[0]) && !(arguments[1] && b.innerText.includes(arguments[1])));"
                " return b ? b.querySelector('.count').textContent : null", text, exclude)

def version_select():
    """The drawer's version dropdown: its value and every [value, text], or None."""
    return d.js("const s = document.querySelector('[data-testid=version-select]'); return s ? {value: s.value, options: [...s.options].map(o => [o.value, o.textContent])} : null")

def choose_version(value):
    """Picks an option the way the app's own controller code does (native setter + change event)."""
    d.js("const s = document.querySelector('[data-testid=version-select]'); "
         "Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(s, arguments[0]); "
         "s.dispatchEvent(new Event('change', {bubbles: true}));", value)

def close_drawer():
    d.js("document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape'})); window.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape'}))")
    d.wait(lambda: d.find("[data-testid=drawer]") is None, 5, "drawer closes")

def open_spyro():
    card = next(c for c in d.find_all("[data-testid=game-card]") if "Spyro" in d.text(c))
    d.click(card)
    d.wait_css("[data-testid=drawer]")
    d.wait(lambda: version_select(), 10, "version dropdown")

def install_selected():
    d.click(d.find("[data-testid=install]"))
    d.wait_css("[data-testid=play]", 40)

d = WD()
try:
    d.start(BIN)
    d.wait_css("[data-testid=server]", 30)
    type_into("[data-testid=server]", "127.0.0.1:18766")
    type_into("[data-testid=code]", "1234-5678")
    d.click(d.find("[data-testid=connect]"))
    d.wait_css(".shell", 20)

    print("== The whole library is fetched once, grouped")
    d.wait(lambda: count("[data-testid=game-card]") >= 48, 30, "first page of games")
    d.wait(lambda: sidebar_count("PlayStation", "Portable") == "3", 30, "sidebar counting games, not files")
    check("sidebar counts games, not files (PlayStation: 3, the server says 5 files)", sidebar_count("PlayStation", "Portable") == "3", str(sidebar_count("PlayStation", "Portable")))
    check("All games counts 67", d.text(d.find("[data-testid=nav-all] .count")) == "67", d.text(d.find("[data-testid=nav-all] .count")))
    reqs = roms_requests()
    check("the library was requested grouped", reqs and all("group_by_meta_id=true" in l for l in reqs), str(reqs[:2]))
    check("the library was requested in big pages, not 48 at a time", all("limit=500" in l for l in reqs), str(reqs[:2]))
    check("the server was never asked to filter or search", not any("platform_ids=" in l or "search_term=" in l for l in reqs), str(reqs))
    before = len(reqs)

    print("== One card per game, with an indication that there are more versions")
    d.click(nav_with("PlayStation", "Portable"))
    d.wait(lambda: count("[data-testid=game-card]") == 3, 10, "PlayStation games")
    check("PlayStation lists 3 games, not 5 files", count("[data-testid=game-card]") == 3, str(count("[data-testid=game-card]")))
    check("exactly one card says it has versions", count("[data-testid=versions-badge]") == 1, str(count("[data-testid=versions-badge]")))
    spyro = next(c for c in d.find_all("[data-testid=game-card]") if "Spyro" in d.text(c))
    check("it is the Spyro card, and says 3 versions", "3 versions" in d.text(spyro), d.text(spyro))
    check("no installed badge before anything is installed", count(".badge-installed") == 0)
    shot("v1-playstation")
    check("switching platform didn't refetch the library", len(roms_requests()) == before, f"{before} -> {len(roms_requests())}")

    type_into("[data-testid=search]", "japan", clear=False)
    d.wait(lambda: count("[data-testid=game-card]") == 1, 10, "search results")
    check("searching finds a game through a version that isn't the one shown", "Spyro" in d.text(d.find("[data-testid=game-card]")))
    d.keys(d.find("[data-testid=search]"), "\ue009a\ue000"); d.keys(d.find("[data-testid=search]"), "\ue003")
    d.wait(lambda: count("[data-testid=game-card]") == 3, 10, "search cleared")

    print("== The drawer offers the other versions; the server's pick is the default")
    open_spyro()
    sel = version_select()
    check("dropdown lists the three versions, the server's pick first", [o[0] for o in sel["options"]] == ["103", "113", "123"], str(sel))
    check("it opens on the server's pick", sel["value"] == "103", str(sel))
    texts = [o[1] for o in sel["options"]]
    check("the pick is marked default and labelled from its file name", texts[0] == "USA — default", str(texts))
    check("the others are labelled by region and language", texts[1:] == ["Europe · En,Fr,De", "Japan"], str(texts))
    check("Install is offered, Play is not", d.find("[data-testid=install]") is not None and d.find("[data-testid=play]") is None)
    shot("v2-drawer")

    print("== Installing a version that isn't the default")
    choose_version("123")
    d.wait(lambda: (version_select() or {}).get("value") == "123", 5, "Japan selected")
    check("still offers Install for Japan", d.find("[data-testid=install]") is not None)
    install_selected()
    japan = f"{HOME}/Games/RomM/psx/Spyro the Dragon (Japan).cue"
    check("the Japan file is on disk", os.path.exists(japan) and os.path.getsize(japan) == 2_000_000, japan)
    log = server_log()
    check("Japan's ROM was downloaded, not the default's", "/api/roms/123/content/" in log and "/api/roms/103/content/" not in log)
    check("the dropdown marks Japan installed", ["123", "Japan — installed"] in version_select()["options"], str(version_select()))
    check("the default stays not installed", not os.path.exists(f"{HOME}/Games/RomM/psx/Spyro the Dragon (USA).cue"))

    print("== The game shows as installed, and the drawer remembers which version")
    close_drawer()
    check("the card shows the game as installed though its default isn't", count("[data-testid=game-card] .badge-installed") == 1, str(count(".badge-installed")))
    open_spyro()
    sel = version_select()
    check("reopening lands on the installed version", sel["value"] == "123", str(sel))
    check("Play is there", d.find("[data-testid=play]") is not None)
    choose_version("103")
    d.wait(lambda: d.find("[data-testid=install]") is not None, 10, "Install for the default")
    check("switching to another version offers Install again (installs are per version)", d.find("[data-testid=play]") is None)
    install_selected()
    check("the default can be installed alongside", os.path.exists(f"{HOME}/Games/RomM/psx/Spyro the Dragon (USA).cue"))
    close_drawer()

    print("== Installed list: two copies of one game are told apart")
    d.click(d.find("[data-testid=nav-installed]"))
    d.wait(lambda: count("[data-testid=game-card]") == 2, 10, "installed list")
    subs = sorted(d.text(e) for e in d.find_all("[data-testid=game-card] .game-sub"))
    check("both copies are listed, each with its version", any("Japan" in t for t in subs) and any("USA" in t for t in subs), str(subs))
    check("the installed list doesn't show a version dropdown or badge", count("[data-testid=versions-badge]") == 0)
    shot("v3-installed")

    print("== Refresh")
    d.click(d.find("[data-testid=nav-all]"))
    d.wait(lambda: count("[data-testid=game-card]") >= 48, 10, "all games")
    n = len(roms_requests())
    d.click(d.find("[data-testid=refresh-library]"))
    d.wait(lambda: len(roms_requests()) > n, 15, "refetch")
    d.wait(lambda: count("[data-testid=game-card]") >= 48, 15, "list back")
    check("the refresh button refetches the library", len(roms_requests()) > n)
except Exception:
    traceback.print_exc()
    try: shot("v-failure")
    except Exception: pass
    check("journey ran to the end", False, "see traceback above")
finally:
    try: d.quit()
    except Exception: pass

failed = [r for r in results if not r[1]]
print(f"\n{len(results) - len(failed)}/{len(results)} checks passed")
sys.exit(1 if failed else 0)
