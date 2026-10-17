import { describe, expect, it } from "vitest";
import { activeActions, DEFAULT_TIMING, initialState, mergeSnapshots, scrollAxis, step, type Snapshot } from "./input";
import { controllerKind, friendlyName, labelsFor } from "./layouts";
import { applyKey, OSK_ROWS } from "./osk";
import { pickNext, type Box } from "./spatial";

const pad = (pressed: number[] = [], axes: number[] = [0, 0, 0, 0]): Snapshot => {
  const buttons: boolean[] = Array(17).fill(false);
  pressed.forEach((i) => (buttons[i] = true));
  return { buttons, axes };
};
const armed = () => step(initialState(), pad(), 0).state;

describe("input mapping", () => {
  it("maps standard buttons and the d-pad", () => {
    expect([...activeActions(pad([0, 9, 14]))].sort()).toEqual(["accept", "left", "start"]);
  });
  it("turns the left stick into one dominant direction with a threshold", () => {
    expect([...activeActions(pad([], [0.4, 0, 0, 0]))]).toEqual([]);
    expect([...activeActions(pad([], [0.9, -0.6, 0, 0]))]).toEqual(["right"]);
    expect([...activeActions(pad([], [-0.3, 0.8, 0, 0]))]).toEqual(["down"]);
  });
  it("applies a deadzone to the scroll stick", () => {
    expect(scrollAxis(pad([], [0, 0, 0, 0.1]))).toBe(0);
    expect(scrollAxis(pad([], [0, 0, 0, -0.9]))).toBe(-0.9);
  });
  it("merges pads: any button, strongest axis", () => {
    const m = mergeSnapshots([pad([0], [0.2, 0, 0, 0]), pad([1], [-0.7, 0, 0, 0])]);
    expect(m.buttons[0] && m.buttons[1]).toBe(true);
    expect(m.axes[0]).toBe(-0.7);
  });
});

describe("press and repeat timing", () => {
  it("fires once on press, not while held, for non-repeating buttons", () => {
    let s = armed();
    let r = step(s, pad([0]), 100);
    expect(r.fired).toEqual(["accept"]);
    r = step(r.state, pad([0]), 2000);
    expect(r.fired).toEqual([]);
    s = step(r.state, pad(), 2100).state;
    expect(step(s, pad([0]), 2200).fired).toEqual(["accept"]);
  });
  it("repeats directions after the delay, at the interval", () => {
    const t = DEFAULT_TIMING;
    let r = step(armed(), pad([15]), 0);
    expect(r.fired).toEqual(["right"]);
    r = step(r.state, pad([15]), t.delay - 10);
    expect(r.fired).toEqual([]);
    r = step(r.state, pad([15]), t.delay);
    expect(r.fired).toEqual(["right"]);
    r = step(r.state, pad([15]), t.delay + t.interval - 5);
    expect(r.fired).toEqual([]);
    r = step(r.state, pad([15]), t.delay + t.interval);
    expect(r.fired).toEqual(["right"]);
  });
  it("handles two buttons at once and releases independently", () => {
    let r = step(armed(), pad([0, 12]), 0);
    expect(r.fired.sort()).toEqual(["accept", "up"]);
    r = step(r.state, pad([12]), 50);
    expect(r.fired).toEqual([]);
    expect(Object.keys(r.state.held)).toEqual(["up"]);
  });
});

describe("safety: input held while the app wasn't listening", () => {
  it("ignores a button that is already down until everything is released", () => {
    let r = step(initialState(), pad([0]), 0); // e.g. the button that closed the emulator
    expect(r.fired).toEqual([]);
    r = step(r.state, pad([0]), 500);
    expect(r.fired).toEqual([]);
    r = step(r.state, pad(), 600);
    expect(r.state.armed).toBe(true);
    expect(step(r.state, pad([0]), 700).fired).toEqual(["accept"]);
  });
  it("also blocks the scroll stick until it's centred", () => {
    const r = step(initialState(), pad([], [0, 0, 0, 1]), 0);
    expect(r.scrollY).toBe(0);
  });
});

// A 5-column grid of 178×238 cards with 18px gutters, like the library, plus a sidebar on the left.
const card = (col: number, row: number): Box => ({ left: 288 + col * 196, top: 80 + row * 297, right: 288 + col * 196 + 178, bottom: 80 + row * 297 + 238 });
const grid = Array.from({ length: 15 }, (_, i) => ({ item: i, box: card(i % 5, Math.floor(i / 5)) }));
const side = Array.from({ length: 4 }, (_, i) => ({ item: 100 + i, box: { left: 10, top: 220 + i * 34, right: 249, bottom: 252 + i * 34 } }));
const next = (from: number, dir: "up" | "down" | "left" | "right", extra = side) =>
  pickNext(grid.find((g) => g.item === from)!.box, [...grid.filter((g) => g.item !== from), ...extra], dir);

describe("spatial navigation", () => {
  it("moves through a grid like you'd expect", () => {
    expect(next(6, "right")).toBe(7);
    expect(next(6, "left")).toBe(5);
    expect(next(6, "down")).toBe(11);
    expect(next(6, "up")).toBe(1);
  });
  it("does not wrap or go diagonal at the edges", () => {
    expect(next(4, "right")).toBeNull();
    expect(next(2, "up")).toBeNull();
    expect(next(12, "down")).toBeNull();
  });
  it("leaves the grid for the sidebar entry level with the card", () => {
    // A tall sidebar list (one row per 34px from y=80) so there is an entry at every card's height.
    const tall = Array.from({ length: 20 }, (_, i) => ({ item: 200 + i, box: { left: 10, top: 80 + i * 34, right: 249, bottom: 112 + i * 34 } }));
    const entryAt = (item: number) => tall.find((t) => t.item === item)!.box;
    const pick = (from: number) => pickNext(grid.find((g) => g.item === from)!.box, tall, "left");
    const centre = (id: number | null) => { const b = entryAt(id!); return (b.top + b.bottom) / 2; };
    // Row 0 (centre y=199) and row 1 (centre y=496) each land on the entry closest to their own height.
    expect(Math.abs(centre(pick(0)) - 199)).toBeLessThan(20);
    expect(Math.abs(centre(pick(5)) - 496)).toBeLessThan(20);
  });
  it("falls back to the nearest sidebar entry when none is level with the card", () => {
    expect(next(0, "left")).toBe(100); // overlaps rows 100-102; closest centre is 100
    expect(next(5, "left")).toBe(103); // entirely below the list; nearest is the last entry
  });
  it("enters the grid from the sidebar", () => {
    const from = side[1].box;
    expect([0, 5]).toContain(pickNext(from, grid, "right"));
  });
  it("walks a vertical list", () => {
    expect(pickNext(side[1].box, [side[0], side[2], side[3]], "down")).toBe(102);
    expect(pickNext(side[1].box, [side[0], side[2], side[3]], "up")).toBe(100);
  });
});

describe("spatial navigation with a focused element that is lifted and scaled", () => {
  // Controller mode lifts the focused card 4px and scales it 1.04, which used to make its same-row
  // neighbour look "below" it, so Down moved sideways instead of to the next row.
  const lifted = (b: Box): Box => {
    const w = b.right - b.left, h = b.bottom - b.top, grow = 0.04;
    return { left: b.left - (w * grow) / 2, right: b.right + (w * grow) / 2, top: b.top - 4 - (h * grow) / 2, bottom: b.bottom - 4 + (h * grow) / 2 };
  };
  const from = lifted(card(2, 0));
  const others = grid.filter((g) => g.item !== 2);
  it("Down goes to the next row, not the neighbour", () => {
    expect(pickNext(from, others, "down")).toBe(7);
  });
  it("Left and Right still reach the neighbours", () => {
    expect(pickNext(from, others, "right")).toBe(3);
    expect(pickNext(from, others, "left")).toBe(1);
  });
  it("Up from the second row returns to the first", () => {
    expect(pickNext(lifted(card(2, 1)), grid.filter((g) => g.item !== 7), "up")).toBe(2);
  });
});

describe("spatial navigation prefers elements that are lined up, however far", () => {
  const box = (left: number, top: number, w = 200, h = 40): Box => ({ left, top, right: left + w, bottom: top + h });
  it("Down stays in the page instead of hopping to a nearer sidebar item", () => {
    // A button near the bottom of the visible page; a sidebar item is physically closer than the next page item.
    const from = box(400, 600);
    const sidebarItem = { item: "sidebar", box: box(10, 780, 239, 32) };
    const nextInPage = { item: "page", box: box(400, 1500) };
    expect(pickNext(from, [sidebarItem, nextInPage], "down")).toBe("page");
  });
  it("still uses a non-aligned element when nothing is lined up", () => {
    const from = box(400, 600);
    expect(pickNext(from, [{ item: "sidebar", box: box(10, 780, 239, 32) }], "down")).toBe("sidebar");
  });
  it("Left from the page reaches the sidebar when it is level with you", () => {
    const from = box(400, 500);
    expect(pickNext(from, [{ item: "side-level", box: box(10, 495, 239, 32) }, { item: "side-high", box: box(10, 100, 239, 32) }], "left")).toBe("side-level");
  });
  it("a sliver of overlap doesn't count as lined up", () => {
    const from = box(400, 600, 200, 40);
    const barelyTouching = { item: "touch", box: box(595, 700, 200, 40) };   // overlaps by 5px of 200
    const further = { item: "aligned", box: box(420, 1000, 200, 40) };
    expect(pickNext(from, [barelyTouching, further], "down")).toBe("aligned");
  });
});

describe("controller labels", () => {
  it("recognises common pads", () => {
    expect(controllerKind("Xbox 360 Controller (XInput STANDARD GAMEPAD)")).toBe("xbox");
    expect(controllerKind("054c-0ce6-DualSense Wireless Controller")).toBe("playstation");
    expect(controllerKind("Wireless Gamepad (STANDARD GAMEPAD Vendor: 057e Product: 2009)")).toBe("nintendo");
    expect(controllerKind("USB Gamepad")).toBe("generic");
  });
  it("labels by platform and cleans names", () => {
    expect(labelsFor("playstation").accept).toBe("✕");
    expect(labelsFor("nintendo").back).toBe("A");
    expect(labelsFor("generic").accept).toBe("A");
    expect(friendlyName("Xbox 360 Controller (XInput STANDARD GAMEPAD)")).toBe("Xbox 360 Controller");
  });
});

describe("on-screen keyboard", () => {
  it("types, backspaces and spaces", () => {
    let r = applyKey("", "h", false);
    r = applyKey(r.value, "i", r.shift);
    r = applyKey(r.value, "Space", r.shift);
    r = applyKey(r.value, "Backspace", r.shift);
    expect(r.value).toBe("hi");
  });
  it("shift is one-shot", () => {
    let r = applyKey("", "Shift", false);
    expect(r.shift).toBe(true);
    r = applyKey(r.value, "a", r.shift);
    r = applyKey(r.value, "b", r.shift);
    expect(r.value).toBe("Ab");
  });
  it("Done reports completion and backspace handles empty strings and emoji", () => {
    expect(applyKey("x", "Done", false).done).toBe(true);
    expect(applyKey("", "Backspace", false).value).toBe("");
    expect(applyKey("a😀", "Backspace", false).value).toBe("a");
  });
  it("offers what's needed to type a server address", () => {
    const keys = new Set(OSK_ROWS.flat());
    for (const c of "0123456789.:/-_@") expect(keys.has(c)).toBe(true);
  });
});
