import { useCallback, useEffect, useRef, useState } from "react";
import { api, errMsg } from "./api";
import { sameChoices } from "./emulation";
import { useStore } from "./store";
import type { EmulationScope, EmulatorSettings } from "./types";

/**
 * Everything the Emulator settings tab works with: what can be controlled for each emulator (asked for
 * once), what's chosen and saved, and the edits not saved yet. It belongs to the page rather than the
 * tab, so edits survive switching tabs. Nothing is asked for until `wanted` first turns true, which is
 * when the tab is first opened.
 */
export function useEmulatorSettings(wanted: boolean) {
  const { push } = useStore();
  const [scopes, setScopes] = useState<EmulationScope[] | null>(null);
  const [saved, setSaved] = useState<EmulatorSettings | null>(null);
  const [values, setValues] = useState<EmulatorSettings>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const started = useRef(false);

  useEffect(() => {
    if (!wanted || started.current) return;
    started.current = true;
    api.emulationSchema().then(setScopes).catch((e) => setError(errMsg(e)));
    api.emulatorSettings().then((v) => { setSaved(v); setValues(v); }).catch((e) => setError(errMsg(e)));
  }, [wanted]);

  const save = useCallback(async () => {
    setBusy(true);
    try {
      await api.saveEmulatorSettings(values);
      setSaved(values);
      push("success", "Emulator settings saved");
    } catch (e) {
      push("error", "Couldn't save emulator settings", [errMsg(e)]);
    } finally {
      setBusy(false);
    }
  }, [values, push]);

  return {
    /** What can be controlled; null until both it and the saved choices have arrived, so nothing is edited before then. */
    scopes: scopes && saved ? scopes : null,
    error,
    values,
    setValues,
    dirty: saved !== null && !sameChoices(values, saved),
    busy,
    save,
    discard: () => saved && setValues(saved),
  };
}
