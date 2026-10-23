import type { Achievement, AchievementsView, ConflictPolicy, SaveStatus, StateStatus, SyncSummary } from "./types";

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

// ---------------------------------------------------------------------- RetroAchievements

export type AchFilter = "all" | "earned" | "locked";

export const ACH_FILTERS: { value: AchFilter; label: string }[] = [
  { value: "all", label: "All" },
  { value: "earned", label: "Earned" },
  { value: "locked", label: "Locked" },
];

/** How many rows to show before "Show all": a full set can run to hundreds. */
export const ACH_PREVIEW = 8;

export function filterAchievements(list: Achievement[], f: AchFilter): Achievement[] {
  return f === "earned" ? list.filter((a) => a.earned) : f === "locked" ? list.filter((a) => !a.earned) : list;
}

export function achievementCounts(list: Achievement[]): Record<AchFilter, number> {
  const earned = list.filter((a) => a.earned).length;
  return { all: list.length, earned, locked: list.length - earned };
}

/**
 * What the achievements list is narrowed by: the earned/locked choice, the kinds to show (missable, progression),
 * and words to search for.
 */
export interface AchQuery {
  status: AchFilter;
  missable: boolean;
  progression: boolean;
  text: string;
}

/** Lower case with accents taken off, so "pokemon" finds "Pokémon". */
export function foldText(s: string): string {
  return s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
}

/** RetroAchievements marks an achievement `missable` when it can be lost for good (a one-time event, a point of no return). */
export const isMissable = (a: Achievement): boolean => a.kind === "missable";

/**
 * What the "Progression" filter shows: RetroAchievements' two story types, `progression` (a step along the way: a boss, a
 * stage, a story beat) and `win_condition` (the one that finishes the game).
 */
export const isProgression = (a: Achievement): boolean => a.kind === "progression" || a.kind === "win_condition";

/**
 * The achievements of the chosen kinds. An achievement has exactly one kind and the two groups (missable; progression and
 * win condition) don't overlap, so choosing both shows the ones that are either (asking for both at once could never match
 * anything). Choosing neither shows everything.
 */
export function filterByKind(list: Achievement[], q: Pick<AchQuery, "missable" | "progression">): Achievement[] {
  if (!q.missable && !q.progression) return list;
  return list.filter((a) => (q.missable && isMissable(a)) || (q.progression && isProgression(a)));
}

/** The achievements whose title or description contains every word of `text`, in any order. No words: all of them. */
export function searchAchievements(list: Achievement[], text: string): Achievement[] {
  const words = foldText(text).split(/\s+/).filter(Boolean);
  if (words.length === 0) return list;
  return list.filter((a) => {
    const hay = foldText(`${a.title} ${a.description}`);
    return words.every((w) => hay.includes(w));
  });
}

/** Search, the kinds and the earned/locked choice, all at once. */
export function queryAchievements(list: Achievement[], q: AchQuery): Achievement[] {
  return filterAchievements(filterByKind(searchAchievements(list, q.text), q), q.status);
}

/** Whether anything beyond the earned/locked choice is narrowing the list. */
export const isNarrowed = (q: AchQuery): boolean => q.missable || q.progression || q.text.trim() !== "";

/**
 * The number on each control: how many achievements choosing it would show, given the other controls. The kinds are
 * alternatives, so each counts its own achievements (within the search and the earned/locked choice) whether or not
 * the other kind is chosen; the earned/locked numbers follow whichever kinds are chosen.
 */
export interface AchFacets {
  status: Record<AchFilter, number>;
  missable: number;
  progression: number;
}

export function achievementFacets(list: Achievement[], q: AchQuery): AchFacets {
  const found = searchAchievements(list, q.text);
  const inStatus = filterAchievements(found, q.status);
  return {
    status: achievementCounts(filterByKind(found, q)),
    missable: inStatus.filter(isMissable).length,
    progression: inStatus.filter(isProgression).length,
  };
}

export function achievementPercent(v: Pick<AchievementsView, "earned" | "total">): number {
  return v.total > 0 ? Math.round((v.earned / v.total) * 100) : 0;
}

export function previewAchievements(list: Achievement[], expanded: boolean, limit = ACH_PREVIEW): Achievement[] {
  return expanded || list.length <= limit ? list : list.slice(0, limit);
}

export const AWARD: Record<string, { label: string; tone: "ok" | "warn" | "muted" }> = {
  mastered: { label: "Mastered", tone: "ok" },
  completed: { label: "Completed", tone: "ok" },
  "beaten-hardcore": { label: "Beaten (hardcore)", tone: "ok" },
  "beaten-softcore": { label: "Beaten", tone: "muted" },
};

export const ACH_KIND: Record<string, { label: string; tone: "warn" | "muted" }> = {
  missable: { label: "Missable", tone: "warn" },
  progression: { label: "Progression", tone: "muted" },
  win_condition: { label: "Win condition", tone: "muted" },
};

/** Full local date and time, for tooltips; "" when the timestamp isn't parseable. */
export function fullDate(iso: string | null | undefined): string {
  const t = iso ? Date.parse(iso) : NaN;
  return Number.isNaN(t) ? "" : new Date(t).toLocaleString();
}
