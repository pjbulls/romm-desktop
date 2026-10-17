import { useCallback, useEffect, useRef, useState } from "react";
import { api, errMsg, loadCover } from "../api";
import {
  ACH_FILTERS, ACH_KIND, ACH_PREVIEW, AWARD, achievementCounts, achievementPercent, filterAchievements, fullDate, previewAchievements, timeAgo,
  type AchFilter,
} from "../format";
import { useStore } from "../store";
import type { Achievement, AchievementsView, Game } from "../types";

/** A badge image. Loads only when scrolled near the viewport: a full set can be hundreds of images. */
function Badge({ romId, path, earned }: { romId: number; path: string | null; earned: boolean }) {
  const holder = useRef<HTMLDivElement | null>(null);
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    setSrc(null);
    const el = holder.current;
    if (!el || !path) return;
    let alive = true;
    const load = () => void loadCover(romId, path).then((u) => alive && setSrc(u));
    if (typeof IntersectionObserver === "undefined") {
      load();
      return () => { alive = false; };
    }
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          io.disconnect();
          load();
        }
      },
      { rootMargin: "200px" },
    );
    io.observe(el);
    return () => { alive = false; io.disconnect(); };
  }, [romId, path]);
  return (
    <div ref={holder} className={`ach-badge${earned ? "" : " locked"}`} aria-hidden>
      {src && <img src={src} alt="" draggable={false} />}
    </div>
  );
}

function AchievementRow({ romId, a }: { romId: number; a: Achievement }) {
  const kind = ACH_KIND[a.kind];
  return (
    <li className={`ach ${a.earned ? "earned" : "locked"}`} data-testid="achievement">
      <Badge romId={romId} path={a.badge} earned={a.earned} />
      <div className="ach-text">
        <div className="ach-title">{a.title || "Untitled achievement"}</div>
        {a.description && <div className="muted small">{a.description}</div>}
        <div className="ach-tags">
          {kind && <span className={`chip chip-${kind.tone}`}>{kind.label}</span>}
          {a.hardcore && <span className="chip chip-ok">Hardcore</span>}
        </div>
      </div>
      <div className="ach-side">
        <span className="chip chip-muted">{a.points} pts</span>
        {a.earned && a.earned_at && <span className="muted small" title={fullDate(a.earned_at)}>{timeAgo(a.earned_at)}</span>}
      </div>
    </li>
  );
}

interface PanelProps {
  romId: number;
  view: AchievementsView | null;
  error: string | null;
  refreshing: boolean;
  filter: AchFilter;
  expanded: boolean;
  onFilter: (f: AchFilter) => void;
  onToggleExpanded: () => void;
  onRefresh: () => void;
  onRetry: () => void;
}

/** Everything the section shows, as a pure function of its props (so it can be rendered in tests). */
export function AchievementsPanel({ romId, view, error, refreshing, filter, expanded, onFilter, onToggleExpanded, onRefresh, onRetry }: PanelProps) {
  const header = (
    <div className="row-flex ach-head">
      <h3>Achievements</h3>
      {view?.state === "ready" && (
        <button className="btn btn-ghost small" disabled={refreshing} onClick={onRefresh} data-testid="ach-refresh"
          title="Ask RomM to re-fetch your progress from retroachievements.org">
          {refreshing ? "Refreshing…" : "Refresh"}
        </button>
      )}
    </div>
  );

  if (!view) {
    return (
      <section data-testid="achievements">
        {header}
        {error ? (
          <div className="banner banner-warn" role="alert">
            Couldn't load achievements ({error}). <button className="link-btn" onClick={onRetry}>Try again</button>
          </div>
        ) : (
          <p className="muted">Loading…</p>
        )}
      </section>
    );
  }

  const stale = view.stale && (
    <p className="muted small" data-testid="ach-stale">Saved copy from {timeAgo(view.fetched_at)}. Couldn't reach the server.</p>
  );

  if (view.state === "no_set") {
    return (
      <section data-testid="achievements">
        {header}
        <p className="muted">No RetroAchievements for this game. RetroAchievements may not cover it, or RomM hasn't matched it.</p>
        {stale}
      </section>
    );
  }

  const counts = achievementCounts(view.achievements);
  const filtered = filterAchievements(view.achievements, filter);
  const shown = previewAchievements(filtered, expanded);
  const pct = achievementPercent(view);
  const award = view.award ? AWARD[view.award] : undefined;
  const ready = view.state === "ready";

  return (
    <section data-testid="achievements">
      {header}
      {stale}

      {view.state === "not_linked" && (
        <div className="banner banner-warn" data-testid="ach-not-linked">
          Add your RetroAchievements username to your profile in RomM to see which of these you've earned.
        </div>
      )}
      {ready && view.progress_missing && (
        <div className="banner banner-warn" data-testid="ach-no-progress">
          RomM hasn't fetched your RetroAchievements progress yet, so nothing is marked as earned. Use Refresh, or run the sync in RomM.
        </div>
      )}

      {ready && (
        <div className="ach-summary" data-testid="ach-summary">
          <div className="row-flex">
            <strong>{view.earned} / {view.total} earned</strong>
            <span className="muted small">{view.points_earned} / {view.points_total} pts</span>
          </div>
          <div className="progress wide" role="progressbar" aria-label="Achievements earned" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
            <div style={{ width: `${pct}%` }} />
          </div>
          <div className="ach-meta muted small">
            {award && <span className={`chip chip-${award.tone}`}>{award.label}</span>}
            {view.last_earned && <span title={fullDate(view.last_earned)}>Last earned {timeAgo(view.last_earned)}</span>}
            {view.ra_username && <span>as {view.ra_username}</span>}
          </div>
        </div>
      )}

      {ready && (
        <div className="ach-filters" role="group" aria-label="Filter achievements">
          {ACH_FILTERS.map((f) => (
            <button key={f.value} className="btn btn-ghost small" aria-pressed={filter === f.value} onClick={() => onFilter(f.value)} data-testid={`ach-filter-${f.value}`}>
              {f.label} <span className="muted">{counts[f.value]}</span>
            </button>
          ))}
        </div>
      )}

      {filtered.length === 0 ? (
        <p className="muted">{filter === "earned" ? "Nothing earned yet." : "Everything is earned."}</p>
      ) : (
        <ul className="rows ach-list" data-testid="ach-list">
          {shown.map((a, i) => <AchievementRow key={`${a.id}-${i}`} romId={romId} a={a} />)}
        </ul>
      )}
      {filtered.length > ACH_PREVIEW && (
        <button className="btn btn-ghost small ach-more" onClick={onToggleExpanded} data-testid="ach-more">
          {expanded ? "Show fewer" : `Show all ${filtered.length}`}
        </button>
      )}
    </section>
  );
}

export function Achievements({ game }: { game: Game }) {
  const { push } = useStore();
  const [view, setView] = useState<AchievementsView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [filter, setFilter] = useState<AchFilter>("all");
  const [expanded, setExpanded] = useState(false);

  const load = useCallback(() => {
    let alive = true;
    setError(null);
    api.achievements(game.id).then((v) => alive && setView(v)).catch((e) => alive && setError(errMsg(e)));
    return () => { alive = false; };
  }, [game.id]);

  useEffect(() => {
    setView(null);
    setFilter("all");
    setExpanded(false);
    return load();
  }, [load]);

  const refresh = async () => {
    setRefreshing(true);
    try {
      setView(await api.refreshAchievements(game.id));
      setError(null);
      push("success", "RetroAchievements progress updated");
    } catch (e) {
      push("error", "Couldn't refresh RetroAchievements", [errMsg(e)]);
    } finally {
      setRefreshing(false);
    }
  };

  return (
    <AchievementsPanel
      romId={game.id} view={view} error={error} refreshing={refreshing} filter={filter} expanded={expanded}
      onFilter={(f) => { setFilter(f); setExpanded(false); }}
      onToggleExpanded={() => setExpanded((x) => !x)}
      onRefresh={refresh}
      onRetry={() => { setView(null); load(); }}
    />
  );
}
