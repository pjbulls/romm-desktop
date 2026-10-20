import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { AchievementsPanel } from "./Achievements";
import type { Achievement, AchievementsView } from "../types";

const ach = (id: number, over: Partial<Achievement> = {}): Achievement => ({
  id, title: `Achievement ${id}`, description: `Do thing ${id}`, points: 5, kind: "", earned: false, hardcore: false, earned_at: null, badge: null, ...over,
});

/** 1 earned, 2 locked, 3 locked+missable, 4 earned+missable. */
const SET = [ach(1, { earned: true }), ach(2), ach(3, { kind: "missable" }), ach(4, { kind: "missable", earned: true })];

const view = (over: Partial<AchievementsView> = {}): AchievementsView => {
  const achievements = over.achievements ?? SET;
  return {
    state: "ready", ra_username: "pj", total: achievements.length, earned: achievements.filter((a) => a.earned).length, earned_hardcore: 0,
    points_total: 20, points_earned: 10, award: null, last_earned: null, progress_missing: false, achievements, stale: false, fetched_at: null, ...over,
  };
};

const noop = () => {};
const render = (v: AchievementsView | null, extra: Partial<Parameters<typeof AchievementsPanel>[0]> = {}) =>
  renderToStaticMarkup(
    <AchievementsPanel romId={7} view={v} error={null} refreshing={false} filter="all" expanded={false}
      onFilter={noop} onToggleExpanded={noop} onRefresh={noop} onRetry={noop} {...extra} />,
  );
const rows = (html: string) => (html.match(/data-testid="achievement"/g) ?? []).length;
const chip = (html: string, id: string) => html.match(new RegExp(`<button[^>]*data-testid="${id}"[^>]*>.*?</button>`))?.[0] ?? "";
const number = (html: string, id: string) => chip(html, id).match(/<span class="muted">(\d+)<\/span>/)?.[1];

describe("search box and missable filter", () => {
  it("offers both next to the earned/locked filters", () => {
    const html = render(view());
    expect(html).toContain('data-testid="ach-search"');
    expect(html).toContain('placeholder="Search achievements"');
    expect(html).toContain('aria-label="Search achievements"');
    expect(html).toContain('type="search"');
    expect(chip(html, "ach-filter-missable")).toContain("Missable");
    expect(html).toContain('data-testid="ach-filter-locked"');
  });

  it("shows the search text it was given", () => {
    expect(render(view(), { query: "boss" })).toMatch(/<input[^>]*value="boss"/);
  });

  it("counts the missable achievements on the toggle", () => {
    expect(number(render(view()), "ach-filter-missable")).toBe("2");
  });

  it("shows only the missable ones when asked, and says how many of how many", () => {
    const html = render(view(), { missableOnly: true });
    expect(rows(html)).toBe(2);
    expect(html).toContain("Achievement 3");
    expect(html).toContain("Achievement 4");
    expect(html).not.toContain("Achievement 2");
    expect(chip(html, "ach-filter-missable")).toContain('aria-pressed="true"');
    expect(html).toContain("Showing 2 of 4");
  });

  it("combines missable with locked: what you can still miss", () => {
    const html = render(view(), { missableOnly: true, filter: "locked" });
    expect(rows(html)).toBe(1);
    expect(html).toContain("Achievement 3");
    expect(html).toContain("Showing 1 of 4");
  });

  it("searches names and descriptions", () => {
    expect(rows(render(view(), { query: "achievement 2" }))).toBe(1);
    expect(rows(render(view(), { query: "thing 3" }))).toBe(1);
    expect(rows(render(view(), { query: "achievement" }))).toBe(4);
  });

  it("makes the earned/locked counts follow the search and the missable filter", () => {
    const searched = render(view(), { query: "achievement 3" });
    expect([number(searched, "ach-filter-all"), number(searched, "ach-filter-earned"), number(searched, "ach-filter-locked")]).toEqual(["1", "0", "1"]);
    const missable = render(view(), { missableOnly: true });
    expect([number(missable, "ach-filter-all"), number(missable, "ach-filter-earned"), number(missable, "ach-filter-locked")]).toEqual(["2", "1", "1"]);
  });

  it("makes the missable count follow the earned/locked choice", () => {
    expect(number(render(view(), { filter: "locked" }), "ach-filter-missable")).toBe("1");
  });

  it("disables the toggle, and says why, when RetroAchievements marks none", () => {
    const html = render(view({ achievements: [ach(1), ach(2)] }));
    const btn = chip(html, "ach-filter-missable");
    expect(btn).toContain("disabled");
    expect(btn).toContain("marks none of this game");
    expect(number(html, "ach-filter-missable")).toBe("0");
  });

  it("says nothing matches, with a way back, when the search finds nothing", () => {
    const html = render(view(), { query: "zelda" });
    expect(rows(html)).toBe(0);
    expect(html).toContain("No achievements match.");
    expect(html).toContain('data-testid="ach-clear"');
    expect(html).not.toContain("Everything is earned.");
  });

  it("says the same for missable-only when nothing is left", () => {
    const html = render(view(), { missableOnly: true, filter: "locked", query: "achievement 4" });
    expect(html).toContain("No achievements match.");
  });

  it("keeps the old messages when only earned/locked is chosen", () => {
    const none = view({ achievements: [ach(1), ach(2)] });
    const earned = render(none, { filter: "earned" });
    expect(earned).toContain("Nothing earned yet.");
    expect(earned).not.toContain("ach-clear");
    expect(render(view({ achievements: [ach(1, { earned: true })] }), { filter: "locked" })).toContain("Everything is earned.");
  });

  it("doesn't say 'showing' when nothing is narrowing the list", () => {
    expect(render(view())).not.toContain("ach-showing");
    expect(render(view(), { filter: "locked" })).not.toContain("ach-showing");
  });

  it("still truncates a long result to the preview, with the real count on the button", () => {
    const many = Array.from({ length: 30 }, (_, i) => ach(100 + i, { kind: "missable" }));
    const html = render(view({ achievements: many }), { missableOnly: true });
    expect(rows(html)).toBe(8);
    expect(html).toContain("Show all 30");
  });
});

describe("where the controls appear", () => {
  it("for a set the user hasn't linked: search and missable, but no earned/locked (nothing can be earned)", () => {
    const html = render(view({ state: "not_linked" }));
    expect(html).toContain('data-testid="ach-search"');
    expect(html).toContain('data-testid="ach-filter-missable"');
    expect(html).not.toContain('data-testid="ach-filter-earned"');
    expect(rows(render(view({ state: "not_linked" }), { missableOnly: true }))).toBe(2);
  });

  it("not for a game with no achievements", () => {
    expect(render(view({ state: "no_set", achievements: [] }))).not.toContain("ach-search");
  });

  it("not for an empty set", () => {
    expect(render(view({ achievements: [] }))).not.toContain("ach-search");
  });

  it("not while loading or after an error", () => {
    expect(render(null)).not.toContain("ach-search");
    expect(render(null, { error: "offline" })).not.toContain("ach-search");
  });
});
