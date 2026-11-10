import { chosen, clearScope, countChosen, setChoice } from "../emulation";
import type { EmulationScope, EmulatorSettings } from "../types";

/**
 * Settings → Emulation: how the user's own emulators should run games. Nothing is installed or
 * managed here (the emulators stay the user's); what is chosen is applied each time a game is
 * launched from this app. A setting left on "Don't change" is never touched.
 */
export function EmulationTab({
  scopes, error, values, onChange,
}: {
  scopes: EmulationScope[] | null;
  error: string | null;
  values: EmulatorSettings;
  onChange: (next: EmulatorSettings) => void;
}) {
  return (
    <>
      <section className="card form">
        <h3>Emulator settings</h3>
        <p className="muted small" style={{ margin: 0 }}>
          Choose how games should run and this app applies it every time you press Play, so you don't have to open the
          emulator's own settings. Anything left on “Don't change” stays exactly as the emulator has it.
        </p>
        <span className="hint">
          The emulators stay yours: install and update them yourself and point to them on the Emulators page. If an
          emulator also has a setting for one particular game, that game-specific setting can still win.
        </span>
      </section>

      {error && <div className="card error-text" role="alert">Couldn't load the emulator settings: {error}</div>}
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

      {sc.core && sc.platforms.length > 0 && (
        <div className="emu-platforms">
          <span className="muted small">Used for</span>
          {sc.platforms.map((p) => <span key={p} className="chip chip-muted">{p}</span>)}
        </div>
      )}

      {sc.settings.map((s) => (
        <label key={s.id}>
          {s.label}
          <select
            value={chosen(values, sc.scope, s.id)}
            onChange={(e) => onChange(setChoice(values, sc.scope, s.id, e.target.value))}
            data-testid={`emu-${sc.scope}-${s.id}`}
          >
            <option value="">Don't change</option>
            {s.choices.map((c) => <option key={c.value} value={c.value}>{c.label}</option>)}
          </select>
          {s.help && <span className="hint">{s.help}</span>}
        </label>
      ))}

      <span className="hint">
        {sc.changesFiles
          ? "Written to the emulator's settings file just before a game starts. The first version of that file is kept in a backups folder."
          : "Passed on the command line each time a game starts. This emulator's own files are never changed."}
      </span>
      {!sc.installed && <span className="hint">Not found on this PC yet. You can choose now and it will apply once it is installed.</span>}
      {set > 0 && (
        <div>
          <button className="link-btn" type="button" onClick={() => onChange(clearScope(values, sc.scope))}>
            Reset everything here to “Don't change”
          </button>
        </div>
      )}
    </section>
  );
}
