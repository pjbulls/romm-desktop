// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Game, VersionSize } from "../types";

(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

const h = vi.hoisted(() => ({
  sizes: null as null | (() => Promise<unknown>),
  calls: [] as { cmd: string; args: any }[],
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args: any) => {
    h.calls.push({ cmd, args });
    switch (cmd) {
      case "version_sizes": return h.sizes!();
      case "game_details": return Promise.resolve({ installed: false, installPath: null, emulator: { id: "e", name: "E", installed: true, problem: null }, saves: [], states: [], statesError: null, statesEnabled: true });
      case "achievements": return Promise.resolve({ state: "no_set", ra_username: null, total: 0, earned: 0, earned_hardcore: 0, points_total: 0, points_earned: 0, award: null, last_earned: null, progress_missing: false, achievements: [], stale: false, fetched_at: null });
      default: return Promise.resolve(null);
    }
  },
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));

import { StoreProvider } from "../store";
import { GameDrawer } from "./GameDrawer";

const MB = 1024 * 1024;
const game = (over: Partial<Game> = {}): Game => ({
  id: 10, name: "Spyro", platformId: 1, platformSlug: "psx", installed: false, sizeBytes: 8 * MB, multiFile: false, coverPath: null, versionLabel: null,
  versions: [
    { id: 10, label: "USA", isDefault: true, installed: false },
    { id: 11, label: "Europe · En,Fr,De", isDefault: false, installed: false },
    { id: 12, label: "Japan", isDefault: false, installed: false },
  ],
  ...over,
});
const size = (id: number, mb: number, multiFile = false): VersionSize => ({ id, sizeBytes: mb * MB, multiFile });

let root: Root | null = null;
const flush = () => act(async () => { await new Promise((r) => setTimeout(r, 0)); });
const q = <T extends Element>(css: string) => document.querySelector<T>(css);
const options = () => [...document.querySelectorAll<HTMLOptionElement>("[data-testid=version-select] option")].map((o) => o.textContent);
const header = () => q(".drawer-head .muted")?.textContent ?? q("[data-testid=drawer] .muted")?.textContent ?? "";
const sizeCalls = () => h.calls.filter((c) => c.cmd === "version_sizes");

async function mount(g: Game) {
  root = createRoot(document.getElementById("root")!);
  await act(async () => { root!.render(<StoreProvider><GameDrawer game={g} onClose={() => {}} onGoEmulators={() => {}} /></StoreProvider>); });
  await flush();
}
async function pick(value: string) {
  const sel = q<HTMLSelectElement>("[data-testid=version-select]")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(sel, value);
    sel.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await flush();
}

beforeEach(() => {
  h.calls.length = 0;
  h.sizes = () => Promise.resolve([size(11, 498), size(12, 512, true)]);
  document.body.innerHTML = '<div id="root"></div>';
});
afterEach(async () => {
  if (root) await act(async () => root!.unmount());
  root = null;
});

const PLAIN = ["USA — default", "Europe · En,Fr,De", "Japan"]; // the dropdown names versions; it never carries a size

describe("sizes of the other versions", () => {
  it("asks only for the versions the list gives no size for", async () => {
    await mount(game());
    expect(sizeCalls()).toHaveLength(1);
    expect(sizeCalls()[0].args).toEqual({ ids: [11, 12] });
  });

  it("keeps sizes out of the dropdown, whether or not they have arrived", async () => {
    let arrive!: (v: unknown) => void;
    h.sizes = () => new Promise((r) => { arrive = r; });
    await mount(game());
    expect(options()).toEqual(PLAIN);
    await act(async () => arrive([size(11, 498), size(12, 512)]));
    expect(options()).toEqual(PLAIN);
    expect(options().join(" ")).not.toMatch(/\bMB\b/);
  });

  it("shows the primary's own size in the panel at first, and a picked version's as soon as it is known", async () => {
    let arrive!: (v: unknown) => void;
    h.sizes = () => new Promise((r) => { arrive = r; });
    await mount(game());
    expect(header()).toContain("8 MB");
    await pick("11");
    expect(header()).not.toContain("MB"); // not fetched yet: nothing rather than the primary's
    await act(async () => arrive([size(11, 498), size(12, 512)]));
    expect(header()).toContain("498 MB");
  });

  it("shows the size of the version you pick, not the primary's", async () => {
    await mount(game());
    expect(header()).toContain("8 MB");
    await pick("11");
    expect(header()).toContain("498 MB");
    expect(header()).not.toContain("8 MB ");
    await pick("12");
    expect(header()).toContain("512 MB");
    expect(header()).toContain("multi-file");
    await pick("10");
    expect(header()).toContain("8 MB");
    expect(header()).not.toContain("multi-file");
  });

  it("shows no size, and no error, for a version it couldn't get one for", async () => {
    h.sizes = () => Promise.resolve([size(12, 512)]); // 11 is missing from the answer
    await mount(game());
    await pick("12");
    expect(header()).toContain("512 MB");
    await pick("11");
    expect(header()).not.toContain("MB");
    expect(q("[data-testid=install]")).not.toBeNull(); // the panel works as before
  });

  it("copes with the request failing altogether", async () => {
    h.sizes = () => Promise.reject(new Error("server unreachable"));
    await mount(game());
    expect(options()).toEqual(PLAIN);
    await pick("12");
    expect(header()).not.toContain("MB");
    expect(q("[data-testid=install]")).not.toBeNull();
    expect(q("[data-testid=version-select]")).not.toBeNull();
  });

  it("leaves out a size of zero rather than printing 0 B", async () => {
    h.sizes = () => Promise.resolve([size(11, 0), size(12, 512)]);
    await mount(game());
    await pick("11");
    expect(header()).not.toContain("0 B");
    expect(header()).not.toContain("MB");
  });

  it("makes no request for a game with a single version", async () => {
    await mount(game({ versions: [] }));
    expect(sizeCalls()).toHaveLength(0);
    expect(q("[data-testid=version-select]")).toBeNull();
    expect(header()).toContain("8 MB");
  });

  it("ignores an answer that arrives after the panel was closed", async () => {
    let arrive!: (v: unknown) => void;
    h.sizes = () => new Promise((r) => { arrive = r; });
    const errors: unknown[] = [];
    const spy = vi.spyOn(console, "error").mockImplementation((...a) => { errors.push(a); });
    await mount(game());
    await act(async () => root!.unmount());
    root = null;
    await act(async () => arrive([size(11, 1), size(12, 2)]));
    spy.mockRestore();
    expect(errors).toEqual([]);
  });
});
