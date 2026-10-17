import { describe, expect, it } from "vitest";
import { formatBytes, formatDuration, normalizeCode, summarizeSync, timeAgo } from "./format";
import type { SyncSummary } from "./types";

const empty = (): SyncSummary => ({
  uploaded: [], downloaded: [], archived: [], conflicts: [], failed: [],
  stateUploaded: [], stateDownloaded: [], stateArchived: [], stateConflicts: [], stateSkipped: [], stateFailed: [],
});

describe("formatBytes", () => {
  it("scales units", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1023)).toBe("1023 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(1048576)).toBe("1 MB");
    expect(formatBytes(150 * 1024 * 1024)).toBe("150 MB");
    expect(formatBytes(5 * 1024 ** 3)).toBe("5 GB");
  });
  it("handles missing values", () => {
    expect(formatBytes(null)).toBe("–");
    expect(formatBytes(undefined)).toBe("–");
  });
});

describe("formatDuration", () => {
  it("picks the right granularity", () => {
    expect(formatDuration(0)).toBe("0s");
    expect(formatDuration(59)).toBe("59s");
    expect(formatDuration(61)).toBe("1m 1s");
    expect(formatDuration(3700)).toBe("1h 1m");
  });
});

describe("timeAgo", () => {
  const now = Date.parse("2026-10-13T12:00:00Z");
  it("is relative for recent times", () => {
    expect(timeAgo("2026-10-13T11:59:50Z", now)).toBe("just now");
    expect(timeAgo("2026-10-13T11:55:00Z", now)).toBe("5 min ago");
    expect(timeAgo("2026-10-13T09:00:00Z", now)).toBe("3 h ago");
    expect(timeAgo("2026-10-11T12:00:00Z", now)).toBe("2 d ago");
  });
  it("tolerates junk", () => {
    expect(timeAgo(null, now)).toBe("–");
    expect(timeAgo("not a date", now)).toBe("not a date");
  });
});

describe("summarizeSync", () => {
  it("is empty when nothing happened", () => {
    expect(summarizeSync(empty())).toEqual([]);
  });
  it("pluralises and reports problems", () => {
    const s = empty();
    s.uploaded = ["a.srm"];
    s.stateDownloaded = ["x.state1", "x.state2"];
    s.stateConflicts = ["x.state3"];
    s.failed = ["boom"];
    expect(summarizeSync(s)).toEqual([
      "Uploaded 1 save",
      "Downloaded 2 save states",
      "1 conflict left for you to resolve",
      "Problem: boom",
    ]);
  });
  it("counts archived saves and states together", () => {
    const s = empty();
    s.archived = ["a"];
    s.stateArchived = ["b"];
    expect(summarizeSync(s)).toEqual(["Kept your local copy of 2 conflicting files on the server"]);
  });
});

describe("normalizeCode", () => {
  it("strips separators and spaces", () => {
    expect(normalizeCode(" 1234-5678 ")).toBe("12345678");
    expect(normalizeCode("ab cd")).toBe("ABCD");
  });
});
