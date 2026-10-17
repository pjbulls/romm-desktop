import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  Bios, Details, Emulator, GamesArgs, GamesPage, JobEvent, LoginArgs, Platform, ProgressEvent, Settings, Status,
} from "./types";

/** Tauri rejects with the Rust `String` error; normalise anything else. */
export function errMsg(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

export const api = {
  status: () => invoke<Status>("status"),
  login: (args: LoginArgs) => invoke<Status>("login", { args }),
  logout: () => invoke<Status>("logout"),
  platforms: () => invoke<Platform[]>("platforms"),
  games: (args: GamesArgs) => invoke<GamesPage>("games", { args }),
  cover: (romId: number, path: string) => invoke<string | null>("cover", { romId, path }),
  installGame: (romId: number) => invoke<void>("install_game", { romId }),
  uninstallGame: (romId: number) => invoke<void>("uninstall_game", { romId }),
  playGame: (romId: number) => invoke<void>("play_game", { romId }),
  syncGame: (romId: number) => invoke<void>("sync_game", { romId }),
  syncAll: () => invoke<void>("sync_all"),
  gameDetails: (romId: number, platformSlug: string) => invoke<Details>("game_details", { romId, platformSlug }),
  emulators: () => invoke<Emulator[]>("emulators"),
  setEmulatorPath: (id: string, path: string | null) => invoke<void>("set_emulator_path", { id, path }),
  setPlatformEmulator: (slug: string, emulator: string | null) => invoke<void>("set_platform_emulator", { slug, emulator }),
  installBios: (platformSlug: string) => invoke<Bios[]>("install_bios", { platformSlug }),
  settings: () => invoke<Settings>("settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
};

export async function pickFile(title: string): Promise<string | null> {
  const r = await open({ title, multiple: false, directory: false });
  return typeof r === "string" ? r : null;
}

export async function pickFolder(title: string): Promise<string | null> {
  const r = await open({ title, multiple: false, directory: true });
  return typeof r === "string" ? r : null;
}

export type Topic = "install" | "play" | "sync" | "sync-all";

export function onJob<T = unknown>(topic: Topic, fn: (e: JobEvent<T>) => void): Promise<UnlistenFn> {
  return listen<JobEvent<T>>(`romm://${topic}`, (e) => fn(e.payload));
}

export function onProgress(fn: (e: ProgressEvent) => void): Promise<UnlistenFn> {
  return listen<ProgressEvent>("romm://progress", (e) => fn(e.payload));
}

// Cover art is fetched through the backend (it needs the auth token) and cached on disk there;
// this just de-duplicates concurrent requests for the same image.
const covers = new Map<string, Promise<string | null>>();
export function loadCover(romId: number, path: string): Promise<string | null> {
  const key = `${romId}:${path}`;
  let p = covers.get(key);
  if (!p) {
    p = api.cover(romId, path).catch(() => null);
    covers.set(key, p);
  }
  return p;
}
