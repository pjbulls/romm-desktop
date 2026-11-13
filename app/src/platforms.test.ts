import { describe, expect, it } from "vitest";
import { hiddenPlatformCount, viewAfterPlatformsChange, visiblePlatforms, withHidden } from "./platforms";
import type { Platform, View } from "./types";

const p = (slug: string, hidden = false): Platform => ({
  id: slug.length, slug, name: slug.toUpperCase(), romCount: 3, firmwareCount: 0,
  emulator: null, emulatorName: null, emulatorInstalled: false, choices: [], selected: "", automaticLabel: null, ignoredOverride: null, hidden,
});

describe("which platforms the game browser offers", () => {
  it("leaves out the hidden ones and keeps the order", () => {
    expect(visiblePlatforms([p("gba"), p("arcade", true), p("snes")]).map((x) => x.slug)).toEqual(["gba", "snes"]);
  });

  it("offers nothing when everything is hidden", () => {
    expect(visiblePlatforms([p("gba", true), p("snes", true)])).toEqual([]);
  });

  it("hides and shows one platform, leaving the others the very same objects", () => {
    const list = [p("gba"), p("snes")];
    const hidden = withHidden(list, "gba", true);
    expect(hidden.map((x) => x.hidden)).toEqual([true, false]);
    expect(hidden[1]).toBe(list[1]);
    expect(list[0].hidden).toBe(false); // the original is untouched
    expect(withHidden(hidden, "gba", false).map((x) => x.hidden)).toEqual([false, false]);
  });

  it("changes nothing for a platform that isn't there or is already as asked", () => {
    const list = [p("gba")];
    expect(withHidden(list, "nope", true)).toEqual(list);
    expect(withHidden(list, "gba", false)[0]).toBe(list[0]);
  });
});

describe("the number beside Hidden platforms", () => {
  const withGames = (slug: string, romCount: number, hidden = true): Platform => ({ ...p(slug, hidden), romCount });

  it("counts hidden platforms, not their games", () => {
    expect(hiddenPlatformCount([withGames("gba", 40), withGames("arcade", 3), withGames("snes", 900, false)])).toBe(2);
  });

  it("is zero when nothing is hidden", () => {
    expect(hiddenPlatformCount([p("gba"), p("snes")])).toBe(0);
    expect(hiddenPlatformCount([])).toBe(0);
  });

  it("leaves out a hidden platform with no games, since there is nothing to see there", () => {
    expect(hiddenPlatformCount([withGames("gba", 0), withGames("arcade", 3)])).toBe(1);
    expect(hiddenPlatformCount([withGames("gba", 0)])).toBe(0);
  });
});

describe("what happens to the open view when platforms are hidden", () => {
  const gba = p("gba");
  const open: View = { kind: "library", platform: gba };

  it("an open platform that gets hidden turns into all games", () => {
    expect(viewAfterPlatformsChange(open, [p("gba", true)])).toEqual({ kind: "library", platform: null });
  });

  it("an open platform that is still shown stays open, as the same object", () => {
    expect(viewAfterPlatformsChange(open, [p("gba")])).toBe(open);
  });

  it("other views are left alone", () => {
    for (const v of [{ kind: "library", platform: null }, { kind: "installed" }, { kind: "emulators" }, { kind: "settings" }] as View[]) {
      expect(viewAfterPlatformsChange(v, [p("gba", true)])).toBe(v);
    }
  });

  describe("the Hidden platforms view", () => {
    const hiddenView: View = { kind: "hidden" };

    it("stays while some hidden platform has games", () => {
      expect(viewAfterPlatformsChange(hiddenView, [p("gba", true), p("snes")])).toBe(hiddenView);
    });

    it("turns into all games once nothing is hidden any more", () => {
      expect(viewAfterPlatformsChange(hiddenView, [p("gba"), p("snes")])).toEqual({ kind: "library", platform: null });
    });

    it("does too when the only hidden platforms are empty", () => {
      expect(viewAfterPlatformsChange(hiddenView, [{ ...p("gba", true), romCount: 0 }])).toEqual({ kind: "library", platform: null });
    });
  });

  it("a platform missing from the list is not treated as hidden (the list may still be loading)", () => {
    expect(viewAfterPlatformsChange(open, [])).toBe(open);
  });
});
