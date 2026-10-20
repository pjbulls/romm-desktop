// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Achievement, AchievementsView, Game } from "../types";

(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

const h = vi.hoisted(() => ({ view: null as unknown, calls: [] as string[] }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => {
    h.calls.push(cmd);
    return Promise.resolve(cmd === "achievements" ? h.view : null);
  },
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));

import { StoreProvider } from "../store";
import { Achievements } from "./Achievements";

const ach = (id: number, title: string, description: string, over: Partial<Achievement> = {}): Achievement => ({
  id, title, description, points: 5, kind: "", earned: false, hardcore: false, earned_at: null, badge: null, ...over,
});

/** Twelve, so the preview (8) matters. Missable: 3, 7, 10 (7 is earned). "boss" appears in 2, 3 and 11. */
const SET: Achievement[] = [
  ach(1, "First Steps", "Leave the village"),
  ach(2, "Boss Rush", "Beat every boss in one run"),
  ach(3, "Secret Room", "Find the hidden room; the boss never mentions it", { kind: "missable" }),
  ach(4, "Collector", "Gather 100 coins"),
  ach(5, "Pokémon Fan", "Meet the mascot"),
  ach(6, "Speed Run", "Finish in an hour"),
  ach(7, "Last Chance", "Buy the sword before the village burns", { kind: "missable", earned: true }),
  ach(8, "Treasure", "Open every chest"),
  ach(9, "Fisher", "Catch a golden fish"),
  ach(10, "Ferry Pass", "Take the ferry before chapter 4", { kind: "missable" }),
  ach(11, "Final Boss", "Defeat the final boss", { kind: "win_condition" }),
  ach(12, "Perfectionist", "Everything else"),
];

const view = (achievements = SET): AchievementsView => ({
  state: "ready", ra_username: "pj", total: achievements.length, earned: achievements.filter((a) => a.earned).length, earned_hardcore: 0,
  points_total: 60, points_earned: 5, award: null, last_earned: null, progress_missing: false, achievements, stale: false, fetched_at: null,
});

const game = (id: number): Game => ({
  id, name: "G", platformId: 1, platformSlug: "gba", installed: false, sizeBytes: 0, multiFile: false, coverPath: null, versions: [], versionLabel: null,
});

let root: Root | null = null;
const flush = () => act(async () => { await new Promise((r) => setTimeout(r, 0)); });
const q = <T extends Element>(css: string) => document.querySelector<T>(css);
const rows = () => document.querySelectorAll("[data-testid=achievement]").length;
const titles = () => [...document.querySelectorAll(".ach-title")].map((e) => e.textContent);
const text = () => document.body.textContent ?? "";

async function mount(g = game(1)) {
  root = createRoot(document.getElementById("root")!);
  await act(async () => { root!.render(<StoreProvider><Achievements game={g} /></StoreProvider>); });
  await flush();
}
async function rerender(g: Game) {
  await act(async () => { root!.render(<StoreProvider><Achievements game={g} /></StoreProvider>); });
  await flush();
}
async function type(value: string) {
  const input = q<HTMLInputElement>("[data-testid=ach-search]")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
const click = (css: string) => act(async () => { q<HTMLElement>(css)!.click(); });
const escape = (el: Element) => act(async () => { el.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true })); });

beforeEach(() => {
  h.view = view();
  h.calls.length = 0;
  document.body.innerHTML = '<div id="root"></div>';
});
afterEach(async () => {
  if (root) await act(async () => root!.unmount());
  root = null;
});

describe("searching and filtering achievements in the panel", () => {
  it("starts with the preview and no narrowing", async () => {
    await mount();
    expect(rows()).toBe(8);
    expect(text()).toContain("Show all 12");
    expect(q("[data-testid=ach-showing]")).toBeNull();
  });

  it("narrows as you type, by title and by description", async () => {
    await mount();
    await type("boss");
    expect(titles()).toEqual(["Boss Rush", "Secret Room", "Final Boss"]); // 3's description mentions the boss
    expect(text()).toContain("Showing 3 of 12");
    await type("golden");
    expect(titles()).toEqual(["Fisher"]);
  });

  it("finds accented names without the accent", async () => {
    await mount();
    await type("pokemon");
    expect(titles()).toEqual(["Pokémon Fan"]);
  });

  it("shows only the missable ones from the toggle, and un-toggles", async () => {
    await mount();
    await click("[data-testid=ach-filter-missable]");
    expect(titles()).toEqual(["Secret Room", "Last Chance", "Ferry Pass"]);
    expect(q("[data-testid=ach-filter-missable]")!.getAttribute("aria-pressed")).toBe("true");
    await click("[data-testid=ach-filter-missable]");
    expect(rows()).toBe(8);
  });

  it("finds the missable ones you haven't earned: the point of the filter", async () => {
    await mount();
    await click("[data-testid=ach-filter-missable]");
    await click("[data-testid=ach-filter-locked]");
    expect(titles()).toEqual(["Secret Room", "Ferry Pass"]);
    expect(text()).toContain("Showing 2 of 12");
  });

  it("combines the search with the toggle", async () => {
    await mount();
    await click("[data-testid=ach-filter-missable]");
    await type("boss");
    expect(titles()).toEqual(["Secret Room"]);
  });

  it("updates the counts as the others change", async () => {
    await mount();
    const num = (id: string) => q(`[data-testid=${id}] .muted`)!.textContent;
    expect([num("ach-filter-all"), num("ach-filter-earned"), num("ach-filter-locked"), num("ach-filter-missable")]).toEqual(["12", "1", "11", "3"]);
    await type("boss");
    expect([num("ach-filter-all"), num("ach-filter-locked"), num("ach-filter-missable")]).toEqual(["3", "3", "1"]);
  });

  it("says nothing matches, and Clear brings everything back", async () => {
    await mount();
    await click("[data-testid=ach-filter-missable]");
    await click("[data-testid=ach-filter-earned]");
    await type("ferry"); // missable, but locked
    expect(rows()).toBe(0);
    expect(text()).toContain("No achievements match.");
    await click("[data-testid=ach-clear]");
    expect(q<HTMLInputElement>("[data-testid=ach-search]")!.value).toBe("");
    expect(q("[data-testid=ach-filter-missable]")!.getAttribute("aria-pressed")).toBe("false");
    expect(q("[data-testid=ach-filter-all]")!.getAttribute("aria-pressed")).toBe("true");
    expect(rows()).toBe(8);
  });

  it("collapses 'Show all' again when the narrowing changes", async () => {
    await mount();
    await click("[data-testid=ach-more]");
    expect(rows()).toBe(12);
    await type("a");
    expect(q("[data-testid=ach-more]")?.textContent ?? "").not.toBe("Show fewer");
  });
});

describe("Escape in the search box", () => {
  it("clears the search and does not reach the drawer's own Escape handler", async () => {
    const closes = vi.fn();
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && closes(); // what the drawer does, on window
    window.addEventListener("keydown", onKey);
    try {
      await mount();
      await type("boss");
      const input = q<HTMLInputElement>("[data-testid=ach-search]")!;
      await escape(input);
      expect(input.value).toBe("");
      expect(rows()).toBe(8);
      expect(closes).not.toHaveBeenCalled();
      // With nothing left to clear, Escape goes on to close the panel as usual.
      await escape(input);
      expect(closes).toHaveBeenCalledTimes(1);
    } finally {
      window.removeEventListener("keydown", onKey);
    }
  });
});

describe("switching to another game or version", () => {
  it("starts the new one from scratch", async () => {
    await mount(game(1));
    await click("[data-testid=ach-filter-missable]");
    await type("boss");
    expect(rows()).toBe(1);
    await rerender(game(2));
    expect(q<HTMLInputElement>("[data-testid=ach-search]")!.value).toBe("");
    expect(q("[data-testid=ach-filter-missable]")!.getAttribute("aria-pressed")).toBe("false");
    expect(rows()).toBe(8);
    expect(h.calls.filter((c) => c === "achievements")).toHaveLength(2);
  });
});
