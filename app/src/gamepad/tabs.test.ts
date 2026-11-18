// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cycleTab, hasTabs } from "./tabs";

/** A page like Settings: a `.main` holding a tab list whose buttons select themselves when clicked. */
function page(labels: string[], active: number, opts: { insideMain?: boolean } = {}) {
  const wrap = document.createElement("div");
  wrap.className = opts.insideMain === false ? "elsewhere" : "main";
  const list = document.createElement("div");
  list.setAttribute("role", "tablist");
  labels.forEach((l, i) => {
    const b = document.createElement("button");
    b.setAttribute("role", "tab");
    b.textContent = l;
    b.setAttribute("aria-selected", String(i === active));
    b.addEventListener("click", () => list.querySelectorAll("[role=tab]").forEach((t, j) => t.setAttribute("aria-selected", String(j === i))));
    list.appendChild(b);
  });
  wrap.appendChild(list);
  document.body.appendChild(wrap);
  return list;
}
const current = (list: HTMLElement) => list.querySelector("[aria-selected=true]")?.textContent;

afterEach(() => { document.body.innerHTML = ""; });

describe("LB/RB on a page with tabs", () => {
  it("moves to the next and previous tab", () => {
    const list = page(["General", "Emulation", "Controller"], 0);
    expect(cycleTab(1)).toBe(true);
    expect(current(list)).toBe("Emulation");
    expect(cycleTab(1)).toBe(true);
    expect(current(list)).toBe("Controller");
    expect(cycleTab(-1)).toBe(true);
    expect(current(list)).toBe("Emulation");
  });

  it("wraps around at both ends", () => {
    const list = page(["A", "B", "C"], 2);
    cycleTab(1);
    expect(current(list)).toBe("A");
    cycleTab(-1);
    expect(current(list)).toBe("C");
  });

  it("leaves focus on the tab it switched to, so the controller carries on from there", () => {
    const list = page(["A", "B"], 0);
    cycleTab(1);
    expect(document.activeElement).toBe(list.querySelectorAll("[role=tab]")[1]);
  });

  it("does nothing, and says so, when the page has no tabs (so LB/RB can change platform instead)", () => {
    document.body.innerHTML = '<div class="main"><button>Not a tab</button></div>';
    expect(hasTabs()).toBe(false);
    expect(cycleTab(1)).toBe(false);
  });

  it("ignores a lone tab and tab lists outside the page content", () => {
    page(["Only"], 0);
    expect(hasTabs()).toBe(false);
    expect(cycleTab(1)).toBe(false);
    document.body.innerHTML = "";
    page(["A", "B"], 0, { insideMain: false });
    expect(hasTabs()).toBe(false);
    expect(cycleTab(1)).toBe(false);
  });
});
