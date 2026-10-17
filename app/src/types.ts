// Mirrors the DTOs in src-tauri/src/lib.rs (camelCase) and romm-core (snake_case for SaveView/StateView).

export interface Status {
  loggedIn: boolean;
  serverUrl: string | null;
  deviceName: string | null;
  version: string;
  warnings: string[];
}

export interface LoginArgs {
  server: string;
  code?: string;
  token?: string;
  insecure: boolean;
  deviceName?: string;
}

/** One thing that can play a platform: a standalone emulator or RetroArch with a core. */
export interface EmulatorChoice {
  /** `dolphin` or `retroarch:gambatte`; pass back to `setPlatformChoice` unchanged. */
  value: string;
  label: string;
  installed: boolean;
}

export interface Platform {
  id: number;
  slug: string;
  name: string;
  romCount: number;
  firmwareCount: number;
  emulator: string | null;
  emulatorName: string | null;
  emulatorInstalled: boolean;
  /** Only emulators/cores that can actually play this platform. Empty = nothing can yet. */
  choices: EmulatorChoice[];
  /** The saved choice's `value`, or "" for Automatic. */
  selected: string;
  /** What Automatic resolves to; null when nothing can play the platform. */
  automaticLabel: string | null;
  /** A saved emulator that can't play this platform and is being ignored. */
  ignoredOverride: string | null;
}

export interface Game {
  id: number;
  name: string;
  platformId: number;
  platformSlug: string;
  installed: boolean;
  sizeBytes: number;
  multiFile: boolean;
  coverPath: string | null;
}

export interface GamesPage {
  items: Game[];
  total: number | null;
}

export interface GamesArgs {
  platformId?: number;
  search?: string;
  offset: number;
  limit: number;
  installedOnly?: boolean;
}

export interface SyncSummary {
  uploaded: string[];
  downloaded: string[];
  archived: string[];
  conflicts: string[];
  failed: string[];
  stateUploaded: string[];
  stateDownloaded: string[];
  stateArchived: string[];
  stateConflicts: string[];
  stateSkipped: string[];
  stateFailed: string[];
}

export interface PlaySummary {
  playedSecs: number;
  exitCode: number | null;
  notes: string[];
  sync: SyncSummary;
}

export interface SyncAllResult {
  games: number;
  skipped: string[];
  summary: SyncSummary;
}

export type Phase = "started" | "syncing" | "running" | "saving" | "finished" | "failed";

export interface JobEvent<T = unknown> {
  romId: number;
  phase: Phase;
  error: string | null;
  result: T | null;
}

export interface ProgressEvent {
  romId: number;
  done: number;
  total: number | null;
}

export type SaveStatus = "synced" | "modified" | "never_synced";

export interface SaveView {
  name: string;
  size: number;
  modified: string | null;
  detail: string | null;
  status: SaveStatus;
}

export type StateStatus = "in_sync" | "local_newer" | "server_newer" | "local_only" | "server_only" | "conflict" | "unverified";

export interface StateView {
  name: string;
  local_size: number | null;
  local_modified: string | null;
  server_size: number | null;
  server_updated_at: string | null;
  status: StateStatus;
}

export interface EmulatorRef {
  id: string;
  name: string;
  installed: boolean;
  problem: string | null;
}

export interface Details {
  installed: boolean;
  installPath: string | null;
  emulator: EmulatorRef | null;
  saves: SaveView[];
  states: StateView[];
  statesError: string | null;
  statesEnabled: boolean;
}

export interface Emulator {
  id: string;
  name: string;
  installed: boolean;
  flatpak: boolean;
  path: string | null;
  dataDir: string | null;
  overridePath: string | null;
  platforms: string[];
  problem: string | null;
}

export interface Bios {
  file: string;
  status: string;
}

export type ConflictPolicy = "keep_both" | "server_wins" | "device_wins" | "ask";

export interface Settings {
  romDir: string;
  syncStates: boolean;
  maxStateMb: number;
  conflictPolicy: ConflictPolicy;
  deviceName: string | null;
  acceptInvalidCerts: boolean;
  platformEmulators: Record<string, string>;
}

/** Mirrors `romm_core::achievements` (snake_case, passed through as-is like SaveView/StateView). */
export type RaState = "ready" | "no_set" | "not_linked";

export interface Achievement {
  id: number;
  title: string;
  description: string;
  points: number;
  /** `progression`, `win_condition`, `missable`, or "". */
  kind: string;
  earned: boolean;
  /** Earned in hardcore mode. */
  hardcore: boolean;
  /** RFC 3339 (UTC). */
  earned_at: string | null;
  /** Server path of the badge to show (coloured if earned, greyed if not); load it with `loadCover`. */
  badge: string | null;
}

export interface AchievementsView {
  state: RaState;
  ra_username: string | null;
  total: number;
  earned: number;
  earned_hardcore: number;
  points_total: number;
  points_earned: number;
  /** `completed`, `mastered`, `beaten-softcore`, `beaten-hardcore`, or null. */
  award: string | null;
  last_earned: string | null;
  /** RomM holds no RetroAchievements progress for this user yet, so "0 earned" may mean "never fetched". */
  progress_missing: boolean;
  achievements: Achievement[];
  /** Served from the saved copy because the server couldn't be reached. */
  stale: boolean;
  fetched_at: string | null;
}

export type View =
  | { kind: "library"; platform: Platform | null }
  | { kind: "installed" }
  | { kind: "emulators" }
  | { kind: "settings" };
