/** Does the page currently shown have a tab list? (Pages with tabs use LB/RB for them, not for platforms.) */
export function hasTabs(root: ParentNode = document): boolean {
  return root.querySelectorAll(".main [role=tablist] [role=tab]").length > 1;
}

/**
 * Selects the previous (-1) or next (+1) tab of the page's tab list, wrapping around, and keeps
 * focus on it so the controller carries on from there. Returns false when the page has no tabs,
 * so the caller can do whatever LB/RB mean elsewhere.
 */
export function cycleTab(delta: number, root: ParentNode = document): boolean {
  const tabs = Array.from(root.querySelectorAll<HTMLElement>(".main [role=tablist] [role=tab]"));
  if (tabs.length < 2) return false;
  const i = Math.max(0, tabs.findIndex((t) => t.getAttribute("aria-selected") === "true"));
  const next = tabs[(i + delta + tabs.length) % tabs.length];
  next.click();
  next.focus({ preventScroll: true });
  return true;
}
