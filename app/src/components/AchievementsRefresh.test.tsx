// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Achievement, AchievementsView, Game } from "../types";

(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

/**
 * After a game closes, the backend asks RomM to re-pull RetroAchievements progress and announces
 * that it worked with `romm://achievements`. These tests drive that announcement into an open
 * panel. Each `achievements` call stays pending until the test answers it, so the order in which
 * answers arrive is under the test's control.
 */
const h = vi.hoisted(() => {
  const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
  return {
    listeners,
    emit: (name: string) => listeners.get(name)?.forEach((f) => f({ payload: null })),
    calls: [] as string[],
    asked: [] as { resolve: (v: unknown) => void; reject: (e: unknown) => void }[],
  };
});
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => {
    h.calls.push(cmd);
    if (cmd !== "achievements") return Promise.resolve(null);
    return new Promise((resolve, reject) => h.asked.push({ resolve, reject }));
  },
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => {
    if (!h.listeners.has(name)) h.listeners.set(name, new Set());
    h.listeners.get(name)!.add(cb);
    return Promise.resolve(() => h.listeners.get(name)?.delete(cb));
  },
}));

import { StoreProvider } from "../store";
import { Achievements } from "./Achievements";

const ach = (id: number, earned: boolean): Achievement => ({
  id, title: `Ach ${id}`, description: "", points: 5, kind: "", earned, hardcore: false, earned_at: null, badge: null,
});
/** Three achievements; the first `earned` of them are earned. */
const view = (earned: number): AchievementsView => ({
  state: "ready", ra_username: "pj", total: 3, earned, earned_hardcore: 0, points_total: 15, points_earned: earned * 5, award: null,
  last_earned: null, progress_missing: false, achievements: [1, 2, 3].map((i) => ach(i, i <= earned)), stale: false, fetched_at: null,
});
const game = (id: number): Game => ({
  id, name: "G", platformId: 1, platformSlug: "gba", installed: false, sizeBytes: 0, multiFile: false, coverPath: null, versions: [], versionLabel: null,
});

let root: Root | null = null;
const flush = () => act(async () => { await new Promise((r) => setTimeout(r, 0)); });
const text = () => document.body.textContent ?? "";
const rows = () => document.querySelectorAll("[data-testid=achievement]").length;
const earned = () => document.querySelectorAll("[data-testid=achievement].earned").length;
const reads = () => h.calls.filter((c) => c === "achievements").length;

/** Answer the n-th `achievements` call (in the order they were made). */
const answer = (n: number, v: AchievementsView) => act(async () => { h.asked[n].resolve(v); await Promise.resolve(); });
const fail = (n: number) => act(async () => { h.asked[n].reject("server down"); await Promise.resolve(); });
/** The backend announcing that RomM has re-pulled progress. */
const refreshed = () => act(async () => { h.emit("romm://achievements"); });

async function mount(g = game(1)) {
  root = createRoot(document.getElementById("root")!);
  await act(async () => { root!.render(<StoreProvider><Achievements game={g} /></StoreProvider>); });
  await flush();
}
async function rerender(g: Game) {
  await act(async () => { root!.render(<StoreProvider><Achievements game={g} /></StoreProvider>); });
  await flush();
}

beforeEach(() => {
  h.calls.length = 0;
  h.asked.length = 0;
  h.listeners.clear();
  document.body.innerHTML = '<div id="root"></div>';
});
afterEach(async () => {
  if (root) await act(async () => root!.unmount());
  root = null;
});

describe("an open panel after the backend re-pulled RetroAchievements progress", () => {
  it("reads again and shows the new progress", async () => {
    await mount();
    await answer(0, view(0));
    expect(earned()).toBe(0);

    await refreshed();
    expect(reads()).toBe(2);
    await answer(1, view(2));
    expect(earned()).toBe(2);
  });

  it("keeps showing what it had while the new answer is on its way", async () => {
    await mount();
    await answer(0, view(1));

    await refreshed();
    // Not back to "Loading…": the old answer stays until the new one lands.
    expect(rows()).toBe(3);
    expect(text()).not.toContain("Loading");
    expect(earned()).toBe(1);

    await answer(1, view(3));
    expect(earned()).toBe(3);
  });

  it("keeps what it had, without an error, if the reload fails", async () => {
    await mount();
    await answer(0, view(1));

    await refreshed();
    await fail(1);
    expect(rows()).toBe(3);
    expect(earned()).toBe(1);
    expect(document.querySelector("[role=alert]")).toBeNull();
  });

  it("never lets an older answer replace a newer one that arrived first", async () => {
    await mount(); // read 0 is still out, with the old progress
    await refreshed(); // read 1 starts, after the refresh
    await answer(1, view(2));
    await answer(0, view(0)); // the slow first read finally lands
    expect(earned()).toBe(2);
  });

  it("still shows the first read if the reload is the one that fails", async () => {
    await mount();
    await refreshed();
    await fail(1);
    await answer(0, view(1));
    expect(earned()).toBe(1);
  });

  it("does not read twice when the game changes after a refresh", async () => {
    await mount();
    await answer(0, view(0));
    await refreshed();
    await answer(1, view(1));
    expect(reads()).toBe(2);

    await rerender(game(2)); // another version of the game: one read for it, not two
    expect(reads()).toBe(3);
  });

  it("ignores the announcement when its answer arrives for a game that is no longer shown", async () => {
    await mount(game(1));
    await answer(0, view(0));
    await refreshed(); // read 1, for game 1
    await rerender(game(2)); // read 2, for game 2
    await answer(2, view(3));
    await answer(1, view(1)); // game 1's late answer must not replace game 2's
    expect(earned()).toBe(3);
  });

  it("does nothing until it hears the announcement", async () => {
    await mount();
    await answer(0, view(0));
    await flush();
    expect(reads()).toBe(1);
  });
});
