import { useCallback, useEffect, useRef, useState } from "react";
import { api, errMsg } from "../api";
import { useStore } from "../store";
import type { Game, Platform } from "../types";
import { GameCard } from "./GameCard";
import { GameDrawer } from "./GameDrawer";

const PAGE = 48;

type Mode = { kind: "library"; platform: Platform | null } | { kind: "installed" };

function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setV(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return v;
}

export function Library({ mode, platforms, onGoEmulators }: { mode: Mode; platforms: Platform[]; onGoEmulators: () => void }) {
  const { remember, installed } = useStore();
  const [search, setSearch] = useState("");
  const q = useDebounced(search.trim(), 250);
  const [items, setItems] = useState<Game[]>([]);
  const [total, setTotal] = useState<number | null>(null);
  // Starts true: the first fetch begins right after mount, and rendering "nothing" for that frame
  // flashed a "No games" message and left a gap in which controller focus recovery could misfire.
  const [loading, setLoading] = useState(true);
  const [done, setDone] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<Game | null>(null);
  const opener = useRef<HTMLElement | null>(null);
  const openGame = useCallback((g: Game) => {
    opener.current = document.activeElement as HTMLElement | null;
    setOpen(g);
  }, []);
  const closeGame = useCallback(() => {
    setOpen(null);
    // Put focus back on the card (matters for keyboard and controller users).
    requestAnimationFrame(() => opener.current?.isConnected && opener.current.focus());
  }, []);

  const gen = useRef(0); // discards results from superseded requests
  const busy = useRef(false);
  const count = useRef(0);
  const sentinel = useRef<HTMLDivElement | null>(null);
  const searchBox = useRef<HTMLInputElement | null>(null);
  const platformId = mode.kind === "library" ? mode.platform?.id : undefined;
  const bySlug = useCallback((slug: string) => platforms.find((p) => p.slug === slug)?.name, [platforms]);

  const fetchPage = useCallback(
    async (offset: number, g: number) => {
      busy.current = true;
      setLoading(true);
      try {
        const page = await api.games({ platformId, search: q || undefined, offset, limit: PAGE, installedOnly: mode.kind === "installed" });
        if (g !== gen.current) return;
        remember(page.items);
        count.current = offset + page.items.length;
        setItems((prev) => (offset === 0 ? page.items : [...prev, ...page.items]));
        setTotal(page.total);
        setDone(mode.kind === "installed" || page.items.length < PAGE);
      } catch (e) {
        if (g === gen.current) setError(errMsg(e));
      } finally {
        if (g === gen.current) {
          busy.current = false;
          setLoading(false);
        }
      }
    },
    [platformId, q, mode.kind, remember],
  );

  useEffect(() => {
    gen.current += 1;
    count.current = 0;
    setItems([]);
    setTotal(null);
    setDone(false);
    setError(null);
    fetchPage(0, gen.current);
  }, [fetchPage]);

  useEffect(() => {
    const el = sentinel.current;
    if (!el) return;
    const io = new IntersectionObserver((e) => {
      if (e.some((x) => x.isIntersecting) && !busy.current && !done && count.current > 0) fetchPage(count.current, gen.current);
    }, { rootMargin: "600px" });
    io.observe(el);
    return () => io.disconnect();
  }, [fetchPage, done]);

  // "/" jumps to search, like most library apps.
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key === "/" && document.activeElement?.tagName !== "INPUT") {
        e.preventDefault();
        searchBox.current?.focus();
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, []);

  const title = mode.kind === "installed" ? "Installed" : mode.platform?.name ?? "All games";
  const shown = mode.kind === "installed" ? items.filter((g) => installed[g.id] !== false) : items;

  return (
    <>
      <header className="topbar">
        <div>
          <h1>{title}</h1>
          <div className="muted small">{total != null ? `${total} game${total === 1 ? "" : "s"}` : "\u00a0"}</div>
        </div>
        <input ref={searchBox} className="search" type="search" placeholder="Search games  ( / )" value={search} onChange={(e) => setSearch(e.target.value)} aria-label="Search games" data-testid="search" />
      </header>

      {mode.kind === "library" && mode.platform && !mode.platform.emulatorInstalled && (
        <div className="banner banner-warn inline">
          {mode.platform.emulatorName ? `${mode.platform.emulatorName} wasn't found on this PC, so ${mode.platform.name} games can be downloaded but not launched.` : `No emulator is set up for ${mode.platform.name}.`}{" "}
          <button className="link-btn" onClick={onGoEmulators}>Set up emulators</button>
        </div>
      )}
      {error && <div className="banner banner-error inline" role="alert">{error}</div>}

      <div className="grid" data-testid="grid">
        {shown.map((g) => <GameCard key={g.id} game={g} platformName={bySlug(g.platformSlug)} onOpen={openGame} />)}
      </div>
      {!loading && !error && shown.length === 0 && (
        <div className="empty">{mode.kind === "installed" ? "Nothing installed yet. Pick a game and choose Install." : q ? `No games match “${q}”.` : "No games here."}</div>
      )}
      {loading && <div className="loading-row"><span className="spinner sm" /> Loading…</div>}
      <div ref={sentinel} style={{ height: 1 }} />

      {open && <GameDrawer game={open} platform={platforms.find((p) => p.slug === open.platformSlug)} onClose={closeGame} onGoEmulators={() => { setOpen(null); onGoEmulators(); }} />}
    </>
  );
}
