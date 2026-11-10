import type { EmulatorSettings, Settings, SettingsTab } from "./types";

/**
 * Pure helpers for Settings → Emulation. The choices are `scope → setting id → value`, where a
 * scope is an emulator's id (or `retroarch:<core>`) and a missing entry, like an empty value,
 * means "don't change": the emulator's own setting is left alone.
 */

/** What's chosen for a setting; "" is "don't change". */
export function chosen(values: EmulatorSettings, scope: string, id: string): string {
  return values[scope]?.[id] ?? "";
}

function tidy(values: EmulatorSettings): EmulatorSettings {
  // No empty entries and keys in a fixed order, so equal choices are equal however they were reached
  // (the server keeps them sorted, and saving must not look like a change).
  const out: EmulatorSettings = {};
  for (const scope of Object.keys(values).sort()) {
    const kept = Object.keys(values[scope]).sort().filter((id) => values[scope][id] !== "");
    if (kept.length) out[scope] = Object.fromEntries(kept.map((id) => [id, values[scope][id]]));
  }
  return out;
}

/** `values` with one setting changed (a new object; "" clears it). */
export function setChoice(values: EmulatorSettings, scope: string, id: string, value: string): EmulatorSettings {
  return tidy({ ...values, [scope]: { ...values[scope], [id]: value } });
}

/** `values` with everything chosen for one emulator put back to "don't change". */
export function clearScope(values: EmulatorSettings, scope: string): EmulatorSettings {
  const { [scope]: _dropped, ...rest } = values;
  return tidy(rest);
}

export function countChosen(values: EmulatorSettings, scope: string): number {
  return Object.values(values[scope] ?? {}).filter((v) => v !== "").length;
}

/** Do two sets of choices say the same thing? */
export function sameChoices(a: EmulatorSettings, b: EmulatorSettings): boolean {
  return JSON.stringify(tidy(a)) === JSON.stringify(tidy(b));
}

/** Which tabs of the Settings page hold changes that haven't been saved. */
export function dirtyTabs(form: Settings, saved: Settings): Set<SettingsTab> {
  const out = new Set<SettingsTab>();
  if (!sameChoices(form.emulatorSettings, saved.emulatorSettings)) out.add("emulation");
  if (form.acceptInvalidCerts !== saved.acceptInvalidCerts) out.add("connection");
  // Everything else on the page (downloads, saves and states) is the General tab's.
  const rest = (s: Settings) => {
    const { emulatorSettings: _e, acceptInvalidCerts: _c, ...others } = s;
    return JSON.stringify(others);
  };
  if (rest(form) !== rest(saved)) out.add("general");
  return out;
}
