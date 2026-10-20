import { invoke } from "@tauri-apps/api/core";

/**
 * The page reporting on itself to a log file the backend keeps (see romm_core::diagnostics). When
 * the window goes dark there is nothing on screen and no console to look at; this leaves the numbers
 * from just before, and shows whether the page was still alive after.
 *
 * Counts and timings only: no game names, no addresses.
 */

type MemoryInfo = { usedJSHeapSize: number };

/** One periodic report, as `key=value` pairs. `lagMs`: how late the timer that drives the reports fired. */
export function report(lagMs: number): { line: string; visible: boolean } {
  const visible = document.visibilityState === "visible";
  const mem = (performance as unknown as { memory?: MemoryInfo }).memory;
  const imgs = Array.from(document.images);
  const pads = (() => {
    try { return Array.from(navigator.getGamepads?.() ?? []).filter(Boolean).length; } catch { return -1; }
  })();
  const parts: [string, string | number][] = [
    ["heap", mem ? `${Math.round(mem.usedJSHeapSize / 1048576)}MB` : "n/a"],
    ["nodes", document.getElementsByTagName("*").length],
    ["cards", document.querySelectorAll(".game-card").length],
    ["imgs", `${imgs.filter((i) => i.complete && i.naturalWidth > 0).length}/${imgs.length}`],
    ["scroll", Math.round(document.querySelector(".main")?.scrollTop ?? 0)],
    ["lag", `${Math.max(0, Math.round(lagMs))}ms`],
    ["vis", visible ? "visible" : document.visibilityState],
    ["focus", document.hasFocus() ? 1 : 0],
    ["pads", pads],
    ["hints", document.querySelector("[data-testid=hints]") ? 1 : 0],
    ["size", `${window.innerWidth}x${window.innerHeight}@${window.devicePixelRatio}`],
  ];
  return { line: "hb " + parts.map(([k, v]) => `${k}=${v}`).join(" "), visible };
}

function send(line: string, reportBeat: boolean, visible = document.visibilityState === "visible") {
  invoke("diag_page", { line, report: reportBeat, visible }).catch(() => {});
}

/** Starts reporting; returns a function that stops it. */
export function startDiagnostics({ intervalMs = 2000 }: { intervalMs?: number } = {}): () => void {
  send(`start ua=${navigator.userAgent.slice(0, 160)} size=${window.innerWidth}x${window.innerHeight}@${window.devicePixelRatio}`, false);

  let last = performance.now();
  const timer = setInterval(() => {
    const now = performance.now();
    const { line, visible } = report(now - last - intervalMs);
    last = now;
    send(line, true, visible);
  }, intervalMs);

  const onError = (e: ErrorEvent) => send(`error ${e.message} @${(e.filename ?? "").split("/").pop()}:${e.lineno}`, false);
  const onRejection = (e: PromiseRejectionEvent) => send(`rejection ${String(e.reason).slice(0, 300)}`, false);
  const onVisibility = () => send(`visibility ${document.visibilityState}`, false);
  const onCsp = (e: SecurityPolicyViolationEvent) => send(`csp blocked ${e.violatedDirective} ${e.blockedURI.slice(0, 60)}`, false);
  window.addEventListener("error", onError);
  window.addEventListener("unhandledrejection", onRejection);
  document.addEventListener("visibilitychange", onVisibility);
  document.addEventListener("securitypolicyviolation", onCsp);

  // A task that kept the page busy for half a second or more (not every engine reports these).
  let longTasks: PerformanceObserver | null = null;
  try {
    longTasks = new PerformanceObserver((list) => {
      for (const t of list.getEntries()) if (t.duration >= 500) send(`longtask ${Math.round(t.duration)}ms`, false);
    });
    longTasks.observe({ type: "longtask", buffered: true });
  } catch {
    longTasks = null;
  }

  return () => {
    clearInterval(timer);
    window.removeEventListener("error", onError);
    window.removeEventListener("unhandledrejection", onRejection);
    document.removeEventListener("visibilitychange", onVisibility);
    document.removeEventListener("securitypolicyviolation", onCsp);
    longTasks?.disconnect();
  };
}
