// On-screen keyboard model: layout plus a pure key-application function.

export const OSK_ROWS: string[][] = [
  ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"],
  ["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"],
  ["a", "s", "d", "f", "g", "h", "j", "k", "l", "-"],
  ["Shift", "z", "x", "c", "v", "b", "n", "m", ".", "_"],
  [":", "/", "@", "Space", "Backspace", "Done"],
];

export interface KeyResult {
  value: string;
  shift: boolean;
  done: boolean;
}

export function applyKey(value: string, key: string, shift: boolean): KeyResult {
  switch (key) {
    case "Shift":
      return { value, shift: !shift, done: false };
    case "Backspace":
      // Array.from so an emoji or accented pair is removed as one character.
      return { value: Array.from(value).slice(0, -1).join(""), shift, done: false };
    case "Space":
      return { value: value + " ", shift, done: false };
    case "Done":
      return { value, shift, done: true };
    default:
      // One-shot shift, like a phone keyboard.
      return { value: value + (shift ? key.toUpperCase() : key), shift: false, done: false };
  }
}

/** Set an input's value in a way React's controlled inputs notice. */
export function setNativeValue(el: HTMLInputElement, value: string): void {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  setter?.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}
