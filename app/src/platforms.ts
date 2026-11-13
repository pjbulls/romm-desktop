import type { Platform, View } from "./types";

/** The platforms the game browser offers: the ones the user hasn't hidden. */
export function visiblePlatforms(platforms: Platform[]): Platform[] {
  return platforms.filter((p) => !p.hidden);
}

/** How many hidden platforms have games: the number in "Hidden platforms (n)". A hidden platform with nothing in it isn't worth a visit. */
export function hiddenPlatformCount(platforms: Platform[]): number {
  return platforms.filter((p) => p.hidden && p.romCount > 0).length;
}

/** `platforms` with one platform hidden or shown (a new list; the others are the same objects). */
export function withHidden(platforms: Platform[], slug: string, hidden: boolean): Platform[] {
  return platforms.map((p) => (p.slug === slug && p.hidden !== hidden ? { ...p, hidden } : p));
}

/**
 * A view whose games are no longer listed has nothing left to show, so it becomes the list of all
 * games: a platform that has since been hidden, or the "Hidden platforms" view once no hidden platform
 * has games. Any other view is returned as it is (the same object, so state holding it doesn't change).
 * A platform that is simply missing from the list is left alone: that is the list still loading.
 */
export function viewAfterPlatformsChange(view: View, platforms: Platform[]): View {
  if (view.kind === "hidden") return hiddenPlatformCount(platforms) === 0 ? { kind: "library", platform: null } : view;
  if (view.kind !== "library" || !view.platform) return view;
  const slug = view.platform.slug;
  return platforms.some((p) => p.slug === slug && p.hidden) ? { kind: "library", platform: null } : view;
}
