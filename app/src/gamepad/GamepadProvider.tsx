import { createContext, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { OnScreenKeyboard } from "../components/OnScreenKeyboard";
import { useStore } from "../store";
import { initialState, mergeSnapshots, step, type Action, type Snapshot } from "./input";
import { controllerKind, friendlyName, labelsFor, type Labels } from "./layouts";
import { setNativeValue } from "./osk";
import { pickNext, type Box, type Dir } from "./spatial";

const STORAGE_KEY = "romm.gamepad.enabled";
const readEnabled = () => {
  try { return localStorage.getItem(STORAGE_KEY) !== "off"; } catch { return true; }
};

interface PadCtx {
  enabled: boolean;
  setEnabled: (v: boolean) => void;
  controller: string | null;
  labels: Labels;
  active: boolean;
}
const Ctx = createContext<PadCtx | null>(null);
export function useGamepad(): PadCtx {
  const c = useContext(Ctx);
  if (!c) throw new Error("useGamepad outside GamepadProvider");
  return c;
}

// ------------------------------------------------------------------------------- DOM helpers

const FOCUSABLE = "a[href], button, input:not([type=hidden]), select, textarea, summary, [tabindex]";
const TEXT_TYPES = new Set(["text", "search", "password", "url", "email", "tel", ""]);
const boxOf = (el: Element): Box => {
  const r = el.getBoundingClientRect();
  return { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
};
const isTextInput = (el: Element | null): el is HTMLInputElement => el instanceof HTMLInputElement && TEXT_TYPES.has(el.type);
const zoneOf = (el: Element | null) => (!el ? "none" : el.closest("[role=dialog]") ? "dialog" : el.closest(".sidebar") ? "sidebar" : "main");

/** The topmost modal if there is one (drawer, keyboard), else the whole page. */
function scopeRoot(): HTMLElement {
  const d = document.querySelectorAll<HTMLElement>("[role=dialog]");
  return d.length ? d[d.length - 1] : document.body;
}

function visible(el: HTMLElement): boolean {
  const r = el.getBoundingClientRect();
  if (r.width < 2 || r.height < 2) return false;
  const cs = getComputedStyle(el);
  return cs.visibility !== "hidden" && cs.display !== "none";
}

function focusables(scope: HTMLElement): HTMLElement[] {
  return Array.from(scope.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) => !(el as HTMLButtonElement).disabled && el.getAttribute("tabindex") !== "-1" && !el.closest(".toasts") && visible(el),
  );
}

function focusEl(el: HTMLElement) {
  el.focus({ preventScroll: true });
  el.scrollIntoView({ block: "nearest", inline: "nearest" });
}

function scrollParent(el: Element | null): HTMLElement | null {
  for (let p = el?.parentElement ?? null; p; p = p.parentElement) {
    const oy = getComputedStyle(p).overflowY;
    if ((oy === "auto" || oy === "scroll") && p.scrollHeight > p.clientHeight) return p;
  }
  return null;
}

function defaultFocus(): HTMLElement | null {
  const scope = scopeRoot();
  if (scope !== document.body) {
    return scope.querySelector<HTMLElement>("[data-testid=play]:not(:disabled), [data-testid=install]:not(:disabled), [data-key='1']") ?? focusables(scope)[0] ?? null;
  }
  const main = document.querySelector<HTMLElement>(".main");
  const card = main?.querySelector<HTMLElement>(".game-card");
  if (card) return card;
  // Games are on their way (e.g. just switched platform): wait for them instead of parking focus on the search box.
  if (main?.querySelector(".loading-row")) return null;
  return (
    (main ? focusables(main)[0] : null) ??
    document.querySelector<HTMLElement>(".sidebar .nav.active") ??
    focusables(document.body)[0] ??
    null
  );
}

function stepSelect(el: HTMLSelectElement, delta: number, wrap = false) {
  const n = el.options.length;
  let i = el.selectedIndex + delta;
  i = wrap ? (i + n) % n : Math.max(0, Math.min(n - 1, i));
  if (i === el.selectedIndex) return;
  Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set?.call(el, el.options[i].value);
  el.dispatchEvent(new Event("change", { bubbles: true }));
}

function stepNumber(el: HTMLInputElement, delta: number) {
  const inc = Number(el.step) || 1;
  const min = el.min !== "" ? Number(el.min) : -Infinity;
  const max = el.max !== "" ? Number(el.max) : Infinity;
  const v = Math.max(min, Math.min(max, (Number(el.value) || 0) + delta * inc));
  setNativeValue(el, String(v));
}

function moveFocus(dir: Dir) {
  const ae = document.activeElement as HTMLElement | null;
  if (ae instanceof HTMLSelectElement && (dir === "left" || dir === "right")) return stepSelect(ae, dir === "right" ? 1 : -1);
  if (ae instanceof HTMLInputElement && ae.type === "number" && (dir === "left" || dir === "right")) return stepNumber(ae, dir === "right" ? 1 : -1);
  const scope = scopeRoot();
  if (!ae || ae === document.body || !scope.contains(ae)) {
    const t = defaultFocus();
    if (t) focusEl(t);
    return;
  }
  // Up/Down never leave the region you're in (menu, page or panel); Left/Right may cross between them.
  const vertical = dir === "up" || dir === "down";
  const zone = zoneOf(ae);
  const cands = focusables(scope)
    .filter((e) => e !== ae && (!vertical || zoneOf(e) === zone))
    .map((e) => ({ item: e, box: boxOf(e) }));
  const target = pickNext(boxOf(ae), cands, dir);
  if (target) return focusEl(target);
  // Nothing further that way: reveal more of the page (this is also what triggers infinite scroll).
  if (dir === "up" || dir === "down") {
    const sp = scrollParent(ae);
    sp?.scrollBy({ top: (dir === "down" ? 1 : -1) * sp.clientHeight * 0.6 });
  }
}

const waitFor = async (sel: string, ms: number): Promise<HTMLElement | null> => {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const el = document.querySelector<HTMLElement>(sel);
    if (el) return el;
    await new Promise((r) => setTimeout(r, 80));
  }
  return null;
};

function readPads(): { id: string; snap: Snapshot }[] {
  let list: (Gamepad | null)[] = [];
  try { list = Array.from(navigator.getGamepads?.() ?? []); } catch { return []; }
  return list
    .filter((p): p is Gamepad => !!p && p.connected)
    .map((p) => ({ id: p.id, snap: { buttons: p.buttons.map((b) => b.pressed || b.value > 0.5), axes: Array.from(p.axes) } }));
}

// ---------------------------------------------------------------------------------- hints

interface HintCtx {
  kind: "osk" | "dialog" | "card" | "text" | "generic";
  primary: "" | "Play" | "Install";
  toasts: boolean;
}

function hintContext(osk: boolean): HintCtx {
  const toasts = document.querySelectorAll(".toast").length > 0;
  if (osk) return { kind: "osk", primary: "", toasts };
  const dlg = document.querySelector<HTMLElement>("[role=dialog]");
  if (dlg) {
    const primary = dlg.querySelector("[data-testid=play]:not(:disabled)") ? "Play" : dlg.querySelector("[data-testid=install]:not(:disabled)") ? "Install" : "";
    return { kind: "dialog", primary, toasts };
  }
  const ae = document.activeElement;
  if (ae?.matches(".game-card")) return { kind: "card", primary: (ae as HTMLElement).dataset.installed === "true" ? "Play" : "", toasts };
  if (isTextInput(ae)) return { kind: "text", primary: "", toasts };
  return { kind: "generic", primary: "", toasts };
}

function Hints({ ctx, labels }: { ctx: HintCtx; labels: Labels }) {
  const items: [string, string][] = [];
  switch (ctx.kind) {
    case "osk":
      items.push([labels.accept, "Type"], [labels.x, "Delete"], [labels.y, "Shift"], [labels.back, "Close"]);
      break;
    case "dialog":
      items.push([labels.accept, "Select"], [labels.back, "Close"]);
      if (ctx.primary) items.push([labels.start, ctx.primary]);
      break;
    case "card":
      items.push([labels.accept, "Details"]);
      if (ctx.primary) items.push([labels.start, ctx.primary]);
      items.push([`${labels.lb}/${labels.rb}`, "Platform"], [`${labels.lt}/${labels.rt}`, "Page"], [labels.y, "Search"]);
      break;
    case "text":
      items.push([labels.accept, "Type"], [labels.back, "Back"]);
      break;
    default:
      items.push([labels.accept, "Select"], [labels.back, "Back"], [`${labels.lb}/${labels.rb}`, "Platform"]);
  }
  if (ctx.toasts && ctx.kind !== "osk") items.push([labels.x, "Dismiss"]);
  return (
    <div className="hints" aria-hidden data-testid="hints">
      {items.map(([k, t]) => (
        <span className="hint-item" key={k + t}><kbd>{k}</kbd>{t}</span>
      ))}
    </div>
  );
}

// -------------------------------------------------------------------------------- provider

export function GamepadProvider({ children }: { children: ReactNode }) {
  const store = useStore();
  const storeRef = useRef(store);
  storeRef.current = store;

  const [enabled, setEnabledState] = useState(readEnabled);
  const [active, setActive] = useState(false);
  const [controller, setController] = useState<string | null>(null);
  const [kind, setKind] = useState(controllerKind(""));
  const [osk, setOsk] = useState<HTMLInputElement | null>(null);
  const [hint, setHint] = useState<HintCtx>({ kind: "generic", primary: "", toasts: false });

  const activeRef = useRef(false);
  const oskRef = useRef<HTMLInputElement | null>(null);
  oskRef.current = osk;
  const lastZone = useRef("main");

  const setEnabled = (v: boolean) => {
    setEnabledState(v);
    try { localStorage.setItem(STORAGE_KEY, v ? "on" : "off"); } catch { /* storage unavailable */ }
  };

  const setActiveBoth = (v: boolean) => {
    if (activeRef.current === v) return;
    activeRef.current = v;
    document.documentElement.classList.toggle("gamepad-active", v);
    setActive(v);
  };

  // Mouse or keyboard use hands control back to normal pointer behaviour.
  useEffect(() => {
    const sleep = () => setActiveBoth(false);
    const onMove = (e: MouseEvent) => { if (Math.abs(e.movementX) + Math.abs(e.movementY) > 0) sleep(); };
    const onKey = (e: KeyboardEvent) => { if (!(e as KeyboardEvent & { __pad?: boolean }).__pad) sleep(); };
    const onFocus = (e: FocusEvent) => { lastZone.current = zoneOf(e.target as Element); };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mousedown", sleep);
    window.addEventListener("keydown", onKey, true);
    document.addEventListener("focusin", onFocus);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mousedown", sleep);
      window.removeEventListener("keydown", onKey, true);
      document.removeEventListener("focusin", onFocus);
    };
  }, []);

  // ---- actions
  const closeOsk = () => setOsk(null);

  const back = () => {
    if (oskRef.current) return closeOsk();
    if (document.querySelector("[role=dialog]")) {
      const ev = new KeyboardEvent("keydown", { key: "Escape", bubbles: true });
      (ev as KeyboardEvent & { __pad?: boolean }).__pad = true;
      window.dispatchEvent(ev);
      return;
    }
    if (zoneOf(document.activeElement) === "main") document.querySelector<HTMLElement>(".sidebar .nav.active")?.focus();
  };

  const accept = () => {
    const ae = document.activeElement as HTMLElement | null;
    if (isTextInput(ae)) return setOsk(ae);
    if (ae instanceof HTMLSelectElement) return stepSelect(ae, 1, true);
    if (ae && ae !== document.body) ae.click();
  };

  const cyclePlatform = (delta: number) => {
    const items = Array.from(document.querySelectorAll<HTMLElement>(".sidebar .nav-group:not(.bottom) .nav, .sidebar .platform-list .nav"));
    if (!items.length) return;
    const i = items.findIndex((e) => e.classList.contains("active"));
    const next = items[(i + delta + items.length) % items.length];
    next.click();
    next.scrollIntoView({ block: "nearest" });
  };

  const page = (dir: 1 | -1) => {
    const sp = scrollParent(document.activeElement) ?? document.querySelector<HTMLElement>(".main");
    if (!sp) return;
    const from = document.activeElement && document.activeElement !== document.body ? boxOf(document.activeElement) : null;
    sp.scrollBy({ top: dir * sp.clientHeight * 0.85 });
    setTimeout(() => {
      const r = sp.getBoundingClientRect();
      const cards = Array.from(sp.querySelectorAll<HTMLElement>(".game-card")).filter((c) => {
        const b = c.getBoundingClientRect();
        return b.top >= r.top - 4 && b.bottom <= r.bottom + 4;
      });
      if (!cards.length) return;
      const x = from ? (from.left + from.right) / 2 : r.left;
      const y = r.top + r.height / 2;
      cards.sort((a, b) => {
        const d = (e: HTMLElement) => { const q = e.getBoundingClientRect(); return Math.hypot((q.left + q.right) / 2 - x, (q.top + q.bottom) / 2 - y); };
        return d(a) - d(b);
      });
      focusEl(cards[0]);
    }, 60);
  };

  const start = async () => {
    const dlg = document.querySelector<HTMLElement>("[role=dialog]");
    if (dlg) {
      (dlg.querySelector<HTMLElement>("[data-testid=play]:not(:disabled)") ?? dlg.querySelector<HTMLElement>("[data-testid=install]:not(:disabled)"))?.click();
      return;
    }
    const ae = document.activeElement as HTMLElement | null;
    if (ae?.matches(".game-card")) {
      ae.click();
      if (ae.dataset.installed === "true") (await waitFor("[role=dialog] [data-testid=play]:not(:disabled)", 4000))?.click();
    }
  };

  const handle = (a: Action) => {
    if (oskRef.current) {
      if (a === "accept") return (document.activeElement as HTMLElement | null)?.click();
      if (a === "back") return closeOsk();
      if (a === "x") return void window.dispatchEvent(new CustomEvent("romm:osk-key", { detail: "Backspace" }));
      if (a === "y") return void window.dispatchEvent(new CustomEvent("romm:osk-key", { detail: "Shift" }));
      if (a === "up" || a === "down" || a === "left" || a === "right") return moveFocus(a);
      return;
    }
    switch (a) {
      case "up": case "down": case "left": case "right": return moveFocus(a);
      case "accept": return accept();
      case "back": return back();
      case "x": return storeRef.current.toasts.forEach((t) => storeRef.current.dismiss(t.id));
      case "y": return (document.querySelector<HTMLElement>("[data-testid=search]") ?? document.querySelector<HTMLElement>(".side-filter"))?.focus();
      case "lb": return cyclePlatform(-1);
      case "rb": return cyclePlatform(1);
      case "lt": return page(-1);
      case "rt": return page(1);
      case "start": return void start();
      case "select": {
        if (zoneOf(document.activeElement) === "sidebar") {
          const t = document.querySelector<HTMLElement>(".main .game-card") ?? defaultFocus();
          if (t) focusEl(t);
        } else document.querySelector<HTMLElement>(".sidebar .nav.active")?.focus();
      }
    }
  };
  const handleRef = useRef(handle);
  handleRef.current = handle;

  // Hints follow focus and DOM changes immediately (the polling loop also refreshes them periodically).
  useEffect(() => {
    if (!enabled) return;
    let t: number | undefined;
    const refresh = () => {
      window.clearTimeout(t);
      t = window.setTimeout(() => {
        const h = hintContext(!!oskRef.current);
        setHint((p) => (p.kind === h.kind && p.primary === h.primary && p.toasts === h.toasts ? p : h));
      }, 40);
    };
    document.addEventListener("focusin", refresh);
    const mo = new MutationObserver(refresh);
    mo.observe(document.body, { childList: true, subtree: true });
    return () => {
      window.clearTimeout(t);
      document.removeEventListener("focusin", refresh);
      mo.disconnect();
    };
  }, [enabled]);

  // ---- the polling loop
  useEffect(() => {
    if (!enabled) {
      setActiveBoth(false);
      return;
    }
    let st = initialState();
    let last = performance.now();
    let lastMaintain = 0;
    let name: string | null = null;

    const maintain = () => {
      if (!activeRef.current) return;
      const ae = document.activeElement as HTMLElement | null;
      const scope = scopeRoot();
      const lost = !ae || ae === document.body || !ae.isConnected;
      if (lost || (scope !== document.body && !scope.contains(ae))) {
        const t = defaultFocus();
        if (t) focusEl(t);
      }
    };

    const frame = () => {
      const now = performance.now();
      const dt = now - last;
      last = now;
      const pads = readPads();

      const current = pads.length ? friendlyName(pads[0].id) : null;
      if (current !== name) {
        name = current;
        setController(current);
        if (current) {
          setKind(controllerKind(pads[0].id));
          storeRef.current.push("info", `Controller connected: ${current}`);
        }
      }

      // Never drive the app from a game: ignore the pad while the window is in the background or an
      // emulator is running, and require a full release afterwards.
      const playing = Object.values(storeRef.current.jobs).some((j) => j.kind === "play" && j.phase === "running");
      const focused = typeof document.hasFocus === "function" ? document.hasFocus() : true;
      if (!pads.length || !focused || playing || document.visibilityState === "hidden") {
        st = initialState();
        return;
      }

      const r = step(st, mergeSnapshots(pads.map((p) => p.snap)), now);
      st = r.state;
      if (r.fired.length || r.scrollY !== 0) {
        setActiveBoth(true);
        if (!r.fired.length) maintain();
      }
      for (const a of r.fired) handleRef.current(a);
      if (r.scrollY !== 0) {
        const sp = scopeRoot() !== document.body ? scopeRoot().querySelector<HTMLElement>(".drawer") ?? scopeRoot() : scrollParent(document.activeElement) ?? document.querySelector<HTMLElement>(".main");
        sp?.scrollBy({ top: r.scrollY * dt * 0.9 });
      }
      if (now - lastMaintain > 250) {
        lastMaintain = now;
        maintain();
        const h = hintContext(!!oskRef.current);
        setHint((p) => (p.kind === h.kind && p.primary === h.primary && p.toasts === h.toasts ? p : h));
      }
    };

    // setInterval rather than requestAnimationFrame so polling isn't throttled when the page isn't composited.
    const id = setInterval(frame, 16);
    return () => clearInterval(id);
  }, [enabled]);

  const labels = useMemo(() => labelsFor(kind), [kind]);
  const value = useMemo<PadCtx>(() => ({ enabled, setEnabled, controller, labels, active }), [enabled, controller, labels, active]);

  return (
    <Ctx.Provider value={value}>
      {children}
      {osk && <OnScreenKeyboard target={osk} onClose={closeOsk} />}
      {enabled && active && <Hints ctx={hint} labels={labels} />}
    </Ctx.Provider>
  );
}
