import { describe, expect, it } from "vitest";
import { achievementFacets, foldText, isMissable, isNarrowed, queryAchievements, searchAchievements, type AchQuery } from "./format";
import type { Achievement } from "./types";

const ach = (id: number, title: string, description: string, over: Partial<Achievement> = {}): Achievement => ({
  id, title, description, points: 5, kind: "", earned: false, hardcore: false, earned_at: null, badge: null, ...over,
});

const LIST: Achievement[] = [
  ach(1, "Boss Slayer", "Defeat the final boss", { earned: true, kind: "progression" }),
  ach(2, "Pokémon Master", "Catch them all"),
  ach(3, "Secret Room", "Find the hidden room before chapter 3", { kind: "missable" }),
  ach(4, "Last Chance", "Buy the sword before the village burns", { earned: true, kind: "missable" }),
  ach(5, "Speed Run", "Finish in under an hour"),
];
const ids = (l: Achievement[]) => l.map((a) => a.id);
const q = (o: Partial<AchQuery> = {}): AchQuery => ({ status: "all", missable: false, text: "", ...o });

describe("foldText", () => {
  it("lower-cases and takes accents off", () => {
    expect(foldText("Pokémon")).toBe("pokemon");
    expect(foldText("ÀÉÎõü")).toBe("aeiou");
    expect(foldText("Plain TEXT 42")).toBe("plain text 42");
  });
});

describe("isMissable", () => {
  it("is true only for RetroAchievements' missable type", () => {
    expect(ids(LIST.filter(isMissable))).toEqual([3, 4]);
    expect(isMissable(ach(9, "x", "y", { kind: "win_condition" }))).toBe(false);
    expect(isMissable(ach(9, "x", "y", { kind: "" }))).toBe(false);
  });
});

describe("searchAchievements", () => {
  it("returns everything for no words", () => {
    expect(searchAchievements(LIST, "")).toBe(LIST);
    expect(searchAchievements(LIST, "   ")).toBe(LIST);
  });
  it("matches the title or the description", () => {
    expect(ids(searchAchievements(LIST, "slayer"))).toEqual([1]); // title
    expect(ids(searchAchievements(LIST, "sword"))).toEqual([4]); // description
  });
  it("ignores case and accents, either way round", () => {
    expect(ids(searchAchievements(LIST, "POKEMON"))).toEqual([2]);
    expect(ids(searchAchievements(LIST, "pokémon"))).toEqual([2]);
  });
  it("matches part of a word", () => {
    expect(ids(searchAchievements(LIST, "bos"))).toEqual([1]);
  });
  it("needs every word, in any order, across title and description together", () => {
    expect(ids(searchAchievements(LIST, "room hidden"))).toEqual([3]); // "room" in the title, "hidden" in the description
    expect(ids(searchAchievements(LIST, "boss sword"))).toEqual([]);
  });
  it("finds nothing for a word that isn't there", () => {
    expect(searchAchievements(LIST, "zelda")).toEqual([]);
  });
  it("copes with an achievement that has no description", () => {
    expect(ids(searchAchievements([ach(8, "Untitled", "")], "untitled"))).toEqual([8]);
  });
});

describe("queryAchievements", () => {
  it("changes nothing when nothing is chosen", () => {
    expect(ids(queryAchievements(LIST, q()))).toEqual([1, 2, 3, 4, 5]);
  });
  it("shows only the missable ones", () => {
    expect(ids(queryAchievements(LIST, q({ missable: true })))).toEqual([3, 4]);
  });
  it("combines missable with earned or locked: the ones you can still miss", () => {
    expect(ids(queryAchievements(LIST, q({ missable: true, status: "locked" })))).toEqual([3]);
    expect(ids(queryAchievements(LIST, q({ missable: true, status: "earned" })))).toEqual([4]);
  });
  it("combines search with the others", () => {
    expect(ids(queryAchievements(LIST, q({ text: "before" })))).toEqual([3, 4]);
    expect(ids(queryAchievements(LIST, q({ text: "before", status: "locked" })))).toEqual([3]);
    expect(ids(queryAchievements(LIST, q({ text: "boss", missable: true })))).toEqual([]);
  });
});

describe("achievementFacets", () => {
  it("counts every status and the missable ones when nothing is narrowing", () => {
    expect(achievementFacets(LIST, q())).toEqual({ status: { all: 5, earned: 2, locked: 3 }, missable: 2 });
  });
  it("counts missable within the chosen status", () => {
    expect(achievementFacets(LIST, q({ status: "locked" })).missable).toBe(1);
    expect(achievementFacets(LIST, q({ status: "earned" })).missable).toBe(1);
  });
  it("counts the statuses within missable-only", () => {
    expect(achievementFacets(LIST, q({ missable: true })).status).toEqual({ all: 2, earned: 1, locked: 1 });
  });
  it("counts within a search", () => {
    expect(achievementFacets(LIST, q({ text: "before" }))).toEqual({ status: { all: 2, earned: 1, locked: 1 }, missable: 2 });
    expect(achievementFacets(LIST, q({ text: "boss" }))).toEqual({ status: { all: 1, earned: 1, locked: 0 }, missable: 0 });
  });
  it("is what choosing the control would show", () => {
    const query = q({ text: "before", status: "locked" });
    expect(achievementFacets(LIST, query).missable).toBe(queryAchievements(LIST, { ...query, missable: true }).length);
    expect(achievementFacets(LIST, query).status.earned).toBe(queryAchievements(LIST, { ...query, status: "earned" }).length);
  });
});

describe("isNarrowed", () => {
  it("is about search and missable, not the earned/locked choice", () => {
    expect(isNarrowed(q())).toBe(false);
    expect(isNarrowed(q({ status: "locked" }))).toBe(false);
    expect(isNarrowed(q({ missable: true }))).toBe(true);
    expect(isNarrowed(q({ text: "x" }))).toBe(true);
    expect(isNarrowed(q({ text: "   " }))).toBe(false);
  });
});
