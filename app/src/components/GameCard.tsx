import { useEffect, useRef, useState } from "react";
import { loadCover } from "../api";
import { formatBytes } from "../format";
import { useStore } from "../store";
import type { Game } from "../types";
import { copyIds, isGameInstalled, versionCountText } from "../versions";

function hue(s: string): number {
  let h = 0;
  for (const ch of s) h = (h * 31 + ch.charCodeAt(0)) % 360;
  return h;
}

/** Cover art loads only once the card scrolls near the viewport. */
export function Cover({ game, className = "" }: { game: Game; className?: string }) {
  const holder = useRef<HTMLDivElement | null>(null);
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    const el = holder.current;
    if (!el || !game.coverPath) return;
    let alive = true;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          io.disconnect();
          loadCover(game.id, game.coverPath!).then((u) => alive && setSrc(u));
        }
      },
      { rootMargin: "300px" },
    );
    io.observe(el);
    return () => {
      alive = false;
      io.disconnect();
    };
  }, [game.id, game.coverPath]);
  const h = hue(game.name);
  return (
    <div ref={holder} className={`cover ${className}`} style={src ? undefined : { background: `linear-gradient(135deg, hsl(${h} 45% 28%), hsl(${(h + 40) % 360} 50% 18%))` }}>
      {src ? <img src={src} alt="" draggable={false} /> : <span className="cover-initials" aria-hidden>{game.name.replace(/[^\p{L}\p{N} ]/gu, "").split(" ").filter(Boolean).slice(0, 2).map((w) => w[0]).join("").toUpperCase()}</span>}
    </div>
  );
}

export function GameCard({ game, platformName, onOpen }: { game: Game; platformName?: string; onOpen: (g: Game) => void }) {
  const { jobs, installed } = useStore();
  // A game is installed when any of its versions is, and busy when any of them is.
  const job = copyIds(game).map((id) => jobs[id]).find(Boolean);
  const isInstalled = isGameInstalled(game, installed);
  const versions = versionCountText(game);
  const pct = job?.kind === "install" && job.total ? Math.min(100, Math.round(((job.done ?? 0) / job.total) * 100)) : null;
  const label = job
    ? job.kind === "install" ? (pct != null ? `${pct}%` : "Downloading…")
      : job.kind === "play" ? (job.phase === "running" ? "Playing" : job.phase === "saving" ? "Saving…" : "Syncing…")
      : "Syncing…"
    : null;
  return (
    <button className="game-card" onClick={() => onOpen(game)} data-testid="game-card" data-game-id={game.id} data-installed={isInstalled} aria-label={`${game.name}${versions ? `, ${versions}` : ""}${isInstalled ? ", installed" : ""}`}>
      <Cover game={game} />
      {versions && <span className="badge-versions" title={`${versions} of this game: open it to choose which to install`} data-testid="versions-badge" aria-hidden>{versions}</span>}
      {isInstalled && <span className="badge-installed" title="Installed" aria-hidden>✓</span>}
      {job && (
        <div className="cover-overlay">
          <span>{label}</span>
          {pct != null && <div className="progress"><div style={{ width: `${pct}%` }} /></div>}
        </div>
      )}
      <div className="game-meta">
        <div className="game-title">{game.name}</div>
        <div className="game-sub">{platformName ?? game.platformSlug}{game.versionLabel ? ` · ${game.versionLabel}` : ""}{game.sizeBytes ? ` · ${formatBytes(game.sizeBytes)}` : ""}</div>
      </div>
    </button>
  );
}
