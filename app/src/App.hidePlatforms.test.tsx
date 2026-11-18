// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * Hiding a platform from the game browser, through the real app against a fake backend that behaves
 * like the real one: `platforms` carries each platform's `hidden`, `games` leaves hidden platforms'
 * games out, and `set_platform_hidden` is what remembers a choice. What this guards: ticking the box on
 * the Emulators page's mapping tab takes the platform, its count and its games out of the browser at once, the
 * platform stays on that page so it can be shown again, and a failure puts everything back.
 */
const h = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: any }[],
  backend: null as null | ((cmd: string, args: any) => Promise<unknown>),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args: any) => {
    h.calls.push({ cmd, args });
    return h.backend!(cmd, args);
  },
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

import App from "./App";
import { GamepadProvider } from "./gamepad/GamepadProvider";
import { StoreProvider } from "./store";

(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

const flush = () => act(async () => { await new Promise((r) => setTimeout(r, 0)); });
const until = async (cond: () => boolean, what: string, ms = 3000) => {
  const end = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}\n${document.body.innerHTML.slice(0, 500)}`);
    await act(async () => { await new Promise((r) => setTimeout(r, 10)); });
  }
};

const PLATFORMS = [
  { id: 1, slug: "gba", name: "Game Boy Advance", romCount: 2 },
  { id: 2, slug: "arcade", name: "Arcade", romCount: 1 },
  { id: 3, slug: "snes", name: "Super Nintendo", romCount: 3 },
  { id: 4, slug: "dc", name: "Dreamcast", romCount: 0 }, // nothing on it, so hiding it never shows up anywhere
];
const game = (id: number, name: string, platformId: number, platformSlug: string, installed = false) => ({
  id, name, platformId, platformSlug, installed, sizeBytes: 1, multiFile: false, coverPath: null, versions: [], versionLabel: null,
});
const GAMES = [
  game(1, "Pokemon Ruby", 1, "gba"),
  game(2, "Metroid Fusion", 1, "gba", true),
  game(3, "Pac-Man", 2, "arcade", true),
  game(4, "Chrono Trigger", 3, "snes"),
  game(5, "Super Metroid", 3, "snes"),
  game(6, "Zelda", 3, "snes"),
];

let hidden: Set<string>;
let platformsGate: Promise<void> | null;
let failToSave: string | null;
let root: Root;
let host: HTMLElement;

function fakeBackend() {
  h.backend = async (cmd, args) => {
    switch (cmd) {
      case "status": return { loggedIn: true, serverUrl: "http://x", deviceName: "d", version: "0", warnings: [] };
      case "platforms":
        if (platformsGate) await platformsGate; // a slow RomM server
        return PLATFORMS.map((p) => ({
          ...p, firmwareCount: 0, emulator: "dolphin", emulatorName: "Dolphin", emulatorInstalled: true,
          choices: [], selected: "", automaticLabel: null, ignoredOverride: null, hidden: hidden.has(p.slug),
        }));
      case "load_library": return GAMES.length;
      case "games": {
        const { platformId, installedOnly, hiddenOnly, search, offset, limit } = args.args;
        // The real backend lists a hidden platform's games in exactly one place: the hidden view.
        let items = GAMES.filter((g) => hiddenOnly ? hidden.has(g.platformSlug) : !hidden.has(g.platformSlug));
        if (installedOnly) items = items.filter((g) => g.installed);
        if (platformId != null) items = items.filter((g) => g.platformId === platformId);
        if (search) items = items.filter((g) => g.name.toLowerCase().includes(String(search).toLowerCase()));
        return { items: items.slice(offset, offset + limit), total: items.length }; // paged, as the real one is
      }
      case "emulators": return [];
      case "set_platform_hidden":
        if (failToSave) throw failToSave;
        if (args.hidden) hidden.add(args.slug); else hidden.delete(args.slug);
        return undefined;
      case "cover": return null;
      default: return undefined;
    }
  };
}

beforeEach(() => {
  h.calls.length = 0;
  hidden = new Set();
  platformsGate = null;
  failToSave = null;
  (globalThis as any).IntersectionObserver = class {
    constructor(private cb: (e: { isIntersecting: boolean }[]) => void) {}
    observe() { setTimeout(() => this.cb([{ isIntersecting: true }]), 0); }
    disconnect() {} unobserve() {}
  };
  (globalThis as any).ResizeObserver = class { observe() {} disconnect() {} unobserve() {} };
  Element.prototype.scrollTo = () => {};
  Element.prototype.scrollIntoView = () => {};
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

async function mount() {
  fakeBackend();
  await act(async () => {
    root.render(<StoreProvider><GamepadProvider><App /></GamepadProvider></StoreProvider>);
  });
  await flush(); await flush();
}

const q = (sel: string) => document.querySelector<HTMLElement>(sel);
const tid = (id: string) => q(`[data-testid="${id}"]`);
const click = (el: Element | null) => act(async () => { (el as HTMLElement).click(); });
const sidebar = () => [...document.querySelectorAll(".platform-list .nav:not(.nav-hidden) .nav-name")].map((e) => e.textContent);
const hiddenEntry = () => tid("nav-hidden");
const allCount = () => q("[data-testid=nav-all] .count")?.textContent;
const cards = () => [...document.querySelectorAll("[data-testid=game-card]")].map((c) => c.getAttribute("aria-label")!.split(",")[0]);
const showBox = (slug: string) => tid(`show-${slug}`) as HTMLInputElement;
const calls = (cmd: string) => h.calls.filter((c) => c.cmd === cmd);

async function openEmulators() {
  await click(tid("nav-emulators"));
  await click(tid("tab-mapping")); // the show/hide boxes are on the Emulator mapping tab
  await until(() => !!showBox("gba"), "the Emulator mapping tab");
}
async function openAll() {
  await click(tid("nav-all"));
  await until(() => cards().length > 0, "the game list");
}

describe("with nothing hidden", () => {
  it("offers every platform and game, and the Emulators page has a ticked box for each", async () => {
    await mount();
    await until(() => cards().length === 6, "games");
    expect(sidebar()).toEqual(["Arcade", "Game Boy Advance", "Super Nintendo"]);
    expect(allCount()).toBe("6");
    await openEmulators();
    for (const slug of ["gba", "arcade", "snes"]) expect(showBox(slug).checked).toBe(true);
  });
});

describe("a platform hidden before the app started", () => {
  it("is not in the sidebar or the count, and none of its games are listed", async () => {
    hidden = new Set(["arcade"]);
    await mount();
    await until(() => cards().length === 5, "games");
    expect(sidebar()).toEqual(["Game Boy Advance", "Super Nintendo"]);
    expect(allCount()).toBe("5");
    expect(cards()).not.toContain("Pac-Man");
  });

  it("is still on the Emulators page, unticked and marked, so it can be shown again", async () => {
    hidden = new Set(["arcade"]);
    await mount();
    await openEmulators();
    expect(showBox("arcade").checked).toBe(false);
    expect(tid("hidden-arcade")!.textContent).toContain("Hidden from the library");
    expect(showBox("gba").checked).toBe(true);
  });
});

describe("unticking a platform on the Emulators page", () => {
  it("saves it, and takes the platform and its count out of the sidebar at once, before the server answers", async () => {
    await mount();
    await openEmulators();

    // The RomM server is slow to answer the refetch the page asks for afterwards.
    let release!: () => void;
    platformsGate = new Promise<void>((r) => { release = r; });
    await click(showBox("snes"));
    await flush();

    expect(calls("set_platform_hidden").map((c) => c.args)).toEqual([{ slug: "snes", hidden: true }]);
    expect(sidebar()).toEqual(["Arcade", "Game Boy Advance"]);
    expect(allCount()).toBe("3");
    expect(showBox("snes").checked).toBe(false);
    expect(tid("hidden-snes")).not.toBeNull();

    release();
    await flush(); await flush();
    expect(sidebar()).toEqual(["Arcade", "Game Boy Advance"]); // the refetch agrees
    expect(showBox("snes").checked).toBe(false);
  });

  it("leaves the platform's games out of All games, and out of a search, when you go back", async () => {
    await mount();
    await openEmulators();
    await click(showBox("snes"));
    await flush();
    await openAll();
    expect(cards().sort()).toEqual(["Metroid Fusion", "Pac-Man", "Pokemon Ruby"]);
  });

  it("leaves its installed games out of Installed too", async () => {
    await mount();
    await openEmulators();
    await click(showBox("arcade")); // Pac-Man is installed
    await flush();
    await click(tid("nav-installed"));
    await until(() => cards().length > 0, "the installed list");
    expect(cards()).toEqual(["Metroid Fusion"]);
  });

  it("can be undone by ticking the box again", async () => {
    hidden = new Set(["gba"]);
    await mount();
    await openEmulators();
    expect(sidebar()).toEqual(["Arcade", "Super Nintendo"]);
    await click(showBox("gba"));
    await flush(); await flush();
    expect(calls("set_platform_hidden").map((c) => c.args)).toEqual([{ slug: "gba", hidden: false }]);
    expect(sidebar()).toEqual(["Arcade", "Game Boy Advance", "Super Nintendo"]);
    expect(allCount()).toBe("6");
    expect(tid("hidden-gba")).toBeNull();
  });

  it("puts everything back and says so when it can't be saved", async () => {
    await mount();
    await openEmulators();
    failToSave = "disk full";
    await click(showBox("snes"));
    await flush(); await flush();
    expect(sidebar()).toEqual(["Arcade", "Game Boy Advance", "Super Nintendo"]);
    expect(allCount()).toBe("6");
    expect(showBox("snes").checked).toBe(true);
    const alert = q("[role=alert]");
    expect(alert?.textContent).toContain("Couldn't change that platform");
    expect(alert?.textContent).toContain("disk full");
  });

  it("changes nothing about which emulator plays the platform, or what else is on the page", async () => {
    await mount();
    await openEmulators();
    await click(showBox("snes"));
    await flush();
    expect(calls("set_platform_choice")).toEqual([]);
    expect(q("[aria-label='Emulator for Super Nintendo']")).toBeNull(); // no emulator choices in this fixture, and none appeared
    expect(tid("show-gba")).not.toBeNull();
  });
});

describe("the open platform", () => {
  it("a platform that is open when it gets hidden falls back to all games, not an empty list", async () => {
    await mount();
    await until(() => sidebar().length === 3, "sidebar");
    await click([...document.querySelectorAll<HTMLElement>(".platform-list .nav")].find((b) => b.textContent?.includes("Super Nintendo"))!);
    await until(() => cards().length === 3, "the SNES list");
    expect(q(".topbar h1")?.textContent).toBe("Super Nintendo");

    // Hidden somewhere else (another window, a synced config) while it is open; refreshing the library
    // asks for the platform list again, which now says so.
    hidden.add("snes");
    await click(tid("refresh-library"));
    await until(() => q(".topbar h1")?.textContent === "All games", "falling back to all games");
    await until(() => cards().length === 3, "the remaining games");
    expect(sidebar()).toEqual(["Arcade", "Game Boy Advance"]);
    expect(cards()).not.toContain("Chrono Trigger");
  });

  it("a platform that stays shown stays open through a refresh", async () => {
    await mount();
    await until(() => sidebar().length === 3, "sidebar");
    await click([...document.querySelectorAll<HTMLElement>(".platform-list .nav")].find((b) => b.textContent?.includes("Game Boy Advance"))!);
    await until(() => cards().length === 2, "the GBA list");
    hidden.add("snes"); // some other platform
    await click(tid("refresh-library"));
    await flush(); await flush();
    expect(q(".topbar h1")?.textContent).toBe("Game Boy Advance");
  });
});

describe("the Hidden platforms entry", () => {
  const last = <T,>(xs: T[]): T | undefined => xs[xs.length - 1];
  const lastInList = () => last([...document.querySelectorAll(".platform-list > *")]);
  const cardFor = (name: string) => [...document.querySelectorAll<HTMLElement>("[data-testid=game-card]")].find((c) => c.getAttribute("aria-label")!.startsWith(name))!;
  const subtitle = () => q(".topbar .muted.small")?.textContent;
  async function openHiddenView() {
    await click(hiddenEntry());
    await until(() => q(".topbar h1")?.textContent === "Hidden platforms", "the hidden view");
    await until(() => cards().length > 0, "its games");
  }
  async function typeInto(selector: string, value: string) {
    const el = q(selector) as HTMLInputElement;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
  }

  it("isn't offered while nothing is hidden", async () => {
    await mount();
    await until(() => sidebar().length === 3, "sidebar");
    expect(hiddenEntry()).toBeNull();
  });

  it("is the last entry of the platform list, saying how many platforms are hidden", async () => {
    hidden = new Set(["arcade"]);
    await mount();
    await until(() => sidebar().length === 2, "sidebar");
    expect(hiddenEntry()!.textContent).toBe("Hidden platforms (1)");
    expect(lastInList()).toBe(hiddenEntry());
    expect(sidebar()).toEqual(["Game Boy Advance", "Super Nintendo"]);
  });

  it("counts platforms, not their games", async () => {
    hidden = new Set(["snes"]); // three games on one platform
    await mount();
    await until(() => sidebar().length === 2, "sidebar");
    expect(hiddenEntry()!.textContent).toBe("Hidden platforms (1)");
  });

  it("counts every hidden platform that has games", async () => {
    hidden = new Set(["arcade", "snes"]);
    await mount();
    await until(() => sidebar().length === 1, "sidebar");
    expect(hiddenEntry()!.textContent).toBe("Hidden platforms (2)");
  });

  it("leaves out a hidden platform with no games, and isn't offered when that is all there is", async () => {
    hidden = new Set(["dc"]);
    await mount();
    await until(() => sidebar().length === 3, "sidebar");
    expect(hiddenEntry()).toBeNull();
  });

  it("doesn't count an empty hidden platform among the ones that have games", async () => {
    hidden = new Set(["dc", "arcade"]);
    await mount();
    await until(() => sidebar().length === 2, "sidebar");
    expect(hiddenEntry()!.textContent).toBe("Hidden platforms (1)");
  });

  it("opens the games of every hidden platform together, and nothing else", async () => {
    hidden = new Set(["arcade", "snes"]);
    await mount();
    await until(() => sidebar().length === 1, "sidebar");
    await openHiddenView();
    await until(() => cards().length === 4, "all four hidden games");
    expect(cards().sort()).toEqual(["Chrono Trigger", "Pac-Man", "Super Metroid", "Zelda"]);
    expect(subtitle()).toBe("4 games");
    expect(hiddenEntry()!.classList.contains("active")).toBe(true);
    expect(last(calls("games"))!.args.args).toMatchObject({ hiddenOnly: true, installedOnly: false });
  });

  it("shows which platform each game is on, and whether it is installed", async () => {
    hidden = new Set(["arcade", "snes"]);
    await mount();
    await until(() => sidebar().length === 1, "sidebar");
    await openHiddenView();
    await until(() => cards().length === 4, "games");
    expect(cardFor("Pac-Man").querySelector(".game-sub")!.textContent).toMatch(/^Arcade\b/); // then the size, which these fixtures give as 1 B
    expect(cardFor("Pac-Man").dataset.installed).toBe("true");
    expect(cardFor("Chrono Trigger").querySelector(".game-sub")!.textContent).toMatch(/^Super Nintendo\b/);
    expect(cardFor("Chrono Trigger").dataset.installed).toBe("false");
  });

  it("is the only place those games are listed", async () => {
    hidden = new Set(["arcade", "snes"]);
    await mount();
    await until(() => sidebar().length === 1, "sidebar");
    await openAll();
    await until(() => cards().length === 2, "the visible games");
    expect(cards().sort()).toEqual(["Metroid Fusion", "Pokemon Ruby"]);
    await click(tid("nav-installed"));
    await until(() => cards().length === 1, "the installed list");
    expect(cards()).toEqual(["Metroid Fusion"]); // Pac-Man is installed, but its platform is hidden
    await openHiddenView();
    await until(() => cards().length === 4, "games");
    expect(cards()).toContain("Pac-Man");
  });

  it("searches only the hidden games", async () => {
    hidden = new Set(["snes"]);
    await mount();
    await until(() => sidebar().length === 2, "sidebar");
    await openHiddenView();
    await until(() => cards().length === 3, "games");
    await typeInto("[data-testid=search]", "metroid");
    // "Metroid Fusion" matches too, but it is on a platform that isn't hidden.
    await until(() => cards().length === 1, "the search result");
    expect(cards()).toEqual(["Super Metroid"]);
  });

  it("obeys the platform filter the way a platform's name does", async () => {
    hidden = new Set(["snes"]);
    await mount();
    await until(() => sidebar().length === 2, "sidebar");
    await typeInto("input[aria-label='Filter platforms']", "hidden");
    expect(hiddenEntry()).not.toBeNull();
    await typeInto("input[aria-label='Filter platforms']", "game boy");
    expect(hiddenEntry()).toBeNull();
    expect(sidebar()).toEqual(["Game Boy Advance"]);
    await typeInto("input[aria-label='Filter platforms']", "");
    expect(hiddenEntry()).not.toBeNull();
  });

  it("is in the controller's platform cycle, after the last platform", async () => {
    hidden = new Set(["snes"]);
    await mount();
    await until(() => sidebar().length === 2, "sidebar");
    const cycle = [...document.querySelectorAll(".sidebar .nav-group:not(.bottom) .nav, .sidebar .platform-list .nav")];
    expect(last(cycle)).toBe(hiddenEntry());
  });

  it("appears at once when a platform is hidden on the Emulators page, and goes when the last one is shown again", async () => {
    await mount();
    await until(() => sidebar().length === 3, "sidebar");
    expect(hiddenEntry()).toBeNull();
    await openEmulators();

    // The RomM server is slow to answer the refetch the page asks for afterwards.
    let release!: () => void;
    platformsGate = new Promise<void>((r) => { release = r; });
    await click(showBox("snes"));
    await flush();
    expect(hiddenEntry()!.textContent).toBe("Hidden platforms (1)");
    await click(showBox("arcade"));
    await flush();
    expect(hiddenEntry()!.textContent).toBe("Hidden platforms (2)");

    await click(showBox("snes"));
    await click(showBox("arcade"));
    await flush();
    expect(hiddenEntry()).toBeNull();
    release();
    await flush(); await flush();
    expect(hiddenEntry()).toBeNull();
  });

  it("falls back to all games when nothing it shows is hidden any more", async () => {
    hidden = new Set(["snes"]);
    await mount();
    await until(() => sidebar().length === 2, "sidebar");
    await openHiddenView();

    // Shown again somewhere else (another window, a synced config); refreshing asks for the platform list again.
    hidden.clear();
    await click(tid("refresh-library"));
    await until(() => q(".topbar h1")?.textContent === "All games", "falling back to all games");
    expect(hiddenEntry()).toBeNull();
  });
});
