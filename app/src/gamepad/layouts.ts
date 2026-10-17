// Controller identification and button labels for on-screen hints.

export type ControllerKind = "xbox" | "playstation" | "nintendo" | "generic";

export function controllerKind(id: string): ControllerKind {
  const s = id.toLowerCase();
  if (/054c|playstation|dualshock|dualsense|sony/.test(s)) return "playstation";
  if (/057e|nintendo|joy-?con|pro controller/.test(s)) return "nintendo";
  if (/045e|xbox|xinput|microsoft/.test(s)) return "xbox";
  return "generic";
}

export interface Labels {
  accept: string; back: string; x: string; y: string;
  lb: string; rb: string; lt: string; rt: string;
  start: string; select: string;
}

const XBOX: Labels = { accept: "A", back: "B", x: "X", y: "Y", lb: "LB", rb: "RB", lt: "LT", rt: "RT", start: "Menu", select: "View" };

export function labelsFor(kind: ControllerKind): Labels {
  switch (kind) {
    case "playstation":
      return { accept: "✕", back: "○", x: "□", y: "△", lb: "L1", rb: "R1", lt: "L2", rt: "R2", start: "Options", select: "Share" };
    case "nintendo":
      // Same physical positions as the Xbox layout, labelled the way the Switch prints them.
      return { accept: "B", back: "A", x: "Y", y: "X", lb: "L", rb: "R", lt: "ZL", rt: "ZR", start: "+", select: "−" };
    default:
      return XBOX;
  }
}

/** "Xbox 360 Controller (XInput STANDARD GAMEPAD)" → "Xbox 360 Controller". */
export function friendlyName(id: string): string {
  const cleaned = id.replace(/\([^)]*\)/g, "").replace(/\s+/g, " ").trim();
  return cleaned || id;
}
