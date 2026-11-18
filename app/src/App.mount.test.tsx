// @vitest-environment jsdom
import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Game } from "./types";

/**
 * Mounts the real app the way main.tsx does, against a fake Tauri backend, and replays startup:
 * platforms, then a slow library load that reports progress, then the second platforms fetch.
 * The failure this guards against is the one that is hardest to diagnose: an exception while
 * rendering unmounts the whole tree and the window is just empty and dark, with nothing on screen.
 */
const h = vi.hoisted(() => {
  const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
  return {
    listeners,
    emit: (name: string, payload: unknown) => listeners.get(name)?.forEach((f) => f({ payload })),
    calls: [] as string[],
    backend: null as null | ((cmd: string, args: any) => Promise<unknown>),
  };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args: unknown) => {
    h.calls.push(cmd);
    return h.backend!(cmd, args);
  },
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => {
    if (!h.listeners.has(name)) h.listeners.set(name, new Set());
    h.listeners.get(name)!.add(cb);
    return Promise.resolve(() => h.listeners.get(name)?.delete(cb));
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { GamepadProvider } from "./gamepad/GamepadProvider";
import { StoreProvider } from "./store";

(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

/** Every live IntersectionObserver callback: "scrolling" calls them all with the element visible. */
const observers = new Set<(e: { isIntersecting: boolean }[]) => void>();
const scrollDown = () => act(async () => { [...observers].forEach((cb) => cb([{ isIntersecting: true }])); await new Promise((r) => setTimeout(r, 0)); });

const flush = () => act(async () => { await new Promise((r) => setTimeout(r, 0)); });
const until = async (cond: () => boolean, what: string, ms = 3000) => {
  const end = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}\n${document.body.innerHTML.slice(0, 600)}`);
    await act(async () => { await new Promise((r) => setTimeout(r, 10)); });
  }
};

const platform = (id: number, name: string, romCount: number) => ({
  id, slug: `p${id}`, name, romCount, firmwareCount: 0, emulator: "dolphin", emulatorName: "Dolphin", emulatorInstalled: true,
  choices: [], selected: "", automaticLabel: null, ignoredOverride: null,
});

/** 130 games on platform 1; every 5th has three versions. */
const GAMES: Game[] = Array.from({ length: 130 }, (_, i) => {
  const id = 1000 + i * 10;
  const multi = i % 5 === 0;
  return {
    id, name: `Game ${String(i).padStart(3, "0")}`, platformId: 1, platformSlug: "p1", installed: i === 5, sizeBytes: 1_000_000, multiFile: false, coverPath: null,
    versions: multi ? [
      { id, label: "USA", isDefault: true, installed: false },
      { id: id + 1, label: "Europe · En,Fr,De", isDefault: false, installed: false },
      { id: id + 2, label: "Japan", isDefault: false, installed: i === 5 },
    ] : [],
    versionLabel: null,
  };
});

let root: Root | null = null;
let errors: unknown[] = [];
let libraryDone: () => void;
let libraryFails: (e: string) => void;
const originalConsoleError = console.error;

function fakeBackend(opts: { platforms?: unknown } = {}) {
  let loaded = false;
  const library = new Promise<number>((res, rej) => {
    libraryDone = () => { loaded = true; res(GAMES.length); };
    libraryFails = (e) => rej(e);
  });
  library.catch(() => {});
  h.backend = async (cmd, args) => {
    switch (cmd) {
      case "status": return { loggedIn: true, serverUrl: "http://x", deviceName: "d", version: "0", warnings: [] };
      case "platforms": return "platforms" in opts ? opts.platforms : [platform(1, "GameCube", loaded ? GAMES.length : 200), platform(2, "Empty", 0)];
      case "load_library": return library;
      case "games": {
        await library;
        const { offset, limit, installedOnly } = args.args;
        const src = installedOnly ? GAMES.filter((g) => g.installed).map((g) => ({ ...g, versions: [] })) : GAMES;
        return { items: src.slice(offset, offset + limit), total: src.length };
      }
      case "cover": return null;
      case "game_details": return { installed: false, installPath: null, emulator: { id: "dolphin", name: "Dolphin", installed: true, problem: null }, saves: [], states: [], statesError: null, statesEnabled: true };
      case "achievements": return { state: "no_set", ra_username: null, total: 0, earned: 0, earned_hardcore: 0, points_total: 0, points_earned: 0, award: null, last_earned: null, progress_missing: false, achievements: [], stale: false, fetched_at: null };
      default: return undefined;
    }
  };
}

beforeEach(() => {
  errors = [];
  h.calls.length = 0;
  h.listeners.clear();
  // Capture what React would have put in the console, and uncaught errors.
  console.error = (...a: unknown[]) => { errors.push(a.map(String).join(" ")); };
  window.addEventListener("error", (e) => { errors.push(e.error ?? e.message); e.preventDefault(); });
  // jsdom has no IntersectionObserver: report every watched element as visible, so scrolling loads pages as fast as it can.
  observers.clear();
  (globalThis as any).IntersectionObserver = class {
    constructor(private cb: (e: { isIntersecting: boolean }[]) => void) {}
    observe() { observers.add(this.cb); setTimeout(() => this.cb([{ isIntersecting: true }]), 0); }
    disconnect() { observers.delete(this.cb); }
    unobserve() { observers.delete(this.cb); }
  };
  (globalThis as any).ResizeObserver = class { observe() {} disconnect() {} unobserve() {} };
  Element.prototype.scrollTo = () => {};
  Element.prototype.scrollIntoView = () => {};
  document.body.innerHTML = '<div id="root"></div>';
});

afterEach(async () => {
  console.error = originalConsoleError;
  if (root) await act(async () => root!.unmount());
  root = null;
});

async function mount() {
  root = createRoot(document.getElementById("root")!);
  await act(async () => {
    root!.render(
      <React.StrictMode>
        <ErrorBoundary scope="app"><StoreProvider><GamepadProvider><App /></GamepadProvider></StoreProvider></ErrorBoundary>
      </React.StrictMode>,
    );
  });
}

const cards = () => document.querySelectorAll("[data-testid=game-card]");
const shellStillThere = () => !!document.querySelector(".shell");
const realErrors = () => errors.filter((e) => !/not wrapped in act|ReactDOMTestUtils|Warning: An update to/.test(String(e)));

describe("startup with a library that loads slowly", () => {
  it("keeps the window alive through loading, progress events and the second platforms fetch", async () => {
    fakeBackend();
    await mount();
    await until(shellStillThere, "the shell");
    expect(document.querySelector("[data-testid=loading]")).not.toBeNull();

    // Progress arrives while the first list is waiting on the library.
    for (const n of [500, 1000, 1500]) await act(async () => h.emit("romm://library", { phase: "loading", loaded: n, total: 1500, error: null }));
    expect(document.querySelector("[data-testid=loading]")!.textContent).toContain("1,500 of 1,500");
    await act(async () => h.emit("romm://library", { phase: "loading", loaded: 0, total: null, error: null }));
    expect(shellStillThere()).toBe(true);

    await act(async () => { libraryDone(); h.emit("romm://library", { phase: "finished", loaded: 130, total: 130, error: null }); });
    await until(() => cards().length >= 48, "the first page of cards");
    await flush();
    for (let i = 0; i < 10 && cards().length < 130; i++) await scrollDown();
    await until(() => cards().length === 130, "every page, by scrolling to the bottom");

    expect(shellStillThere()).toBe(true);
    expect(document.querySelectorAll("[data-testid=versions-badge]").length).toBe(26);
    expect(document.querySelector(".sidebar")!.textContent).toContain("130"); // counts games once the library is in
    expect(h.calls.filter((c) => c === "platforms").length).toBe(2);
    expect(realErrors()).toEqual([]);
  });

  it("survives the library failing to load", async () => {
    fakeBackend();
    await mount();
    await until(shellStillThere, "the shell");
    await act(async () => { libraryFails("connection refused"); });
    await flush(); await flush();
    expect(shellStillThere()).toBe(true);
    expect(document.querySelector(".banner-error")?.textContent ?? "").toContain("connection refused");
    expect(realErrors().filter((e) => !String(e).includes("connection refused"))).toEqual([]);
  });

  it("survives a progress event after the list is gone, and with odd payloads", async () => {
    fakeBackend();
    await mount();
    await until(shellStillThere, "the shell");
    await act(async () => { libraryDone(); });
    await until(() => cards().length >= 48, "cards");
    for (const p of [{ phase: "loading", loaded: 5, total: null, error: null }, { phase: "failed", loaded: 0, total: null, error: "x" }, { phase: "finished", loaded: 0, total: 0, error: null }])
      await act(async () => h.emit("romm://library", p));
    expect(shellStillThere()).toBe(true);
    expect(realErrors()).toEqual([]);
  });
});

describe("using the app after it loaded", () => {
  async function loaded() {
    fakeBackend();
    await mount();
    await until(shellStillThere, "the shell");
    await act(async () => { libraryDone(); });
    await until(() => cards().length >= 48, "cards");
  }

  it("opens a game with versions, switches version, and closes", async () => {
    await loaded();
    const card = [...cards()].find((c) => c.textContent?.includes("Game 000"))!;
    await act(async () => (card as HTMLElement).click());
    await until(() => !!document.querySelector("[data-testid=version-select]"), "the version dropdown");
    const sel = document.querySelector("[data-testid=version-select]") as HTMLSelectElement;
    expect([...sel.options].map((o) => o.value)).toEqual(["1000", "1001", "1002"]);
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(sel, "1002");
      sel.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await flush();
    expect((document.querySelector("[data-testid=version-select]") as HTMLSelectElement).value).toBe("1002");
    expect(h.calls.filter((c) => c === "game_details").length).toBeGreaterThanOrEqual(2);
    expect(shellStillThere()).toBe(true);
    await act(async () => { window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" })); });
    await until(() => !document.querySelector("[data-testid=drawer]"), "the drawer closing");
    expect(realErrors()).toEqual([]);
  });

  it("shows a game as installed when only one of its other versions is", async () => {
    await loaded();
    const card = [...cards()].find((c) => c.textContent?.includes("Game 005"))!;
    expect(card.getAttribute("data-installed")).toBe("true");
    await act(async () => (card as HTMLElement).click());
    await until(() => !!document.querySelector("[data-testid=version-select]"), "the dropdown");
    expect((document.querySelector("[data-testid=version-select]") as HTMLSelectElement).value).toBe("1052"); // the installed Japan copy
    expect(realErrors()).toEqual([]);
  });

  it("refreshes the library from the button", async () => {
    await loaded();
    h.calls.length = 0;
    await act(async () => (document.querySelector("[data-testid=refresh-library]") as HTMLElement).click());
    await until(() => h.calls.includes("load_library"), "load_library(force)");
    await flush(); await flush();
    expect(cards().length).toBeGreaterThanOrEqual(48);
    expect(h.calls.filter((c) => c === "platforms").length).toBeGreaterThanOrEqual(1);
    expect(shellStillThere()).toBe(true);
    expect(realErrors()).toEqual([]);
  });

  it("switches between All games, Installed and a platform without losing the window", async () => {
    await loaded();
    for (const t of ["nav-installed", "nav-all"]) {
      await act(async () => (document.querySelector(`[data-testid=${t}]`) as HTMLElement).click());
      await flush(); await flush();
      expect(shellStillThere()).toBe(true);
    }
    await until(() => cards().length >= 48, "cards after switching back");
    const platformBtn = [...document.querySelectorAll(".platform-list .nav")].find((b) => b.textContent?.includes("GameCube"))!;
    await act(async () => (platformBtn as HTMLElement).click());
    await until(() => cards().length >= 48, "platform list");
    expect(realErrors()).toEqual([]);
  });
});

describe("a crash outside the current view", () => {
  // The sidebar sits outside the per-view error boundary, so before the app had a boundary of its own,
  // an exception there left an empty window with no message at all.
  it("shows what went wrong instead of an empty window", async () => {
    fakeBackend({ platforms: null }); // not a list: the sidebar can't render it
    await mount();
    await flush(); await flush();
    const root = document.getElementById("root")!;
    expect(root.textContent?.trim() ?? "").not.toBe("");
    expect(root.querySelector("[data-testid=app-error]")).not.toBeNull();
    expect(root.textContent).toContain("filter"); // the actual message: "…platforms.filter is not a function"
    expect(root.querySelector("[data-testid=app-error-reload]")).not.toBeNull();
  });
});
