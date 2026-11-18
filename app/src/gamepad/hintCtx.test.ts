import { describe, expect, it } from "vitest";
import { sameHint, type HintCtx } from "./hintCtx";

const base: HintCtx = { kind: "generic", primary: "", toasts: false, tabs: false };

describe("sameHint", () => {
  it("is true for the same context, and for an equal copy", () => {
    expect(sameHint(base, base)).toBe(true);
    expect(sameHint(base, { ...base })).toBe(true);
  });

  it("notices a change in each field, including a page gaining a tab list", () => {
    const changed: Record<keyof HintCtx, HintCtx> = {
      kind: { ...base, kind: "card" },
      primary: { ...base, primary: "Play" },
      toasts: { ...base, toasts: true },
      // The one that was missed: only the tabs differ, and LB/RB then mean something else.
      tabs: { ...base, tabs: true },
    };
    for (const [field, other] of Object.entries(changed)) {
      expect(sameHint(base, other), `a change in ${field}`).toBe(false);
      expect(sameHint(other, base), `a change in ${field}, the other way round`).toBe(false);
    }
  });

  it("covers every field the context has, so a new one cannot be left out", () => {
    // `changed` above is typed over all of HintCtx's keys, so adding a field fails to compile until a case
    // is written for it; this is the runtime half: whatever keys a context has are the ones compared.
    const extra = { ...base, future: 1 } as unknown as HintCtx;
    expect(sameHint(extra, { ...extra, future: 2 } as unknown as HintCtx)).toBe(false);
  });
});
