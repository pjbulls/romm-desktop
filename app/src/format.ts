import type { ConflictPolicy, SaveStatus, StateStatus, SyncSummary } from "./types";

export function formatBytes(n: number | null | undefined): string {
  if (n == null) return "–";
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 || Number.isInteger(v) ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

export function formatDuration(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m ${secs % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

export function timeAgo(iso: string | null | undefined, now: number = Date.now()): string {
  if (!iso) return "–";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const s = Math.max(0, Math.round((now - t) / 1000));
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  if (s < 86400 * 14) return `${Math.round(s / 86400)} d ago`;
  return new Date(t).toLocaleDateString();
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** Plain-language lines describing what a sync did. Empty array = nothing changed. */
export function summarizeSync(s: SyncSummary): string[] {
  const lines: string[] = [];
  const add = (n: number, text: string) => n > 0 && lines.push(text);
  add(s.uploaded.length, `Uploaded ${plural(s.uploaded.length, "save")}`);
  add(s.downloaded.length, `Downloaded ${plural(s.downloaded.length, "save")}`);
  add(s.stateUploaded.length, `Uploaded ${plural(s.stateUploaded.length, "save state")}`);
  add(s.stateDownloaded.length, `Downloaded ${plural(s.stateDownloaded.length, "save state")}`);
  add(s.archived.length + s.stateArchived.length, `Kept your local copy of ${plural(s.archived.length + s.stateArchived.length, "conflicting file")} on the server`);
  add(s.conflicts.length + s.stateConflicts.length, `${plural(s.conflicts.length + s.stateConflicts.length, "conflict")} left for you to resolve`);
  add(s.stateSkipped.length, `Skipped ${plural(s.stateSkipped.length, "state")} (over the size limit)`);
  for (const f of [...s.failed, ...s.stateFailed]) lines.push(`Problem: ${f}`);
  return lines;
}

export function hasProblems(s: SyncSummary): boolean {
  return s.failed.length + s.stateFailed.length > 0;
}

export const SAVE_STATUS: Record<SaveStatus, { label: string; tone: "ok" | "warn" | "muted" }> = {
  synced: { label: "In sync", tone: "ok" },
  modified: { label: "Changed here", tone: "warn" },
  never_synced: { label: "Not uploaded yet", tone: "muted" },
};

export const STATE_STATUS: Record<StateStatus, { label: string; tone: "ok" | "warn" | "bad" | "muted" }> = {
  in_sync: { label: "In sync", tone: "ok" },
  local_newer: { label: "Newer here", tone: "warn" },
  server_newer: { label: "Newer on server", tone: "warn" },
  local_only: { label: "Only on this PC", tone: "muted" },
  server_only: { label: "Only on server", tone: "muted" },
  conflict: { label: "Conflict", tone: "bad" },
  unverified: { label: "Not compared yet", tone: "muted" },
};

export const POLICY_LABEL: Record<ConflictPolicy, { title: string; help: string }> = {
  keep_both: { title: "Keep both (recommended)", help: "Your version is archived on the server, then the server's version is used. Nothing is lost." },
  server_wins: { title: "Prefer the server", help: "The server's version is used. Your local file is backed up first." },
  device_wins: { title: "Prefer this PC", help: "Your version overwrites the one on the server." },
  ask: { title: "Leave it alone", help: "Conflicting files aren't touched; you're told about them." },
};

/** Pairing codes are shown as "1234-5678"; the server wants digits only. */
export function normalizeCode(input: string): string {
  return input.replace(/[^0-9a-zA-Z]/g, "").toUpperCase();
}
