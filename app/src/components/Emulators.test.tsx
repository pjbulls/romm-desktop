// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { EmulationScope, Emulator, EmulatorSettings, EmulatorsTab, Platform } from "../types";

/**
 * Mounts the real Emulators & BIOS page against a fake Tauri backend. What this guards: the page's
 * three blocks each have their own tab (Executables, BIOS, Emulator mapping) and the emulator
 * settings, moved here from Settings, are the last one; edits to those survive switching tabs, are
 * saved on their own and only when asked, and nothing about them is requested until that tab is opened.
 */
const h = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: any }[],
  backend: null as null | ((cmd: string, args: any) => Promise<unknown>),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args: any) => {
    h.calls.push({ cmd, args });
    return h.backend!(cmd, args);
  },
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

import { GamepadProvider } from "../gamepad/GamepadProvider";
import { StoreProvider, useStore } from "../store";
import { Emulators } from "./Emulators";

(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

const flush = () => act(async () => { await new Promise((r) => setTimeout(r, 0)); });

const EMULATORS: Emulator[] = [
  { id: "dolphin", name: "Dolphin", installed: true, flatpak: false, path: "/usr/bin/dolphin-emu", dataDir: "/home/u/.dolphin-emu", overridePath: null, platforms: ["ngc", "wii"], problem: null },
  { id: "ppsspp", name: "PPSSPP", installed: false, flatpak: false, path: null, dataDir: null, overridePath: null, platforms: ["psp"], problem: "PPSSPP wasn't found on this PC." },
];

const platform = (id: number, slug: string, name: string, extra: Partial<Platform> = {}): Platform => ({
  id, slug, name, romCount: 4, firmwareCount: 0, emulator: "dolphin", emulatorName: "Dolphin", emulatorInstalled: true,
  choices: [{ value: "dolphin", label: "Dolphin", installed: true }], selected: "", automaticLabel: "Dolphin", ignoredOverride: null, hidden: false, ...extra,
});
const PLATFORMS: Platform[] = [
  platform(1, "ngc", "GameCube"),
  platform(2, "psx", "PlayStation", { firmwareCount: 2, emulator: "duckstation", emulatorName: "DuckStation" }),
  platform(3, "arcade", "Arcade", { choices: [], emulator: null, emulatorName: null, emulatorInstalled: false, automaticLabel: null }),
];

const SCHEMA: EmulationScope[] = [
  {
    scope: "dolphin", emulatorId: "dolphin", name: "Dolphin", core: null, installed: true, platforms: ["ngc", "wii"], changesFiles: false,
    settings: [{
      id: "resolution", label: "Internal resolution", help: "How sharp the picture is.",
      choices: [{ value: "1", label: "1× (native)" }, { value: "3", label: "3× (about 1080p)" }],
    }],
  },
  {
    scope: "retroarch:flycast", emulatorId: "retroarch", name: "RetroArch · Flycast", core: "flycast", installed: false, platforms: ["dc"], changesFiles: true,
    settings: [{
      id: "resolution", label: "Internal resolution", help: "",
      choices: [{ value: "640x480", label: "640×480 (native)" }, { value: "1280x960", label: "1280×960 (2×)" }],
    }],
  },
];

let root: Root;
let host: HTMLElement;
let stored: EmulatorSettings;
let saves: EmulatorSettings[];
let schemaError: string | null;
let saveError: string | null;
let hiddenCalls: [string, boolean][];

function fakeBackend() {
  h.backend = async (cmd, args) => {
    switch (cmd) {
      case "emulators": return structuredClone(EMULATORS);
      case "emulation_schema": return schemaError ? Promise.reject(schemaError) : structuredClone(SCHEMA);
      case "emulator_settings": return structuredClone(stored);
      case "save_emulator_settings":
        if (saveError) throw saveError;
        saves.push(structuredClone(args.values));
        stored = structuredClone(args.values);
        return undefined;
      case "set_platform_choice": return undefined;
      case "set_platform_hidden": return undefined;
      case "install_bios": return [{ file: "scph5501.bin", status: "downloaded" }];
      default: throw new Error(`unexpected command ${cmd}`);
    }
  };
}

function Toasts() {
  const { toasts } = useStore();
  return <ul data-testid="toasts">{toasts.map((t) => <li key={t.id} data-tone={t.tone}>{[t.text, ...(t.detail ?? [])].join(" ")}</li>)}</ul>;
}

async function mount(platforms: Platform[] = PLATFORMS, withHide = true) {
  fakeBackend();
  await act(async () => {
    root.render(
      <StoreProvider>
        <GamepadProvider>
          <div className="main">
            <Emulators platforms={platforms} onChanged={() => {}} onHiddenChange={withHide ? (slug, hidden) => { hiddenCalls.push([slug, hidden]); } : undefined} />
          </div>
          <Toasts />
        </GamepadProvider>
      </StoreProvider>,
    );
  });
  await flush();
}

const q = (sel: string) => host.querySelector<HTMLElement>(sel);
const tid = (id: string) => q(`[data-testid="${id}"]`);
const click = (el: Element | null) => act(async () => { (el as HTMLElement).click(); });
const goto = async (tab: EmulatorsTab) => { await click(tid(`tab-${tab}`)); await flush(); };
const selected = () => [...host.querySelectorAll("[role=tab]")].filter((t) => t.getAttribute("aria-selected") === "true").map((t) => t.getAttribute("data-testid"));
const hasDot = (tab: EmulatorsTab) => !!tid(`tab-${tab}`)?.querySelector(".tab-dot");
const calls = (cmd: string) => h.calls.filter((c) => c.cmd === cmd);
const toasts = () => [...host.querySelectorAll("[data-testid=toasts] li")].map((li) => ({ tone: li.getAttribute("data-tone"), text: li.textContent }));

async function choose(testid: string, value: string) {
  const el = tid(testid) as HTMLSelectElement;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

beforeEach(() => {
  h.calls.length = 0;
  stored = {};
  saves = [];
  schemaError = null;
  saveError = null;
  hiddenCalls = [];
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

describe("Emulators & BIOS tabs", () => {
  it("has Executables, BIOS, Emulator mapping and Emulator settings, in that order, opening on Executables", async () => {
    await mount();
    expect([...host.querySelectorAll("[role=tab]")].map((t) => t.textContent)).toEqual(["Executables", "BIOS", "Emulator mapping", "Emulator settings"]);
    expect(selected()).toEqual(["tab-executables"]);
    expect(host.querySelectorAll("[role=tabpanel]").length).toBe(1);
  });

  it("has every tab in the tab order, so the controller's focus can reach all of them", async () => {
    await mount();
    for (const t of host.querySelectorAll<HTMLElement>("[role=tab]")) expect(t.tabIndex).toBe(0);
  });

  it("Executables holds the emulators and nothing from the other blocks", async () => {
    await mount();
    expect(tid("emu-dolphin")!.textContent).toContain("Found");
    expect(tid("emu-ppsspp")!.textContent).toContain("Not found");
    expect(tid("bios-psx")).toBeNull();
    expect(tid("show-ngc")).toBeNull();
    expect(tid("emu-scope-dolphin")).toBeNull();
  });

  it("BIOS holds the BIOS files and nothing from the other blocks", async () => {
    await mount();
    await goto("bios");
    expect(tid("bios-psx")).toBeTruthy();
    expect(host.textContent).toContain("2 files → DuckStation");
    expect(tid("emu-dolphin")).toBeNull();
    expect(tid("show-ngc")).toBeNull();
  });

  it("BIOS says so when no platform has any", async () => {
    await mount([platform(1, "ngc", "GameCube")]);
    await goto("bios");
    expect(host.textContent).toContain("None of your platforms have BIOS files");
  });

  it("installing a BIOS works from its tab", async () => {
    await mount();
    await goto("bios");
    await click(tid("bios-psx"));
    await flush();
    expect(calls("install_bios").map((c) => c.args)).toEqual([{ platformSlug: "psx" }]);
    expect(host.textContent).toContain("scph5501.bin");
  });

  it("Emulator mapping holds the platform rows, with their emulator choice and show/hide box", async () => {
    await mount();
    await goto("mapping");
    expect([...host.querySelectorAll(".rows .row-name")].map((e) => e.textContent)).toEqual(["Arcade", "GameCube", "PlayStation"]);
    expect(tid("show-ngc")).toBeTruthy();
    expect(host.textContent).toContain("Untick a platform");
    expect(tid("emu-dolphin")).toBeNull();
    expect(tid("bios-psx")).toBeNull();
  });

  it("picking an emulator for a platform still saves from the mapping tab", async () => {
    await mount();
    await goto("mapping");
    const sel = q("[aria-label='Emulator for GameCube']") as HTMLSelectElement;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(sel, "dolphin");
      sel.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await flush();
    expect(calls("set_platform_choice").map((c) => c.args)).toEqual([{ slug: "ngc", choice: "dolphin" }]);
  });

  it("hiding a platform still works from the mapping tab", async () => {
    await mount();
    await goto("mapping");
    await click(tid("show-arcade"));
    await flush();
    expect(hiddenCalls).toEqual([["arcade", true]]);
    expect(calls("set_platform_hidden").map((c) => c.args)).toEqual([{ slug: "arcade", hidden: true }]);
  });

  it("the link on Executables opens Emulator settings", async () => {
    await mount();
    await click(tid("go-emulator-settings"));
    await flush();
    expect(selected()).toEqual(["tab-settings"]);
    expect(tid("emu-scope-dolphin")).toBeTruthy();
  });

  it("moves between tabs with the arrow keys, wrapping at the ends", async () => {
    await mount();
    const key = (k: string) => act(async () => { (q("[role=tab][aria-selected=true]") as HTMLElement).dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true })); });
    await key("ArrowRight");
    expect(selected()).toEqual(["tab-bios"]);
    await key("ArrowLeft");
    await key("ArrowLeft");
    expect(selected()).toEqual(["tab-settings"]);
    await key("Home");
    expect(selected()).toEqual(["tab-executables"]);
    await key("End");
    expect(selected()).toEqual(["tab-settings"]);
  });

  it("the page shows its tabs at once, even while the emulator list is still loading", async () => {
    fakeBackend();
    const slow = h.backend!;
    let release!: () => void;
    const gate = new Promise<void>((r) => { release = r; });
    h.backend = async (cmd, args) => { if (cmd === "emulators") await gate; return slow(cmd, args); };
    await act(async () => { root.render(<StoreProvider><GamepadProvider><div className="main"><Emulators platforms={PLATFORMS} onChanged={() => {}} /></div></GamepadProvider></StoreProvider>); });
    await flush();
    expect(tid("tab-bios")).toBeTruthy();
    expect(q(".loading-row")).not.toBeNull();
    await goto("bios");
    expect(tid("bios-psx")).toBeTruthy(); // BIOS doesn't need the emulator list
    release();
    await flush();
  });
});

describe("Emulators & BIOS → Emulator settings", () => {
  it("asks for nothing about emulator settings until the tab is opened, and only once", async () => {
    await mount();
    expect(calls("emulation_schema")).toEqual([]);
    expect(calls("emulator_settings")).toEqual([]);
    await goto("settings");
    expect(calls("emulation_schema")).toHaveLength(1);
    expect(calls("emulator_settings")).toHaveLength(1);
    await goto("mapping");
    await goto("settings");
    expect(calls("emulation_schema")).toHaveLength(1);
    expect(calls("emulator_settings")).toHaveLength(1);
  });

  it("lists what the backend describes, with each emulator's state, and no heading repeating the tab's name", async () => {
    await mount();
    await goto("settings");
    expect(host.textContent).toContain("Dolphin");
    expect(host.textContent).toContain("RetroArch · Flycast");
    expect(tid("emu-scope-dolphin")!.textContent).toContain("Installed");
    expect(tid("emu-scope-retroarch:flycast")!.textContent).toContain("Not installed");
    expect([...host.querySelectorAll("h3")].map((e) => e.textContent)).toEqual(["Dolphin", "RetroArch · Flycast"]);
  });

  it("starts every setting on 'Don't change' and offers the emulator's own choices after it", async () => {
    await mount();
    await goto("settings");
    const sel = tid("emu-dolphin-resolution") as HTMLSelectElement;
    expect(sel.value).toBe("");
    expect([...sel.options].map((o) => o.textContent)).toEqual(["Don't change", "1× (native)", "3× (about 1080p)"]);
  });

  it("shows what is already chosen", async () => {
    stored = { dolphin: { resolution: "3" } };
    await mount();
    await goto("settings");
    expect((tid("emu-dolphin-resolution") as HTMLSelectElement).value).toBe("3");
    expect(tid("emu-scope-dolphin")!.textContent).toContain("1 set");
    expect(hasDot("settings")).toBe(false);
  });

  it("a choice marks the tab, and Save sends exactly that and clears the mark", async () => {
    await mount();
    await goto("settings");
    expect((tid("save-emulator-settings") as HTMLButtonElement).disabled).toBe(true);

    await choose("emu-dolphin-resolution", "3");
    expect(hasDot("settings")).toBe(true);
    expect((tid("save-emulator-settings") as HTMLButtonElement).disabled).toBe(false);

    await click(tid("save-emulator-settings"));
    await flush();
    expect(calls("save_emulator_settings").map((c) => c.args)).toEqual([{ values: { dolphin: { resolution: "3" } } }]);
    expect(hasDot("settings")).toBe(false);
    expect((tid("save-emulator-settings") as HTMLButtonElement).disabled).toBe(true);
    expect(toasts()).toEqual([{ tone: "success", text: "Emulator settings saved" }]);
  });

  it("saving touches nothing but the emulator settings", async () => {
    await mount();
    await goto("settings");
    await choose("emu-dolphin-resolution", "3");
    await click(tid("save-emulator-settings"));
    await flush();
    expect(h.calls.map((c) => c.cmd)).not.toContain("save_settings");
    expect(h.calls.map((c) => c.cmd)).not.toContain("settings");
  });

  it("keeps unsaved edits when you switch tabs and back, with the mark showing meanwhile", async () => {
    await mount();
    await goto("settings");
    await choose("emu-dolphin-resolution", "3");
    await goto("bios");
    expect(hasDot("settings")).toBe(true);
    expect(tid("save-emulator-settings")).toBeNull(); // the Save bar belongs to its own tab
    await goto("settings");
    expect((tid("emu-dolphin-resolution") as HTMLSelectElement).value).toBe("3");
    expect(calls("save_emulator_settings")).toEqual([]);
  });

  it("putting a setting back to 'Don't change' is not a change", async () => {
    stored = { dolphin: { resolution: "3" } };
    await mount();
    await goto("settings");
    await choose("emu-dolphin-resolution", "");
    expect(hasDot("settings")).toBe(true);
    await choose("emu-dolphin-resolution", "3");
    expect(hasDot("settings")).toBe(false);
  });

  it("Discard puts back what was saved", async () => {
    stored = { dolphin: { resolution: "3" } };
    await mount();
    await goto("settings");
    await choose("emu-dolphin-resolution", "1");
    await click([...host.querySelectorAll<HTMLElement>(".savebar .btn")].find((b) => b.textContent === "Discard")!);
    expect((tid("emu-dolphin-resolution") as HTMLSelectElement).value).toBe("3");
    expect(hasDot("settings")).toBe(false);
  });

  it("resets everything for one emulator", async () => {
    stored = { dolphin: { resolution: "3" }, "retroarch:flycast": { resolution: "1280x960" } };
    await mount();
    await goto("settings");
    await click(tid("emu-scope-dolphin")!.querySelector(".link-btn"));
    expect((tid("emu-dolphin-resolution") as HTMLSelectElement).value).toBe("");
    expect((tid("emu-retroarch:flycast-resolution") as HTMLSelectElement).value).toBe("1280x960");
    await click(tid("save-emulator-settings"));
    await flush();
    expect(saves).toEqual([{ "retroarch:flycast": { resolution: "1280x960" } }]);
  });

  it("a failed save is said plainly and leaves the edit in place to try again", async () => {
    await mount();
    await goto("settings");
    await choose("emu-dolphin-resolution", "3");
    saveError = "disk full";
    await click(tid("save-emulator-settings"));
    await flush();
    expect(toasts()).toEqual([{ tone: "error", text: "Couldn't save emulator settings disk full" }]);
    expect(hasDot("settings")).toBe(true);
    expect((tid("emu-dolphin-resolution") as HTMLSelectElement).value).toBe("3");
    saveError = null;
    await click(tid("save-emulator-settings"));
    await flush();
    expect(saves).toEqual([{ dolphin: { resolution: "3" } }]);
    expect(hasDot("settings")).toBe(false);
  });

  it("a failure to load the options is said plainly and leaves the rest of the page working", async () => {
    schemaError = "backend went away";
    await mount();
    await goto("settings");
    expect(q("[role=alert]")!.textContent).toContain("backend went away");
    await goto("executables");
    expect(tid("emu-dolphin")).toBeTruthy();
  });
});
