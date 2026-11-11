import { describe, expect, it } from "vitest";
import { viewAfterPlatformsChange, visiblePlatforms, withHidden } from "./platforms";
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

  it("a platform missing from the list is not treated as hidden (the list may still be loading)", () => {
    expect(viewAfterPlatformsChange(open, [])).toBe(open);
  });
});
