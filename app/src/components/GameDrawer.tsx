import { useCallback, useEffect, useRef, useState } from "react";
import { api, errMsg } from "../api";
import { formatBytes, SAVE_STATUS, STATE_STATUS, timeAgo } from "../format";
import { useStore } from "../store";
import type { Details, Game, Platform, VersionSize } from "../types";
import { Achievements } from "./Achievements";
import { Cover } from "./GameCard";
import { copyInfo, initialCopy, isCopyInstalled, otherCopyIds, versionOptionText } from "../versions";

interface Props {
  game: Game;
  platform?: Platform;
  onClose: () => void;
  onGoEmulators: () => void;
}

export function GameDrawer({ game, platform, onClose, onGoEmulators }: Props) {
  const { jobs, installed, revision, push, setInstalled } = useStore();
  // A game with several versions (regions, revisions) opens on the one that's installed, else on the
  // primary the server chose. Each version is its own ROM, so everything below runs on `id`, the
  // selected one: install, play, sync, saves, states and achievements are all per version.
  const [id, setId] = useState(() => initialCopy(game, installed));
  const job = jobs[id];
  const rev = revision[id] ?? 0;
  const [details, setDetails] = useState<Details | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const panel = useRef<HTMLElement | null>(null);
  const isInstalled = isCopyInstalled(game, id, installed);
  // The list knows the primary version's size; the others' are fetched when the panel opens.
  const [sizes, setSizes] = useState<Record<number, VersionSize>>({});
  const info = copyInfo(game, id, sizes);

  useEffect(() => {
    const others = otherCopyIds(game);
    if (others.length === 0) return;
    let alive = true;
    api.versionSizes(others)
      .then((got) => alive && setSizes(Object.fromEntries(got.map((v) => [v.id, v]))))
      .catch(() => {}); // a size is a nicety: without it the panel is just as before
    return () => { alive = false; };
  }, [game]);

  const refresh = useCallback(() => {
    api.gameDetails(id, game.platformSlug).then((d) => { setDetails(d); setLoadErr(null); }).catch((e) => setLoadErr(errMsg(e)));
  }, [id, game.platformSlug]);

  const pickVersion = (next: number) => {
    setId(next);
    setDetails(null);
    setLoadErr(null);
    setConfirmRemove(false);
  };

  // Refetch when the game is installed/removed or a job on it finishes.
  useEffect(refresh, [refresh, rev, isInstalled]);

  // Focus moves into the panel when it opens, then to Play/Install once that button is usable
  // (also after an install finishes and the Install button is replaced).
  useEffect(() => { panel.current?.focus(); }, []);
  useEffect(() => {
    const root = panel.current;
    if (!root || !details) return;
    const ae = document.activeElement;
    if (!ae || ae === document.body || ae === root || ae.classList.contains("drawer-close") || !root.contains(ae)) {
      root.querySelector<HTMLElement>("[data-testid=play]:not(:disabled), [data-testid=install]:not(:disabled)")?.focus();
    }
  }, [details]);

  useEffect(() => {
    const h = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [onClose]);

  const run = (label: string, fn: () => Promise<void>) => async () => {
    try {
      await fn();
    } catch (e) {
      push("error", label, [errMsg(e)]);
    }
  };

  const emu = details?.emulator ?? null;
  const canPlay = isInstalled && !!emu?.installed && !job;
  const pct = job?.kind === "install" && job.total ? Math.min(100, Math.round(((job.done ?? 0) / job.total) * 100)) : null;

  return (
    <div className="drawer-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <aside className="drawer" role="dialog" aria-modal="true" aria-label={game.name} data-testid="drawer" tabIndex={-1} ref={panel}>
        <button className="icon-btn drawer-close" onClick={onClose} aria-label="Close">×</button>
        <div className="drawer-head">
          <Cover game={game} className="cover-lg" />
          <div className="drawer-title">
            <h2>{game.name}</h2>
            <div className="muted">{platform?.name ?? game.platformSlug}{info?.sizeBytes ? ` · ${formatBytes(info.sizeBytes)}` : ""}{info?.multiFile ? " · multi-file" : ""}</div>
            {emu ? (
              emu.installed
                ? <div className="emu-line ok"><span className="dot dot-ok" /> Plays in {emu.name}</div>
                : <div className="emu-line warn"><span className="dot dot-warn" /> {emu.name} not found. <button className="link-btn" onClick={onGoEmulators}>Set it up</button></div>
            ) : details && <div className="emu-line warn"><span className="dot dot-warn" /> No emulator configured for this platform. <button className="link-btn" onClick={onGoEmulators}>Choose one</button></div>}
          </div>
        </div>

        {game.versions.length > 1 && (
          <div className="version-row" data-testid="versions">
            <label htmlFor="version-select" className="muted small">Version</label>
            <select id="version-select" className="choice-select" data-testid="version-select" value={id} onChange={(e) => pickVersion(Number(e.target.value))}>
              {game.versions.map((v) => <option key={v.id} value={v.id}>{versionOptionText(v, isCopyInstalled(game, v.id, installed))}</option>)}
            </select>
            <div className="muted small">{game.versions.length} versions of this game.</div>
          </div>
        )}

        <div className="actions">
          {!isInstalled ? (
            <button className="btn btn-primary" disabled={!!job} onClick={run("Couldn't start the download", () => api.installGame(id))} data-testid="install">
              {job?.kind === "install" ? (pct != null ? `Downloading… ${pct}%` : "Downloading…") : "Install"}
            </button>
          ) : (
            <>
              <button className="btn btn-primary" disabled={!canPlay} onClick={run("Couldn't launch the game", () => api.playGame(id))} data-testid="play"
                title={!emu?.installed ? "Set up the emulator first" : undefined}>
                {job?.kind === "play" ? ({ syncing: "Syncing saves…", running: "Playing…", saving: "Saving…" } as Record<string, string>)[job.phase] ?? "Working…" : "▶ Play"}
              </button>
              <button className="btn" disabled={!!job || !emu?.installed} onClick={run("Couldn't start the sync", () => api.syncGame(id))} data-testid="sync">
                {job?.kind === "sync" ? "Syncing…" : "Sync now"}
              </button>
              {!confirmRemove ? (
                <button className="btn btn-ghost" disabled={!!job} onClick={() => setConfirmRemove(true)}>Uninstall</button>
              ) : (
                <span className="confirm">
                  Remove the downloaded files? Your saves stay.
                  <button className="btn btn-danger" onClick={run("Couldn't uninstall", async () => { await api.uninstallGame(id); setInstalled(id, false); setConfirmRemove(false); refresh(); })}>Remove</button>
                  <button className="btn btn-ghost" onClick={() => setConfirmRemove(false)}>Cancel</button>
                </span>
              )}
            </>
          )}
        </div>
        {job?.kind === "install" && <div className="progress wide"><div style={{ width: `${pct ?? 5}%` }} /></div>}
        {job?.kind === "install" && job.total ? <div className="muted small">{formatBytes(job.done ?? 0)} of {formatBytes(job.total)}</div> : null}
        {emu && !emu.installed && emu.problem && <div className="banner banner-warn">{emu.problem}</div>}
        {loadErr && <div className="banner banner-error" role="alert">{loadErr}</div>}

        {isInstalled && details && (
          <>
            <section>
              <h3>Saves</h3>
              {details.saves.length === 0 ? (
                <p className="muted">No save file yet.</p>
              ) : (
                <ul className="rows" data-testid="saves">
                  {details.saves.map((s) => (
                    <li key={s.name}>
                      <div className="row-main"><span className="row-name">{s.name}</span><span className="muted small">{formatBytes(s.size)}{s.detail ? ` · ${s.detail}` : ""} · {timeAgo(s.modified)}</span></div>
                      <span className={`chip chip-${SAVE_STATUS[s.status].tone}`}>{SAVE_STATUS[s.status].label}</span>
                    </li>
                  ))}
                </ul>
              )}
            </section>

            <section>
              <h3>Save states</h3>
              {!details.statesEnabled ? (
                <p className="muted">Save-state syncing is turned off.</p>
              ) : (
                <>
                  {details.statesError && <div className="banner banner-warn">Couldn't reach the server: ({details.statesError})</div>}
                  {details.states.length === 0 ? (
                    <p className="muted">No save states yet.</p>
                  ) : (
                    <ul className="rows" data-testid="states">
                      {details.states.map((s) => (
                        <li key={s.name}>
                          <div className="row-main">
                            <span className="row-name">{s.name}</span>
                            <span className="muted small">{formatBytes(s.local_size ?? s.server_size)} · {timeAgo(s.local_modified ?? s.server_updated_at)}</span>
                          </div>
                          <span className={`chip chip-${STATE_STATUS[s.status].tone}`}>{STATE_STATUS[s.status].label}</span>
                        </li>
                      ))}
                    </ul>
                  )}
                </>
              )}
            </section>
          </>
        )}

        <Achievements game={{ ...game, id }} />

        {isInstalled && details?.installPath && <div className="muted small path">Installed at {details.installPath}</div>}
      </aside>
    </div>
  );
}
