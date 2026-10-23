import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { AchievementsPanel } from "./Achievements";
import type { AchFilter } from "../format";
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
    expect([number(searched, "ach-filter-earned"), number(searched, "ach-filter-locked")]).toEqual(["0", "1"]);
    const missable = render(view(), { missableOnly: true });
    expect([number(missable, "ach-filter-earned"), number(missable, "ach-filter-locked")]).toEqual(["1", "1"]);
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

/** 1 progression earned, 2 progression locked, 3 missable locked, 4 no kind locked. */
const KIND_SET = [ach(1, { kind: "progression", earned: true }), ach(2, { kind: "progression" }), ach(3, { kind: "missable" }), ach(4)];

describe("progression filter", () => {
  it("is offered next to Missable and the earned/locked filters", () => {
    const html = render(view({ achievements: KIND_SET }));
    expect(chip(html, "ach-filter-progression")).toContain("Progression");
    expect(chip(html, "ach-filter-progression")).toContain('aria-pressed="false"');
    expect(html).toContain('data-testid="ach-filter-missable"');
    expect(html).toContain('data-testid="ach-filter-locked"');
  });

  it("counts the progression achievements on the toggle", () => {
    const html = render(view({ achievements: KIND_SET }));
    expect(number(html, "ach-filter-progression")).toBe("2");
    expect(number(html, "ach-filter-missable")).toBe("1");
  });

  it("shows only the progression ones when asked, and says how many of how many", () => {
    const html = render(view({ achievements: KIND_SET }), { progressionOnly: true });
    expect(rows(html)).toBe(2);
    expect(html).toContain("Achievement 1");
    expect(html).toContain("Achievement 2");
    expect(html).not.toContain("Achievement 3");
    expect(chip(html, "ach-filter-progression")).toContain('aria-pressed="true"');
    expect(chip(html, "ach-filter-missable")).toContain('aria-pressed="false"');
    expect(html).toContain("Showing 2 of 4");
  });

  it("combines with locked and with earned", () => {
    const locked = render(view({ achievements: KIND_SET }), { progressionOnly: true, filter: "locked" });
    expect(rows(locked)).toBe(1);
    expect(locked).toContain("Achievement 2");
    expect(locked).toContain("Showing 1 of 4");
    const earned = render(view({ achievements: KIND_SET }), { progressionOnly: true, filter: "earned" });
    expect(rows(earned)).toBe(1);
    expect(earned).toContain("Achievement 1");
  });

  it("shows either kind when Missable is chosen too", () => {
    const html = render(view({ achievements: KIND_SET }), { progressionOnly: true, missableOnly: true });
    expect(rows(html)).toBe(3);
    expect(html).not.toContain("Achievement 4");
    expect(chip(html, "ach-filter-progression")).toContain('aria-pressed="true"');
    expect(chip(html, "ach-filter-missable")).toContain('aria-pressed="true"');
    expect(html).toContain("Showing 3 of 4");
  });

  it("makes the earned/locked counts follow the progression filter", () => {
    const html = render(view({ achievements: KIND_SET }), { progressionOnly: true });
    expect([number(html, "ach-filter-earned"), number(html, "ach-filter-locked")]).toEqual(["1", "1"]);
  });

  it("makes the progression count follow the earned/locked choice, and counts it whether or not Missable is chosen", () => {
    expect(number(render(view({ achievements: KIND_SET }), { filter: "locked" }), "ach-filter-progression")).toBe("1");
    expect(number(render(view({ achievements: KIND_SET }), { missableOnly: true }), "ach-filter-progression")).toBe("2");
  });

  it("disables the toggle, and says why, when RetroAchievements marks none", () => {
    const html = render(view()); // SET has missable achievements but no progression or win-condition ones
    const btn = chip(html, "ach-filter-progression");
    expect(btn).toContain("disabled");
    expect(btn).toContain("marks none of this game");
    expect(number(html, "ach-filter-progression")).toBe("0");
    expect(chip(html, "ach-filter-missable")).not.toContain("disabled");
  });

  it("includes win conditions, in the count and in what it shows, each keeping its own badge", () => {
    const set = [ach(1, { kind: "progression", earned: true }), ach(2, { kind: "win_condition" }), ach(3, { kind: "missable" }), ach(4)];
    const html = render(view({ achievements: set }), { progressionOnly: true });
    expect(number(html, "ach-filter-progression")).toBe("2");
    expect(rows(html)).toBe(2);
    expect(html).toContain("Achievement 2");
    expect(html).not.toContain("Achievement 3");
    expect(html).toContain("Win condition");
    expect(html).toContain("Showing 2 of 4");
    const locked = render(view({ achievements: set }), { progressionOnly: true, filter: "locked" });
    expect(rows(locked)).toBe(1);
    expect(locked).toContain("Achievement 2");
  });

  it("is available for a game whose only story achievement is a win condition", () => {
    const html = render(view({ achievements: [ach(1, { kind: "win_condition" }), ach(2)] }));
    expect(chip(html, "ach-filter-progression")).not.toContain("disabled");
    expect(number(html, "ach-filter-progression")).toBe("1");
  });

  it("says nothing matches, with a way back, when nothing is left", () => {
    const html = render(view({ achievements: KIND_SET }), { progressionOnly: true, filter: "earned", query: "achievement 2" });
    expect(rows(html)).toBe(0);
    expect(html).toContain("No achievements match.");
    expect(html).toContain('data-testid="ach-clear"');
  });

  it("is there for a set the user hasn't linked, like Missable", () => {
    const html = render(view({ state: "not_linked", achievements: KIND_SET }), { progressionOnly: true });
    expect(html).toContain('data-testid="ach-filter-progression"');
    expect(html).not.toContain('data-testid="ach-filter-earned"');
    expect(rows(html)).toBe(2);
  });
});

describe("earned and locked controls", () => {
  it("are two toggles ahead of the kind toggles, with no All button", () => {
    const html = render(view());
    expect(html).not.toContain('data-testid="ach-filter-all"');
    const at = (id: string) => html.indexOf(`data-testid="ach-filter-${id}"`);
    const order = [at("earned"), at("locked"), at("missable"), at("progression")];
    expect(order.every((n) => n > 0)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
  });

  it("show which one is chosen", () => {
    const cases: [AchFilter, string, string][] = [["all", "false", "false"], ["earned", "true", "false"], ["locked", "false", "true"]];
    for (const [filter, earned, locked] of cases) {
      const html = render(view(), { filter });
      expect(chip(html, "ach-filter-earned")).toContain(`aria-pressed="${earned}"`);
      expect(chip(html, "ach-filter-locked")).toContain(`aria-pressed="${locked}"`);
    }
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
