// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn((..._a: unknown[]) => Promise.resolve()));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { report, startDiagnostics } from "./diagnostics";

const sent = () => invoke.mock.calls.map((c) => ({ cmd: c[0] as string, ...(c[1] as { line: string; report: boolean; visible: boolean }) }));

beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockClear();
  document.body.innerHTML = '<main class="main"><button class="game-card"></button><button class="game-card"></button><img></main>';
});
afterEach(() => vi.useRealTimers());

describe("report", () => {
  it("describes the page in counts and timings", () => {
    const { line, visible } = report(7.4);
    expect(visible).toBe(true);
    expect(line.startsWith("hb ")).toBe(true);
    for (const k of ["heap=", "nodes=", "cards=2", "imgs=0/1", "scroll=0", "lag=7ms", "vis=visible", "pads=0", "hints=0", "size="]) expect(line).toContain(k);
  });

  it("never reports a negative lag", () => {
    expect(report(-30).line).toContain("lag=0ms");
  });

  it("says when the controller hint bar is on screen", () => {
    document.body.insertAdjacentHTML("beforeend", '<div data-testid="hints"></div>');
    expect(report(0).line).toContain("hints=1");
  });

  it("holds no names or addresses", () => {
    document.body.innerHTML = '<main class="main"><button class="game-card">Secret Game Title</button></main>';
    expect(report(0).line).not.toContain("Secret");
  });
});

describe("startDiagnostics", () => {
  it("says it started, then reports every interval as a report the watchdog counts", () => {
    const stop = startDiagnostics({ intervalMs: 1000 });
    expect(sent()[0]).toMatchObject({ cmd: "diag_page", report: false });
    expect(sent()[0].line).toMatch(/^start ua=/);
    vi.advanceTimersByTime(3000);
    const beats = sent().filter((m) => m.report);
    expect(beats).toHaveLength(3);
    expect(beats.every((b) => b.line.startsWith("hb ") && b.visible)).toBe(true);
    stop();
    vi.advanceTimersByTime(5000);
    expect(sent().filter((m) => m.report)).toHaveLength(3);
  });

  it("logs errors, unhandled rejections and visibility changes, but they aren't reports", () => {
    const stop = startDiagnostics();
    invoke.mockClear();
    window.dispatchEvent(new ErrorEvent("error", { message: "boom", filename: "http://x/assets/index-abc.js", lineno: 42 }));
    const rej = new Event("unhandledrejection") as Event & { reason: unknown };
    rej.reason = new Error("nope");
    window.dispatchEvent(rej);
    document.dispatchEvent(new Event("visibilitychange"));
    const lines = sent().map((m) => m.line);
    expect(lines[0]).toBe("error boom @index-abc.js:42");
    expect(lines[1]).toContain("rejection Error: nope");
    expect(lines[2]).toBe("visibility visible");
    expect(sent().every((m) => !m.report)).toBe(true);
    stop();
  });

  it("stops listening when stopped", () => {
    const stop = startDiagnostics();
    stop();
    invoke.mockClear();
    window.dispatchEvent(new ErrorEvent("error", { message: "late" }));
    expect(sent()).toEqual([]);
  });

  it("does not break the page if the backend refuses a report", async () => {
    invoke.mockImplementation(() => Promise.reject(new Error("no backend")));
    const stop = startDiagnostics({ intervalMs: 500 });
    vi.advanceTimersByTime(1500);
    await Promise.resolve();
    stop();
    invoke.mockImplementation(() => Promise.resolve());
  });
});
