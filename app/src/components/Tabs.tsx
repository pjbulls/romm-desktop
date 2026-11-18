import { useRef, type ReactNode } from "react";

export interface TabDef<T extends string> {
  id: T;
  label: string;
  /** Unsaved changes on this tab: shown as a dot. */
  dirty?: boolean;
}

/**
 * A row of tabs. Every tab stays in the tab order (no roving tabindex) because the game controller
 * navigates by moving focus to the nearest focusable element, and would skip tabs that aren't.
 * Arrow keys, Home and End also move between them, as on any tab list.
 */
export function Tabs<T extends string>({
  tabs, active, onChange, label, idPrefix,
}: {
  tabs: TabDef<T>[];
  active: T;
  onChange: (id: T) => void;
  label: string;
  idPrefix: string;
}) {
  const refs = useRef<Record<string, HTMLButtonElement | null>>({});

  function onKeyDown(e: React.KeyboardEvent, i: number) {
    let to = -1;
    if (e.key === "ArrowRight") to = (i + 1) % tabs.length;
    else if (e.key === "ArrowLeft") to = (i - 1 + tabs.length) % tabs.length;
    else if (e.key === "Home") to = 0;
    else if (e.key === "End") to = tabs.length - 1;
    if (to < 0) return;
    e.preventDefault();
    onChange(tabs[to].id);
    refs.current[tabs[to].id]?.focus();
  }

  return (
    <div className="tabs" role="tablist" aria-label={label}>
      {tabs.map((t, i) => (
        <button
          key={t.id}
          ref={(el) => { refs.current[t.id] = el; }}
          id={`${idPrefix}-tab-${t.id}`}
          type="button"
          role="tab"
          aria-selected={t.id === active}
          aria-controls={`${idPrefix}-panel-${t.id}`}
          className={`tab${t.id === active ? " active" : ""}`}
          data-testid={`tab-${t.id}`}
          onClick={() => onChange(t.id)}
          onKeyDown={(e) => onKeyDown(e, i)}
        >
          {t.label}
          {t.dirty && <span className="tab-dot" role="img" aria-label="unsaved changes" />}
        </button>
      ))}
    </div>
  );
}

/** The content of the active tab. Only that one is rendered, so hidden controls are never focusable. */
export function TabPanel({ id, idPrefix, children }: { id: string; idPrefix: string; children: ReactNode }) {
  return (
    <div role="tabpanel" id={`${idPrefix}-panel-${id}`} aria-labelledby={`${idPrefix}-tab-${id}`}>
      {children}
    </div>
  );
}
