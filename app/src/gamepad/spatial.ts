// Directional ("spatial") focus navigation on plain rectangles. DOM-free, so it's unit-testable.

export type Dir = "up" | "down" | "left" | "right";

export interface Box {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

const cx = (b: Box) => (b.left + b.right) / 2;
const cy = (b: Box) => (b.top + b.bottom) / 2;

/** Distance between two 1-D intervals; 0 when they overlap. */
const gap = (a0: number, a1: number, b0: number, b1: number) => Math.max(0, Math.max(a0, b0) - Math.min(a1, b1));

/** How much two 1-D intervals overlap. */
const overlap = (a0: number, a1: number, b0: number, b1: number) => Math.max(0, Math.min(a1, b1) - Math.max(a0, b0));

/**
 * Best candidate in a direction, in two tiers:
 *  1. elements lined up with the current one on the other axis (at least a quarter overlap), nearest first.
 *     This keeps Down inside a long page instead of hopping to a closer but unrelated sidebar item, and keeps
 *     Down from a grid card on the card below rather than one diagonally;
 *  2. otherwise the nearest of the rest, penalising sideways drift.
 * A candidate must be meaningfully ahead (half the smaller element's size), not merely a pixel or two:
 * a focused element lifted/scaled by a few px would otherwise see its same-row neighbours as "below" it.
 */
export function pickNext<T>(from: Box, cands: { item: T; box: Box }[], dir: Dir): T | null {
  const horizontal = dir === "left" || dir === "right";
  let best: { item: T; score: number } | null = null;
  for (const { item, box } of cands) {
    const primary = dir === "right" ? cx(box) - cx(from)
      : dir === "left" ? cx(from) - cx(box)
      : dir === "down" ? cy(box) - cy(from)
      : cy(from) - cy(box);
    const size = horizontal ? Math.min(from.right - from.left, box.right - box.left) : Math.min(from.bottom - from.top, box.bottom - box.top);
    if (primary < Math.max(1, size * 0.5)) continue;

    const [f0, f1, b0, b1] = horizontal ? [from.top, from.bottom, box.top, box.bottom] : [from.left, from.right, box.left, box.right];
    const crossDelta = Math.abs(horizontal ? cy(box) - cy(from) : cx(box) - cx(from));
    const aligned = overlap(f0, f1, b0, b1) >= Math.min(f1 - f0, b1 - b0) * 0.25;
    const score = aligned
      ? primary + crossDelta * 0.5
      : 1e6 + primary + gap(f0, f1, b0, b1) * 3 + crossDelta * 0.5;
    if (!best || score < best.score) best = { item, score };
  }
  return best ? best.item : null;
}
