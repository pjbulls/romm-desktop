import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { AchievementsPanel } from "./Achievements";
import { ACH_PREVIEW } from "../format";
import type { Achievement, AchievementsView } from "../types";

const ach = (id: number, over: Partial<Achievement> = {}): Achievement => ({
  id, title: `Achievement ${id}`, description: `Do thing ${id}`, points: 5, kind: "", earned: false, hardcore: false,
  earned_at: null, badge: `/assets/b/${id}.png`, ...over,
});

const view = (over: Partial<AchievementsView> = {}): AchievementsView => {
  const achievements = over.achievements ?? [ach(1, { earned: true, earned_at: "2026-01-02T03:04:05Z", hardcore: true }), ach(2), ach(3, { kind: "missable" })];
  const earned = achievements.filter((a) => a.earned).length;
  return {
    state: "ready", ra_username: "pj", total: achievements.length, earned, earned_hardcore: 1, points_total: 15, points_earned: 5,
    award: null, last_earned: "2026-01-02T03:04:05Z", progress_missing: false, achievements, stale: false, fetched_at: "2026-01-02T03:04:05Z",
    ...over,
  };
};

const noop = () => {};
const render = (v: AchievementsView | null, extra: Partial<Parameters<typeof AchievementsPanel>[0]> = {}) =>
  renderToStaticMarkup(
    <AchievementsPanel romId={7} view={v} error={null} refreshing={false} filter="all" expanded={false}
      onFilter={noop} onToggleExpanded={noop} onRefresh={noop} onRetry={noop} {...extra} />,
  );
const rows = (html: string) => (html.match(/data-testid="achievement"/g) ?? []).length;

describe("AchievementsPanel", () => {
  it("shows progress, points and a row per achievement", () => {
    const html = render(view());
    expect(html).toContain("1 / 3 earned");
    expect(html).toContain("5 / 15 pts");
    expect(html).toContain('aria-valuenow="33"');
    expect(rows(html)).toBe(3);
    expect(html).toContain("Hardcore");
    expect(html).toContain("Missable");
    expect(html).toContain("as pj");
  });

  it("marks earned and locked rows differently", () => {
    const html = render(view());
    expect((html.match(/class="ach earned"/g) ?? []).length).toBe(1);
    expect((html.match(/class="ach locked"/g) ?? []).length).toBe(2);
  });

  it("applies the filter and says so when nothing matches", () => {
    expect(rows(render(view(), { filter: "earned" }))).toBe(1);
    expect(rows(render(view(), { filter: "locked" }))).toBe(2);
    const none = view({ achievements: [ach(1), ach(2)] });
    expect(render(none, { filter: "earned" })).toContain("Nothing earned yet.");
    const all = view({ achievements: [ach(1, { earned: true })] });
    expect(render(all, { filter: "locked" })).toContain("Everything is earned.");
  });

  it("shows only a preview of a long set, with a control to expand", () => {
    const many = view({ achievements: Array.from({ length: ACH_PREVIEW + 6 }, (_, i) => ach(i + 1)) });
    const collapsed = render(many);
    expect(rows(collapsed)).toBe(ACH_PREVIEW);
    expect(collapsed).toContain(`Show all ${ACH_PREVIEW + 6}`);
    const open = render(many, { expanded: true });
    expect(rows(open)).toBe(ACH_PREVIEW + 6);
    expect(open).toContain("Show fewer");
  });

  it("offers no expand control for a short set", () => {
    expect(render(view())).not.toContain('data-testid="ach-more"');
  });

  it("shows the achievements but explains, when no RetroAchievements account is linked", () => {
    const html = render(view({ state: "not_linked", ra_username: null, earned: 0, achievements: [ach(1), ach(2)] }));
    expect(html).toContain('data-testid="ach-not-linked"');
    expect(rows(html)).toBe(2);
    expect(html).not.toContain('data-testid="ach-refresh"'); // refreshing can't work without a username
    expect(html).not.toContain('data-testid="ach-summary"'); // "0 / 2 earned" would be misleading
  });

  it("warns when RomM has never fetched progress, rather than showing a silent zero", () => {
    expect(render(view({ progress_missing: true }))).toContain('data-testid="ach-no-progress"');
    expect(render(view({ progress_missing: false }))).not.toContain('data-testid="ach-no-progress"');
  });

  it("says plainly when a game has no set", () => {
    const html = render(view({ state: "no_set", achievements: [], total: 0 }));
    expect(html).toContain("No RetroAchievements found for this game");
    expect(rows(html)).toBe(0);
    expect(html).not.toContain('data-testid="ach-refresh"');
  });

  it("flags a saved copy served while offline", () => {
    expect(render(view({ stale: true }))).toContain('data-testid="ach-stale"');
    expect(render(view({ stale: false }))).not.toContain('data-testid="ach-stale"');
  });

  it("shows Refresh only when ready, and disables it while running", () => {
    expect(render(view())).toContain('data-testid="ach-refresh"');
    const busy = render(view(), { refreshing: true });
    expect(busy).toContain("Refreshing…");
    expect(busy).toMatch(/<button[^>]*disabled[^>]*data-testid="ach-refresh"/);
  });

  it("shows the award when there is one, and ignores an unknown one", () => {
    expect(render(view({ award: "mastered" }))).toContain("Mastered");
    expect(render(view({ award: "a-future-award" }))).not.toContain("a-future-award");
  });

  it("loads, errors and retries", () => {
    expect(render(null)).toContain("Loading…");
    const failed = render(null, { error: "boom" });
    expect(failed).toContain("Couldn&#x27;t load achievements (boom)");
    expect(failed).toContain("Try again");
  });

  it("escapes text that came from the network", () => {
    const html = render(view({ achievements: [ach(1, { title: "<script>alert(1)</script>", description: "<img src=x onerror=1>" })] }));
    expect(html).not.toContain("<script>");
    expect(html).not.toContain("<img src=x");
    expect(html).toContain("&lt;script&gt;");
  });

  it("copes with an achievement that has no title", () => {
    expect(render(view({ achievements: [ach(1, { title: "" })] }))).toContain("Untitled achievement");
  });
});
