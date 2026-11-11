// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * Hiding a platform from the game browser, through the real app against a fake backend that behaves
 * like the real one: `platforms` carries each platform's `hidden`, `games` leaves hidden platforms'
 * games out, and `set_platform_hidden` is what remembers a choice. What this guards: ticking the box on
 * the Emulators page takes the platform, its count and its games out of the browser at once, the
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
        const { platformId, installedOnly, search, offset, limit } = args.args;
        let items = GAMES.filter((g) => !hidden.has(g.platformSlug));
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
const sidebar = () => [...document.querySelectorAll(".platform-list .nav-name")].map((e) => e.textContent);
const allCount = () => q("[data-testid=nav-all] .count")?.textContent;
const cards = () => [...document.querySelectorAll("[data-testid=game-card]")].map((c) => c.getAttribute("aria-label")!.split(",")[0]);
const showBox = (slug: string) => tid(`show-${slug}`) as HTMLInputElement;
const calls = (cmd: string) => h.calls.filter((c) => c.cmd === cmd);

async function openEmulators() {
  await click(tid("nav-emulators"));
  await until(() => !!showBox("gba"), "the Emulators page");
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
