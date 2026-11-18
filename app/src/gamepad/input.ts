// Gamepad → actions. Pure functions, no DOM, so the timing rules are unit-testable.
// Assumes the W3C "standard" button layout, which browsers apply to Xbox/PlayStation/Switch pads.

export type Action =
  | "up" | "down" | "left" | "right"
  | "accept" | "back" | "x" | "y"
  | "lb" | "rb" | "lt" | "rt"
  | "start" | "select";

const BUTTONS: Record<number, Action> = {
  0: "accept", 1: "back", 2: "x", 3: "y",
  4: "lb", 5: "rb", 6: "lt", 7: "rt",
  8: "select", 9: "start",
  12: "up", 13: "down", 14: "left", 15: "right",
};

export const STICK_THRESHOLD = 0.55;
export const SCROLL_DEADZONE = 0.2;
/** Only these keep firing while held. */
const REPEATABLE: ReadonlySet<Action> = new Set<Action>(["up", "down", "left", "right", "lt", "rt"]);

export interface Snapshot {
  buttons: boolean[];
  axes: number[];
}

export interface Held {
  since: number;
  last: number;
}

export interface InputState {
  /** False until the pad has been seen fully released, so a button that's still down when we
   *  (re)gain focus, e.g. the one that quit the emulator, can't trigger anything. */
  armed: boolean;
  held: Partial<Record<Action, Held>>;
}

export const initialState = (): InputState => ({ armed: false, held: {} });

export interface Timing {
  delay: number;
  interval: number;
}
export const DEFAULT_TIMING: Timing = { delay: 380, interval: 95 };

export function activeActions(s: Snapshot): Set<Action> {
  const out = new Set<Action>();
  s.buttons.forEach((down, i) => {
    const a = BUTTONS[i];
    if (down && a) out.add(a);
  });
  const ax = s.axes[0] ?? 0;
  const ay = s.axes[1] ?? 0;
  if (Math.max(Math.abs(ax), Math.abs(ay)) > STICK_THRESHOLD) {
    // Dominant axis only, so a diagonal flick doesn't fire two directions.
    if (Math.abs(ax) >= Math.abs(ay)) out.add(ax < 0 ? "left" : "right");
    else out.add(ay < 0 ? "up" : "down");
  }
  return out;
}

/** Right stick vertical, −1…1 with a deadzone; used for smooth scrolling. */
export function scrollAxis(s: Snapshot): number {
  const v = s.axes[3] ?? 0;
  return Math.abs(v) < SCROLL_DEADZONE ? 0 : v;
}

export interface StepResult {
  state: InputState;
  fired: Action[];
  scrollY: number;
}

export function step(state: InputState, snap: Snapshot, now: number, timing: Timing = DEFAULT_TIMING): StepResult {
  const down = activeActions(snap);
  const scrollY = scrollAxis(snap);

  if (!state.armed) {
    const neutral = down.size === 0 && scrollY === 0;
    return { state: neutral ? { armed: true, held: {} } : state, fired: [], scrollY: 0 };
  }

  const held: InputState["held"] = {};
  const fired: Action[] = [];
  for (const a of down) {
    const prev = state.held[a];
    if (!prev) {
      fired.push(a);
      held[a] = { since: now, last: now };
    } else if (REPEATABLE.has(a) && now - prev.since >= timing.delay && now - prev.last >= timing.interval) {
      fired.push(a);
      held[a] = { since: prev.since, last: now };
    } else {
      held[a] = prev;
    }
  }
  return { state: { armed: true, held }, fired, scrollY };
}

/** Combine several pads: any pad's button counts, and the strongest axis wins. */
export function mergeSnapshots(snaps: Snapshot[]): Snapshot {
  const buttons: boolean[] = [];
  const axes: number[] = [];
  for (const s of snaps) {
    s.buttons.forEach((b, i) => { buttons[i] = buttons[i] || b; });
    s.axes.forEach((v, i) => { if (Math.abs(v) > Math.abs(axes[i] ?? 0)) axes[i] = v; });
  }
  return { buttons, axes };
}
