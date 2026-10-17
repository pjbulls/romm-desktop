import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { onJob, onProgress } from "./api";
import { formatDuration, hasProblems, summarizeSync } from "./format";
import type { Game, JobEvent, PlaySummary, SyncAllResult, SyncSummary } from "./types";

export type JobKind = "install" | "play" | "sync";

export interface Job {
  kind: JobKind;
  phase: string;
  done?: number;
  total?: number | null;
}

export interface Toast {
  id: number;
  tone: "info" | "success" | "error";
  text: string;
  detail?: string[];
}

interface Ctx {
  jobs: Record<number, Job>;
  syncingAll: boolean;
  toasts: Toast[];
  /** Overrides the server-provided `installed` flag after an install/uninstall in this session. */
  installed: Record<number, boolean>;
  /** Bumped when a game's saves/states may have changed, so open panels refetch. */
  revision: Record<number, number>;
  remember: (games: Game[]) => void;
  setInstalled: (id: number, v: boolean) => void;
  push: (tone: Toast["tone"], text: string, detail?: string[]) => void;
  dismiss: (id: number) => void;
  bump: (id: number) => void;
}

const StoreContext = createContext<Ctx | null>(null);

export function useStore(): Ctx {
  const c = useContext(StoreContext);
  if (!c) throw new Error("useStore outside StoreProvider");
  return c;
}

export function StoreProvider({ children }: { children: ReactNode }) {
  const [jobs, setJobs] = useState<Record<number, Job>>({});
  const [syncingAll, setSyncingAll] = useState(false);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [installed, setInstalledMap] = useState<Record<number, boolean>>({});
  const [revision, setRevision] = useState<Record<number, number>>({});
  const names = useRef(new Map<number, string>());
  const nextId = useRef(1);

  const dismiss = useCallback((id: number) => setToasts((t) => t.filter((x) => x.id !== id)), []);
  const push = useCallback(
    (tone: Toast["tone"], text: string, detail?: string[]) => {
      const id = nextId.current++;
      setToasts((t) => [...t.slice(-4), { id, tone, text, detail }]);
      if (tone !== "error") setTimeout(() => dismiss(id), 5000);
    },
    [dismiss],
  );
  const bump = useCallback((id: number) => setRevision((r) => ({ ...r, [id]: (r[id] ?? 0) + 1 })), []);
  const remember = useCallback((games: Game[]) => games.forEach((g) => names.current.set(g.id, g.name)), []);
  const setInstalled = useCallback((id: number, v: boolean) => setInstalledMap((m) => ({ ...m, [id]: v })), []);

  useEffect(() => {
    const name = (id: number) => names.current.get(id) ?? `Game ${id}`;
    const setJob = (id: number, job: Job | null) =>
      setJobs((j) => {
        const next = { ...j };
        if (job) next[id] = job;
        else delete next[id];
        return next;
      });

    const un: Promise<() => void>[] = [
      onProgress((p) => setJobs((j) => (j[p.romId] ? { ...j, [p.romId]: { ...j[p.romId], done: p.done, total: p.total } } : j))),

      onJob("install", (e: JobEvent) => {
        if (e.phase === "started") return setJob(e.romId, { kind: "install", phase: "downloading", done: 0, total: null });
        setJob(e.romId, null);
        if (e.phase === "finished") {
          setInstalled(e.romId, true);
          push("success", `${name(e.romId)} installed`);
        } else push("error", `Couldn't install ${name(e.romId)}`, e.error ? [e.error] : undefined);
        bump(e.romId);
      }),

      onJob<PlaySummary>("play", (e) => {
        if (e.phase === "failed") {
          setJob(e.romId, null);
          push("error", `Couldn't launch ${name(e.romId)}`, e.error ? [e.error] : undefined);
          return bump(e.romId);
        }
        if (e.phase !== "finished") return setJob(e.romId, { kind: "play", phase: e.phase });
        setJob(e.romId, null);
        const r = e.result;
        const lines = r ? [...summarizeSync(r.sync), ...r.notes] : [];
        const bad = r ? hasProblems(r.sync) || r.notes.length > 0 : false;
        push(bad ? "error" : "success", `${name(e.romId)}: played ${r ? formatDuration(r.playedSecs) : ""}`.trim(), lines.length ? lines : ["Saves and states are up to date"]);
        bump(e.romId);
      }),

      onJob<SyncSummary>("sync", (e) => {
        if (e.phase === "failed") {
          setJob(e.romId, null);
          push("error", `Couldn't sync ${name(e.romId)}`, e.error ? [e.error] : undefined);
          return bump(e.romId);
        }
        if (e.phase !== "finished") return setJob(e.romId, { kind: "sync", phase: "syncing" });
        setJob(e.romId, null);
        const lines = e.result ? summarizeSync(e.result) : [];
        push(e.result && hasProblems(e.result) ? "error" : "success", `${name(e.romId)} synced`, lines.length ? lines : ["Everything was already up to date"]);
        bump(e.romId);
      }),

      onJob<SyncAllResult>("sync-all", (e) => {
        if (e.phase === "syncing") return setSyncingAll(true);
        setSyncingAll(false);
        if (e.phase === "failed") return push("error", "Sync failed", e.error ? [e.error] : undefined);
        const r = e.result;
        const lines = r ? [...summarizeSync(r.summary), ...r.skipped.map((s) => `Skipped ${s}`)] : [];
        push(r && (hasProblems(r.summary) || r.skipped.length) ? "error" : "success", `Synced ${r?.games ?? 0} game${r?.games === 1 ? "" : "s"}`, lines.length ? lines : ["Everything was already up to date"]);
        setRevision((m) => Object.fromEntries([...Object.keys(m).map((k) => [k, (m[+k] ?? 0) + 1])]));
      }),
    ];
    return () => {
      un.forEach((p) => p.then((f) => f()).catch(() => {}));
    };
  }, [push, bump, setInstalled]);

  const value = useMemo<Ctx>(
    () => ({ jobs, syncingAll, toasts, installed, revision, remember, setInstalled, push, dismiss, bump }),
    [jobs, syncingAll, toasts, installed, revision, remember, setInstalled, push, dismiss, bump],
  );
  return <StoreContext.Provider value={value}>{children}</StoreContext.Provider>;
}
