/** What the controller hints depend on. */
export interface HintCtx {
  kind: "osk" | "dialog" | "card" | "text" | "generic";
  primary: "" | "Play" | "Install";
  toasts: boolean;
  /** The page has tabs, so LB/RB switch tab rather than platform. */
  tabs: boolean;
}

/**
 * Whether two contexts would show the same hints, so the state can be left alone. Every field is
 * compared, whatever the fields are: the comparison was once written out by hand and left `tabs`
 * out, so a page that only gained a tab list kept saying LB/RB changed the platform.
 */
export function sameHint(a: HintCtx, b: HintCtx): boolean {
  return (Object.keys(a) as (keyof HintCtx)[]).every((k) => a[k] === b[k]);
}
