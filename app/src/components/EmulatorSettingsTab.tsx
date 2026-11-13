import { chosen, clearScope, countChosen, setChoice } from "../emulation";
import type { EmulationScope, EmulatorSettings } from "../types";

/**
 * Emulators & BIOS → Emulator settings: how the user's own emulators should run games. Nothing is
 * installed or managed here (the emulators stay the user's); what is chosen is applied each time a
 * game is launched from this app. A setting left on "Don't change" is never touched.
 */
export function EmulatorSettingsTab({
  scopes, error, values, onChange,
}: {
  scopes: EmulationScope[] | null;
  error: string | null;
  values: EmulatorSettings;
  onChange: (next: EmulatorSettings) => void;
}) {
  return (
    <>
      {error && <div className="card error-text" role="alert">Couldn't load emulator settings: {error}</div>}
      {!scopes && !error && <div className="loading-row"><span className="spinner sm" /> Loading…</div>}

      {scopes?.map((sc) => (
        <ScopeCard key={sc.scope} sc={sc} values={values} onChange={onChange} />
      ))}
    </>
  );
}

function ScopeCard({ sc, values, onChange }: { sc: EmulationScope; values: EmulatorSettings; onChange: (next: EmulatorSettings) => void }) {
  const set = countChosen(values, sc.scope);
  return (
    <section className="card form" data-testid={`emu-scope-${sc.scope}`}>
      <div className="emu-head">
        <h3>{sc.name}</h3>
        <span className="row-flex" style={{ gap: 6 }}>
          {set > 0 && <span className="chip chip-accent">{set} set</span>}
          <span className={`chip ${sc.installed ? "chip-ok" : "chip-muted"}`}>{sc.installed ? "Installed" : "Not installed"}</span>
        </span>
      </div>

      {/* The same rows as Emulator mapping: the name (with its note under it) on the left, the dropdown on the right. */}
      <ul className="rows">
        {sc.settings.map((s) => (
          <li key={s.id}>
            <div className="row-main">
              <span className="row-name">{s.label}</span>
              {s.help && <span className="muted small">{s.help}</span>}
            </div>
            <select
              className="choice-select"
              value={chosen(values, sc.scope, s.id)}
              onChange={(e) => onChange(setChoice(values, sc.scope, s.id, e.target.value))}
              aria-label={`${s.label} for ${sc.name}`}
              data-testid={`emu-${sc.scope}-${s.id}`}
            >
              <option value="">Don't change</option>
              {s.choices.map((c) => <option key={c.value} value={c.value}>{c.label}</option>)}
            </select>
          </li>
        ))}
      </ul>

      {set > 0 && (
        <div>
          <button className="link-btn" type="button" onClick={() => onChange(clearScope(values, sc.scope))}>
            Reset all to "Don't change"
          </button>
        </div>
      )}
    </section>
  );
}
