// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { EmulationScope, Settings as S, SettingsTab, Status } from "../types";

/**
 * Mounts the real Settings page against a fake Tauri backend. What this guards: the page is split
 * into tabs without losing anything (edits survive switching tabs, one Save covers them all), the
 * Emulation tab offers exactly what the backend describes and saves exactly what was chosen, and an
 * unsaved change shows on the tab it belongs to.
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
import { StoreProvider } from "../store";
import { Settings } from "./Settings";

(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

const flush = () => act(async () => { await new Promise((r) => setTimeout(r, 0)); });

const status = { serverUrl: "https://romm.example", deviceName: "Desk", version: "0.2.0" } as unknown as Status;

const SAVED: S = {
  romDir: "/games", syncStates: true, maxStateMb: 512, conflictPolicy: "keep_both", deviceName: null,
  acceptInvalidCerts: false, platformEmulators: {}, emulatorSettings: {},
};

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
let saved: S[];

function backend(opts: { settings?: S; schemaError?: string } = {}) {
  saved = [];
  h.backend = async (cmd, args) => {
    if (cmd === "settings") return structuredClone(opts.settings ?? SAVED);
    if (cmd === "emulation_schema") return opts.schemaError ? Promise.reject(opts.schemaError) : structuredClone(SCHEMA);
    if (cmd === "save_settings") { saved.push(structuredClone(args.settings)); return undefined; }
    throw new Error(`unexpected command ${cmd}`);
  };
}

async function mount(initialTab?: SettingsTab) {
  await act(async () => {
    root.render(
      <StoreProvider>
        <GamepadProvider>
          <div className="main"><Settings status={status} onDisconnected={() => {}} initialTab={initialTab} /></div>
        </GamepadProvider>
      </StoreProvider>,
    );
  });
  await flush();
}

const q = (sel: string) => host.querySelector<HTMLElement>(sel);
const tid = (id: string) => q(`[data-testid="${id}"]`);
const click = (el: Element | null) => act(async () => { (el as HTMLElement).click(); });
const goto = async (tab: SettingsTab) => { await click(tid(`tab-${tab}`)); await flush(); };
const selected = () => [...host.querySelectorAll("[role=tab]")].filter((t) => t.getAttribute("aria-selected") === "true").map((t) => t.getAttribute("data-testid"));
const hasDot = (tab: SettingsTab) => !!tid(`tab-${tab}`)?.querySelector(".tab-dot");

async function choose(testid: string, value: string) {
  const el = tid(testid) as HTMLSelectElement;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

async function type(testid: string, value: string) {
  const el = tid(testid) as HTMLInputElement;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

beforeEach(() => {
  h.calls.length = 0;
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

describe("Settings tabs", () => {
  it("opens on General, with the four tabs and only that tab's content", async () => {
    backend();
    await mount();
    expect([...host.querySelectorAll("[role=tab]")].map((t) => t.textContent)).toEqual(["General", "Emulation", "Controller", "Connection"]);
    expect(selected()).toEqual(["tab-general"]);
    expect(tid("rom-dir")).toBeTruthy();
    expect(tid("sync-states")).toBeTruthy();
    expect(tid("pad-enabled")).toBeNull();
    expect(host.querySelectorAll("[role=tabpanel]").length).toBe(1);
  });

  it("has every tab in the tab order, so the controller's focus can reach all of them", async () => {
    backend();
    await mount();
    for (const t of host.querySelectorAll<HTMLElement>("[role=tab]")) expect(t.tabIndex).toBe(0);
  });

  it("shows each section on its own tab", async () => {
    backend();
    await mount();
    await goto("controller");
    expect(tid("controller-settings")).toBeTruthy();
    expect(tid("pad-enabled")).toBeTruthy();
    expect(tid("rom-dir")).toBeNull();
    await goto("connection");
    expect(host.textContent).toContain("https://romm.example");
    expect(host.textContent).toContain("Disconnect");
    await goto("general");
    expect(host.textContent).toContain("Saves & save states");
  });

  it("can open on a given tab", async () => {
    backend();
    await mount("emulation");
    expect(selected()).toEqual(["tab-emulation"]);
    expect(tid("emu-scope-dolphin")).toBeTruthy();
  });

  it("moves between tabs with the arrow keys, wrapping at the ends", async () => {
    backend();
    await mount();
    const key = (k: string) => act(async () => { (q("[role=tab][aria-selected=true]") as HTMLElement).dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true })); });
    await key("ArrowRight");
    expect(selected()).toEqual(["tab-emulation"]);
    await key("ArrowLeft");
    await key("ArrowLeft");
    expect(selected()).toEqual(["tab-connection"]);
    await key("Home");
    expect(selected()).toEqual(["tab-general"]);
    await key("End");
    expect(selected()).toEqual(["tab-connection"]);
  });

  it("keeps an edit when you switch tabs and back, and marks the tab it is on", async () => {
    backend();
    await mount();
    await type("rom-dir", "/elsewhere");
    expect(hasDot("general")).toBe(true);
    await goto("emulation");
    expect(hasDot("general")).toBe(true);
    expect(hasDot("emulation")).toBe(false);
    await goto("general");
    expect((tid("rom-dir") as HTMLInputElement).value).toBe("/elsewhere");
  });
});

describe("Settings → Emulation", () => {
  it("lists what the backend describes, with each emulator's state", async () => {
    backend();
    await mount("emulation");
    expect(host.textContent).toContain("Dolphin");
    expect(host.textContent).toContain("RetroArch · Flycast");
    expect(tid("emu-scope-dolphin")!.textContent).toContain("Installed");
    expect(tid("emu-scope-retroarch:flycast")!.textContent).toContain("Not installed");
  });

  it("starts every setting on 'Don't change' and offers the emulator's own choices after it", async () => {
    backend();
    await mount("emulation");
    const sel = tid("emu-dolphin-resolution") as HTMLSelectElement;
    expect(sel.value).toBe("");
    expect([...sel.options].map((o) => o.textContent)).toEqual(["Don't change", "1× (native)", "3× (about 1080p)"]);
  });

  it("shows what is already chosen", async () => {
    backend({ settings: { ...SAVED, emulatorSettings: { dolphin: { resolution: "3" } } } });
    await mount("emulation");
    expect((tid("emu-dolphin-resolution") as HTMLSelectElement).value).toBe("3");
    expect(tid("emu-scope-dolphin")!.textContent).toContain("1 set");
  });

  it("a choice marks the tab, saves exactly that, and clears the mark", async () => {
    backend();
    await mount("emulation");
    expect((tid("save-settings") as HTMLButtonElement).disabled).toBe(true);

    await choose("emu-dolphin-resolution", "3");
    expect(hasDot("emulation")).toBe(true);
    expect((tid("save-settings") as HTMLButtonElement).disabled).toBe(false);

    await click(tid("save-settings"));
    await flush();
    expect(saved).toHaveLength(1);
    expect(saved[0].emulatorSettings).toEqual({ dolphin: { resolution: "3" } });
    expect(saved[0].romDir).toBe("/games"); // the rest of the form goes along unchanged
    expect(hasDot("emulation")).toBe(false);
    expect((tid("save-settings") as HTMLButtonElement).disabled).toBe(true);
  });

  it("one Save covers changes on several tabs", async () => {
    backend();
    await mount();
    await type("rom-dir", "/new");
    await goto("emulation");
    await choose("emu-retroarch:flycast-resolution", "1280x960");
    await click(tid("save-settings"));
    await flush();
    expect(saved[0].romDir).toBe("/new");
    expect(saved[0].emulatorSettings).toEqual({ "retroarch:flycast": { resolution: "1280x960" } });
  });

  it("putting a setting back to 'Don't change' is not a change", async () => {
    backend({ settings: { ...SAVED, emulatorSettings: { dolphin: { resolution: "3" } } } });
    await mount("emulation");
    await choose("emu-dolphin-resolution", "");
    expect(hasDot("emulation")).toBe(true);
    await choose("emu-dolphin-resolution", "3");
    expect(hasDot("emulation")).toBe(false);
  });

  it("resets everything for one emulator", async () => {
    backend({ settings: { ...SAVED, emulatorSettings: { dolphin: { resolution: "3" }, "retroarch:flycast": { resolution: "1280x960" } } } });
    await mount("emulation");
    await click(tid("emu-scope-dolphin")!.querySelector(".link-btn"));
    expect((tid("emu-dolphin-resolution") as HTMLSelectElement).value).toBe("");
    expect((tid("emu-retroarch:flycast-resolution") as HTMLSelectElement).value).toBe("1280x960");
    await click(tid("save-settings"));
    await flush();
    expect(saved[0].emulatorSettings).toEqual({ "retroarch:flycast": { resolution: "1280x960" } });
  });

  it("a failure to load the options is said plainly and leaves the rest of Settings working", async () => {
    backend({ schemaError: "backend went away" });
    await mount("emulation");
    expect(q("[role=alert]")!.textContent).toContain("backend went away");
    await goto("general");
    expect(tid("rom-dir")).toBeTruthy();
  });
});
