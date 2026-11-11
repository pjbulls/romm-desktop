import type { Platform } from "../types";

/** Text for one option: choices whose emulator or core isn't installed are flagged, not hidden. */
export function choiceText(label: string, installed: boolean): string {
  return installed ? label : `${label} (not installed)`;
}

/**
 * One row of "Which emulator plays what". The dropdown lists only what can really play this
 * platform (the backend decides), so Game Boy shows Game Boy cores and never an Xbox emulator.
 */
export function PlatformChoice({
  platform: p, onChange, onToggleHidden,
}: {
  platform: Platform;
  onChange: (slug: string, value: string) => void;
  /** When given, the row has a checkbox to show or hide the platform (and all its games) in the game browser. */
  onToggleHidden?: (slug: string, hidden: boolean) => void;
}) {
  const lead = (note?: string) => (
    <div className="row-lead">
      {onToggleHidden && (
        <input
          type="checkbox"
          className="show-toggle"
          checked={!p.hidden}
          onChange={(e) => onToggleHidden(p.slug, !e.target.checked)}
          aria-label={`Show ${p.name} in the library`}
          title="Show in the library"
          data-testid={`show-${p.slug}`}
        />
      )}
      <div className="row-main">
        <span className="row-name">{p.name}</span>
        {p.hidden && <span className="muted small" data-testid={`hidden-${p.slug}`}>Hidden from the library</span>}
        {note && (
          <span className="muted small" data-testid={`ignored-${p.slug}`}>{note}</span>
        )}
      </div>
    </div>
  );
  const rowClass = p.hidden ? "is-hidden" : undefined;

  if (p.choices.length === 0) {
    return (
      <li className={rowClass}>
        {lead()}
        <span className="muted small" data-testid={`no-emulator-${p.slug}`}>No supported emulator yet</span>
      </li>
    );
  }
  return (
    <li className={rowClass}>
      {lead(p.ignoredOverride ? `${p.ignoredOverride} was saved for this, but it can't play ${p.name}, so Automatic is used.` : undefined)}
      <select className="choice-select" value={p.selected} onChange={(e) => onChange(p.slug, e.target.value)} aria-label={`Emulator for ${p.name}`}>
        <option value="">{p.automaticLabel ? `Automatic (${p.automaticLabel})` : "Automatic"}</option>
        {p.choices.map((c) => <option key={c.value} value={c.value}>{choiceText(c.label, c.installed)}</option>)}
      </select>
    </li>
  );
}
