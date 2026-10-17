import type { Platform } from "../types";

/** Text for one option: choices whose emulator or core isn't installed are flagged, not hidden. */
export function choiceText(label: string, installed: boolean): string {
  return installed ? label : `${label} (not installed)`;
}

/**
 * One row of "Which emulator plays what". The dropdown lists only what can really play this
 * platform (the backend decides), so Game Boy shows Game Boy cores and never an Xbox emulator.
 */
export function PlatformChoice({ platform: p, onChange }: { platform: Platform; onChange: (slug: string, value: string) => void }) {
  if (p.choices.length === 0) {
    return (
      <li>
        <span className="row-name">{p.name}</span>
        <span className="muted small" data-testid={`no-emulator-${p.slug}`}>No supported emulator yet</span>
      </li>
    );
  }
  return (
    <li>
      <div className="row-main">
        <span className="row-name">{p.name}</span>
        {p.ignoredOverride && (
          <span className="muted small" data-testid={`ignored-${p.slug}`}>
            {p.ignoredOverride} was saved for this, but it can't play {p.name}, so Automatic is used.
          </span>
        )}
      </div>
      <select className="choice-select" value={p.selected} onChange={(e) => onChange(p.slug, e.target.value)} aria-label={`Emulator for ${p.name}`}>
        <option value="">{p.automaticLabel ? `Automatic (${p.automaticLabel})` : "Automatic"}</option>
        {p.choices.map((c) => <option key={c.value} value={c.value}>{choiceText(c.label, c.installed)}</option>)}
      </select>
    </li>
  );
}
