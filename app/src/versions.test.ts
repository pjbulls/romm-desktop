import { describe, expect, it } from "vitest";
import { copyIds, initialCopy, isCopyInstalled, isGameInstalled, versionCountText, versionOptionText } from "./versions";
import type { Game, Version } from "./types";

const v = (id: number, label: string, o: Partial<Version> = {}): Version => ({ id, label, isDefault: false, installed: false, ...o });

const single: Game = {
  id: 1, name: "Solo", platformId: 1, platformSlug: "gba", installed: false, sizeBytes: 1, multiFile: false, coverPath: null, versions: [], versionLabel: null,
};

/** Primary is 10 (Europe); 11 (USA) and 12 (Japan) are the other copies. */
const multi = (installedIds: number[] = []): Game => {
  const versions = [v(10, "Europe", { isDefault: true }), v(11, "USA"), v(12, "Japan")].map((x) => ({ ...x, installed: installedIds.includes(x.id) }));
  return { ...single, id: 10, name: "Multi", versions, installed: installedIds.length > 0 };
};

describe("copyIds", () => {
  it("is the game itself when there is nothing to choose", () => expect(copyIds(single)).toEqual([1]));
  it("lists every copy, the primary first", () => expect(copyIds(multi())).toEqual([10, 11, 12]));
});

describe("isCopyInstalled", () => {
  it("reads the list's per-copy flag", () => {
    const g = multi([11]);
    expect(isCopyInstalled(g, 11, {})).toBe(true);
    expect(isCopyInstalled(g, 10, {})).toBe(false);
  });
  it("lets this session's install or uninstall win", () => {
    const g = multi([11]);
    expect(isCopyInstalled(g, 12, { 12: true })).toBe(true);
    expect(isCopyInstalled(g, 11, { 11: false })).toBe(false);
  });
  it("uses the game's own flag when it has no copies", () => {
    expect(isCopyInstalled({ ...single, installed: true }, 1, {})).toBe(true);
    expect(isCopyInstalled(single, 1, {})).toBe(false);
  });
  it("says no for an id that isn't one of its copies", () => expect(isCopyInstalled(multi([10]), 99, {})).toBe(false));
});

describe("isGameInstalled", () => {
  it("is true when only a non-primary copy is installed", () => expect(isGameInstalled(multi([12]), {})).toBe(true));
  it("is false when none is", () => expect(isGameInstalled(multi(), {})).toBe(false));
  it("goes false after uninstalling the only installed copy", () => expect(isGameInstalled(multi([12]), { 12: false })).toBe(false));
  it("goes true after installing any copy", () => expect(isGameInstalled(multi(), { 11: true })).toBe(true));
  it("works for a game with no copies", () => {
    expect(isGameInstalled(single, {})).toBe(false);
    expect(isGameInstalled(single, { 1: true })).toBe(true);
  });
});

describe("initialCopy", () => {
  it("opens on the primary when nothing is installed", () => expect(initialCopy(multi(), {})).toBe(10));
  it("opens on the installed copy so Play is available", () => expect(initialCopy(multi([12]), {})).toBe(12));
  it("prefers the primary when it is installed too", () => expect(initialCopy(multi([10, 12]), {})).toBe(10));
  it("takes the first installed copy, in list order, when the primary isn't one", () => expect(initialCopy(multi([12, 11]), {})).toBe(11));
  it("follows installs made this session", () => expect(initialCopy(multi(), { 11: true })).toBe(11));
  it("is the game's own id when it has no copies", () => expect(initialCopy(single, {})).toBe(1));
});

describe("versionOptionText", () => {
  it("notes the default and installed copies", () => {
    expect(versionOptionText(v(1, "Europe · En,Fr,De", { isDefault: true }), true)).toBe("Europe · En,Fr,De — default, installed");
    expect(versionOptionText(v(1, "Europe", { isDefault: true }), false)).toBe("Europe — default");
    expect(versionOptionText(v(2, "USA"), true)).toBe("USA — installed");
  });
  it("is just the label otherwise", () => expect(versionOptionText(v(2, "Japan"), false)).toBe("Japan"));
});

describe("versionCountText", () => {
  it("counts copies, and says nothing for one", () => {
    expect(versionCountText(multi())).toBe("3 versions");
    expect(versionCountText(single)).toBe("");
  });
});
