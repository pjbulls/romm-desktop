import { describe, expect, it } from "vitest";
import { copyInfo, otherCopyIds } from "./versions";
import type { Game, Version, VersionSize } from "./types";

const v = (id: number, label: string, o: Partial<Version> = {}): Version => ({ id, label, isDefault: false, installed: false, ...o });

/** Primary 10 (Europe, 8 MB), others 11 and 12. */
const game: Game = {
  id: 10, name: "G", platformId: 1, platformSlug: "gba", installed: false, sizeBytes: 8 * 1024 * 1024, multiFile: false, coverPath: null, versionLabel: null,
  versions: [v(10, "Europe", { isDefault: true }), v(11, "USA"), v(12, "Japan")],
};
const size = (id: number, mb: number, multiFile = false): VersionSize => ({ id, sizeBytes: mb * 1024 * 1024, multiFile });

describe("copyInfo", () => {
  it("gives the primary's size from the list, with nothing fetched", () => {
    expect(copyInfo(game, 10, {})).toEqual({ sizeBytes: 8 * 1024 * 1024, multiFile: false });
  });
  it("knows nothing about another version until its size has been fetched", () => {
    expect(copyInfo(game, 11, {})).toBeNull();
  });
  it("gives a fetched version's size and whether it is several files", () => {
    expect(copyInfo(game, 12, { 12: size(12, 700, true) })).toEqual({ sizeBytes: 700 * 1024 * 1024, multiFile: true });
  });
  it("takes the primary's from the list even if a fetch also returned it", () => {
    expect(copyInfo(game, 10, { 10: size(10, 1) })?.sizeBytes).toBe(8 * 1024 * 1024);
  });
  it("says nothing for an id that isn't a version of this game", () => {
    expect(copyInfo(game, 99, { 12: size(12, 1) })).toBeNull();
  });
});

describe("otherCopyIds", () => {
  it("is every version but the primary, in order", () => {
    expect(otherCopyIds(game)).toEqual([11, 12]);
  });
  it("is empty for a game with nothing to choose", () => {
    expect(otherCopyIds({ ...game, versions: [] })).toEqual([]);
  });
});
