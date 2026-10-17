import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { PlatformChoice, choiceText } from "./PlatformChoice";
import type { Platform } from "../types";

const base: Platform = {
  id: 1, slug: "gb", name: "Game Boy", romCount: 12, firmwareCount: 0,
  emulator: "retroarch", emulatorName: "RetroArch", emulatorInstalled: true,
  choices: [], selected: "", automaticLabel: null, ignoredOverride: null,
};

const gameBoy: Platform = {
  ...base,
  automaticLabel: "RetroArch · Gambatte",
  choices: [
    { value: "retroarch:gambatte", label: "RetroArch · Gambatte", installed: true },
    { value: "retroarch:mgba", label: "RetroArch · mGBA", installed: true },
    { value: "retroarch:vbam", label: "RetroArch · VBA-M", installed: false },
  ],
};

const render = (p: Platform) => renderToStaticMarkup(<PlatformChoice platform={p} onChange={() => {}} />);
const options = (html: string) => [...html.matchAll(/<option value="([^"]*)"[^>]*>([^<]*)<\/option>/g)].map((m) => [m[1], m[2]]);

describe("PlatformChoice", () => {
  it("lists Automatic first, then exactly the supplied choices in order", () => {
    expect(options(render(gameBoy))).toEqual([
      ["", "Automatic (RetroArch · Gambatte)"],
      ["retroarch:gambatte", "RetroArch · Gambatte"],
      ["retroarch:mgba", "RetroArch · mGBA"],
      ["retroarch:vbam", "RetroArch · VBA-M (not installed)"],
    ]);
  });

  it("offers nothing beyond what it was given (no other emulators leak in)", () => {
    const html = render(gameBoy);
    for (const other of ["xemu", "Xbox", "Dolphin", "DuckStation", "PCSX2", "PPSSPP", "Azahar"]) {
      expect(html).not.toContain(other);
    }
  });

  it("marks the saved choice as selected", () => {
    const html = render({ ...gameBoy, selected: "retroarch:mgba" });
    expect(html).toMatch(/<option value="retroarch:mgba" selected="">/);
    expect(html).not.toMatch(/<option value="" selected/);
  });

  it("selects Automatic when nothing is saved", () => {
    expect(render(gameBoy)).toMatch(/<option value="" selected="">/);
  });

  it("falls back to a plain Automatic label when there is no default", () => {
    const p = { ...gameBoy, automaticLabel: null };
    expect(options(render(p))[0]).toEqual(["", "Automatic"]);
  });

  it("shows a message instead of a dropdown when nothing can play the platform", () => {
    const html = render({ ...base, slug: "switch", name: "Nintendo Switch" });
    expect(html).not.toContain("<select");
    expect(html).toContain("No supported emulator yet");
    expect(html).toContain('data-testid="no-emulator-switch"');
  });

  it("explains a saved emulator that can't play the platform and is being ignored", () => {
    const html = render({ ...gameBoy, ignoredOverride: "xemu" });
    expect(html).toContain('data-testid="ignored-gb"');
    expect(html).toContain("xemu was saved for this, but it can&#x27;t play Game Boy, so Automatic is used.");
    expect(render(gameBoy)).not.toContain("ignored-");
  });

  it("labels the select for assistive tech and the gamepad", () => {
    expect(render(gameBoy)).toContain('aria-label="Emulator for Game Boy"');
  });

  it("only flags uninstalled choices", () => {
    expect(choiceText("Dolphin", true)).toBe("Dolphin");
    expect(choiceText("Dolphin", false)).toBe("Dolphin (not installed)");
  });
});
