import { describe, expect, it } from "vitest";
import { chosen, clearScope, countChosen, dirtyTabs, sameChoices, setChoice } from "./emulation";
import type { Settings } from "./types";

const base: Settings = {
  romDir: "/roms", syncStates: true, maxStateMb: 512, conflictPolicy: "keep_both", deviceName: null,
  acceptInvalidCerts: false, platformEmulators: {},
};

describe("choosing emulator settings", () => {
  it("reads an unset setting as 'don't change'", () => {
    expect(chosen({}, "pcsx2", "resolution")).toBe("");
    expect(chosen({ pcsx2: { resolution: "3" } }, "pcsx2", "resolution")).toBe("3");
    expect(chosen({ pcsx2: { resolution: "3" } }, "pcsx2", "fullscreen")).toBe("");
  });

  it("sets a choice without touching the object it was given", () => {
    const before = { pcsx2: { resolution: "2" } };
    const after = setChoice(before, "dolphin", "resolution", "4");
    expect(after).toEqual({ dolphin: { resolution: "4" }, pcsx2: { resolution: "2" } });
    expect(before).toEqual({ pcsx2: { resolution: "2" } });
  });

  it("clearing a choice removes it, and an emulator with nothing left disappears", () => {
    let v = setChoice({}, "pcsx2", "resolution", "3");
    v = setChoice(v, "pcsx2", "fullscreen", "on");
    v = setChoice(v, "pcsx2", "resolution", "");
    expect(v).toEqual({ pcsx2: { fullscreen: "on" } });
    expect(setChoice(v, "pcsx2", "fullscreen", "")).toEqual({});
  });

  it("clears every choice for one emulator and leaves the others", () => {
    const v = { pcsx2: { resolution: "3", fullscreen: "on" }, dolphin: { resolution: "2" } };
    expect(clearScope(v, "pcsx2")).toEqual({ dolphin: { resolution: "2" } });
    expect(countChosen(v, "pcsx2")).toBe(2);
    expect(countChosen(v, "xemu")).toBe(0);
  });

  it("treats equal choices as equal whatever order they were made in", () => {
    const a = setChoice(setChoice({}, "pcsx2", "resolution", "3"), "dolphin", "resolution", "2");
    const b = setChoice(setChoice({}, "dolphin", "resolution", "2"), "pcsx2", "resolution", "3");
    expect(sameChoices(a, b)).toBe(true);
    expect(JSON.stringify(a)).toBe(JSON.stringify(b));
    expect(sameChoices(a, setChoice(a, "pcsx2", "resolution", "4"))).toBe(false);
  });

  it("changing something and changing it back is not a change", () => {
    const saved = { pcsx2: { resolution: "3" } };
    const edited = setChoice(setChoice(saved, "pcsx2", "resolution", ""), "pcsx2", "resolution", "3");
    expect(sameChoices(saved, edited)).toBe(true);
  });
});

describe("which tabs have unsaved changes", () => {
  it("none when nothing changed", () => {
    expect(dirtyTabs(base, base).size).toBe(0);
  });

  it("marks only the tab a change belongs to", () => {
    expect([...dirtyTabs({ ...base, acceptInvalidCerts: true }, base)]).toEqual(["connection"]);
    expect([...dirtyTabs({ ...base, romDir: "/elsewhere" }, base)]).toEqual(["general"]);
    expect([...dirtyTabs({ ...base, maxStateMb: 64 }, base)]).toEqual(["general"]);
  });

  it("marks several at once", () => {
    const form = { ...base, syncStates: false, acceptInvalidCerts: true };
    expect([...dirtyTabs(form, base)].sort()).toEqual(["connection", "general"]);
  });
});
