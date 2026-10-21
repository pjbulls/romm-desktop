import { useCallback, useEffect, useRef, useState } from "react";
import { api, errMsg, loadCover } from "../api";
import {
  ACH_FILTERS, ACH_KIND, ACH_PREVIEW, AWARD, achievementFacets, achievementPercent, fullDate, isMissable, isNarrowed, previewAchievements, queryAchievements,
  timeAgo, type AchFilter, type AchQuery,
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
  /** Words to search titles and descriptions for. */
  query?: string;
  /** Show only the achievements RetroAchievements marks as missable. */
  missableOnly?: boolean;
  onQuery?: (text: string) => void;
  onMissableOnly?: (on: boolean) => void;
  /** Back to everything: no search, no missable filter, no earned/locked choice. */
  onClear?: () => void;
}

const nothing = () => {};

/** Everything the section shows, as a pure function of its props (so it can be rendered in tests). */
export function AchievementsPanel({
  romId, view, error, refreshing, filter, expanded, onFilter, onToggleExpanded, onRefresh, onRetry,
  query = "", missableOnly = false, onQuery = nothing, onMissableOnly = nothing, onClear = nothing,
}: PanelProps) {
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
        <p className="muted">No RetroAchievements found for this game.</p>
        {stale}
      </section>
    );
  }

  const q: AchQuery = { status: filter, missable: missableOnly, text: query };
  const facets = achievementFacets(view.achievements, q);
  const filtered = queryAchievements(view.achievements, q);
  const narrowed = isNarrowed(q);
  const missableTotal = view.achievements.filter(isMissable).length;
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
          Add your RetroAchievements username to your RomM profile first.
        </div>
      )}
      {ready && view.progress_missing && (
        <div className="banner banner-warn" data-testid="ach-no-progress">
          RomM hasn't fetched your RetroAchievements progress yet.
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

      {view.achievements.length > 0 && (
        <div className="ach-controls">
          <input
            className="search ach-search" type="search" placeholder="Search achievements" aria-label="Search achievements" data-testid="ach-search"
            value={query} onChange={(e) => onQuery(e.target.value)}
            // Escape clears a search before it closes the panel the search is in.
            onKeyDown={(e) => { if (e.key === "Escape" && query) { e.stopPropagation(); onQuery(""); } }}
          />
          <div className="ach-filters" role="group" aria-label="Filter achievements">
            {ready && ACH_FILTERS.map((f) => (
              <button key={f.value} className="btn btn-ghost small" aria-pressed={filter === f.value} onClick={() => onFilter(f.value)} data-testid={`ach-filter-${f.value}`}>
                {f.label} <span className="muted">{facets.status[f.value]}</span>
              </button>
            ))}
            <button
              className="btn btn-ghost small" aria-pressed={missableOnly} disabled={missableTotal === 0} onClick={() => onMissableOnly(!missableOnly)} data-testid="ach-filter-missable"
              title={missableTotal === 0 ? "RetroAchievements marks none of this game's achievements as missable" : "Only achievements that can be missed for good"}
            >
              Missable <span className="muted">{facets.missable}</span>
            </button>
          </div>
        </div>
      )}
      {narrowed && filtered.length > 0 && <p className="muted small" data-testid="ach-showing">Showing {filtered.length} of {view.achievements.length}</p>}

      {filtered.length === 0 ? (
        narrowed ? (
          <p className="muted" data-testid="ach-none">
            No achievements match. <button className="link-btn" onClick={onClear} data-testid="ach-clear">Clear</button>
          </p>
        ) : (
          <p className="muted">{filter === "earned" ? "Nothing earned yet." : "Everything is earned."}</p>
        )
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
  const { push, achRevision } = useStore();
  const [view, setView] = useState<AchievementsView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [filter, setFilter] = useState<AchFilter>("all");
  const [query, setQuery] = useState("");
  const [missableOnly, setMissableOnly] = useState(false);
  const [expanded, setExpanded] = useState(false);

  // Reads can overlap (the backend announces a refresh while an earlier read may still be out), so an
  // answer older than one already on screen is dropped: tickets are handed out in the order reads start.
  const issued = useRef(0);
  const applied = useRef(0);
  const show = useCallback((ticket: number, v: AchievementsView) => {
    if (ticket <= applied.current) return;
    applied.current = ticket;
    setView(v);
  }, []);

  const load = useCallback(() => {
    let alive = true;
    const ticket = ++issued.current;
    setError(null);
    api.achievements(game.id).then((v) => alive && show(ticket, v)).catch((e) => alive && setError(errMsg(e)));
    return () => { alive = false; };
  }, [game.id, show]);

  useEffect(() => {
    setView(null);
    setFilter("all");
    setQuery("");
    setMissableOnly(false);
    setExpanded(false);
    return load();
  }, [load]);

  // The backend re-pulled this user's progress from retroachievements.org after a game closed. Swap in
  // the new answer quietly: what is on screen stays until it arrives, and if the reload fails it is
  // still the best we have.
  const seen = useRef(achRevision);
  useEffect(() => {
    if (seen.current === achRevision) return; // also what a change of game looks like: `load` handles that
    seen.current = achRevision;
    let alive = true;
    const ticket = ++issued.current;
    api.achievements(game.id).then((v) => { if (alive) { show(ticket, v); setError(null); } }).catch(() => {});
    return () => { alive = false; };
  }, [achRevision, game.id, show]);

  const refresh = async () => {
    setRefreshing(true);
    try {
      const fresh = await api.refreshAchievements(game.id);
      show(++issued.current, fresh);
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
      query={query} missableOnly={missableOnly}
      onQuery={(t) => { setQuery(t); setExpanded(false); }}
      onMissableOnly={(on) => { setMissableOnly(on); setExpanded(false); }}
      onClear={() => { setQuery(""); setMissableOnly(false); setFilter("all"); setExpanded(false); }}
      onToggleExpanded={() => setExpanded((x) => !x)}
      onRefresh={refresh}
      onRetry={() => { setView(null); load(); }}
    />
  );
}
