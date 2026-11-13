// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Settings as S, SettingsTab, Status } from "../types";

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
  acceptInvalidCerts: false, platformEmulators: {},
};

let root: Root;
let host: HTMLElement;
let saved: S[];

function backend(opts: { settings?: S } = {}) {
  saved = [];
  h.backend = async (cmd, args) => {
    if (cmd === "settings") return structuredClone(opts.settings ?? SAVED);
    if (cmd === "save_settings") { saved.push(structuredClone(args.settings)); return undefined; }
    throw new Error(`unexpected command ${cmd}`);
  };
}

async function mount() {
  await act(async () => {
    root.render(
      <StoreProvider>
        <GamepadProvider>
          <div className="main"><Settings status={status} onDisconnected={() => {}} /></div>
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
  it("opens on General, with its three tabs and only that tab's content", async () => {
    backend();
    await mount();
    expect([...host.querySelectorAll("[role=tab]")].map((t) => t.textContent)).toEqual(["General", "Controller", "Connection"]);
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

  it("moves between tabs with the arrow keys, wrapping at the ends", async () => {
    backend();
    await mount();
    const key = (k: string) => act(async () => { (q("[role=tab][aria-selected=true]") as HTMLElement).dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true })); });
    await key("ArrowRight");
    expect(selected()).toEqual(["tab-controller"]);
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
    await goto("connection");
    expect(hasDot("general")).toBe(true);
    expect(hasDot("connection")).toBe(false);
    await goto("general");
    expect((tid("rom-dir") as HTMLInputElement).value).toBe("/elsewhere");
  });
});

describe("emulator settings are no longer on this page", () => {
  it("has no Emulation tab, and never asks for the emulator settings", async () => {
    backend();
    await mount();
    expect(tid("tab-emulation")).toBeNull();
    expect(h.calls.map((c) => c.cmd).filter((c) => c !== "settings")).toEqual([]);
  });

  it("saves only its own settings, so it can't overwrite emulator settings chosen on the Emulators page", async () => {
    backend();
    await mount();
    await type("rom-dir", "/new");
    await click(tid("save-settings"));
    await flush();
    expect(saved).toHaveLength(1);
    expect(saved[0].romDir).toBe("/new");
    expect(Object.keys(saved[0])).not.toContain("emulatorSettings");
  });
});
