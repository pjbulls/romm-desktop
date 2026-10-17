import { useCallback, useEffect, useRef, useState } from "react";
import { applyKey, OSK_ROWS, setNativeValue } from "../gamepad/osk";

const GLYPH: Record<string, string> = { Backspace: "⌫", Space: "Space", Shift: "⇧", Done: "Done" };

/** Types into a real <input>, so the page behaves exactly as if a keyboard had been used. */
export function OnScreenKeyboard({ target, onClose }: { target: HTMLInputElement; onClose: () => void }) {
  const [value, setValue] = useState(target.value);
  const [shift, setShift] = useState(false);
  const valueRef = useRef(target.value);
  const shiftRef = useRef(false);
  const root = useRef<HTMLDivElement | null>(null);

  const press = useCallback(
    (key: string) => {
      const r = applyKey(valueRef.current, key, shiftRef.current);
      valueRef.current = r.value;
      shiftRef.current = r.shift;
      setValue(r.value);
      setShift(r.shift);
      if (key !== "Shift" && key !== "Done" && r.value !== target.value) setNativeValue(target, r.value);
      if (r.done) onClose();
    },
    [target, onClose],
  );

  // Face buttons (X = delete, Y = shift) arrive as events from the gamepad layer.
  useEffect(() => {
    const h = (e: Event) => press((e as CustomEvent<string>).detail);
    window.addEventListener("romm:osk-key", h);
    return () => window.removeEventListener("romm:osk-key", h);
  }, [press]);

  useEffect(() => {
    root.current?.querySelector<HTMLElement>('[data-key="1"]')?.focus();
    return () => target.focus();
  }, [target]);

  const label = (target.closest("label")?.childNodes[0]?.textContent ?? target.placeholder ?? "").trim();
  const shown = target.type === "password" ? "•".repeat(Array.from(value).length) : value;

  return (
    <div className="osk" role="dialog" aria-modal="true" aria-label="On-screen keyboard" ref={root} data-testid="osk">
      <div className="osk-label">{label || "Type here"}</div>
      <div className="osk-preview" data-testid="osk-preview">{shown}<span className="osk-caret" /></div>
      {OSK_ROWS.map((row, i) => (
        <div className="osk-row" key={i}>
          {row.map((k) => {
            const letter = k.length === 1 && /[a-z]/.test(k);
            return (
              <button
                key={k}
                type="button"
                data-key={k}
                className={`osk-key${k === "Space" ? " space" : ""}${["Shift", "Backspace", "Done"].includes(k) ? " wide" : ""}${k === "Shift" && shift ? " on" : ""}${k === "Done" ? " done" : ""}`}
                onClick={() => press(k)}
                aria-label={k.length > 1 ? k : undefined}
              >
                {GLYPH[k] ?? (letter && shift ? k.toUpperCase() : k)}
              </button>
            );
          })}
        </div>
      ))}
    </div>
  );
}
