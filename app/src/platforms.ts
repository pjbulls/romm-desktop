import type { Platform, View } from "./types";

/** The platforms the game browser offers: the ones the user hasn't hidden. */
export function visiblePlatforms(platforms: Platform[]): Platform[] {
  return platforms.filter((p) => !p.hidden);
}

/** `platforms` with one platform hidden or shown (a new list; the others are the same objects). */
export function withHidden(platforms: Platform[], slug: string, hidden: boolean): Platform[] {
  return platforms.map((p) => (p.slug === slug && p.hidden !== hidden ? { ...p, hidden } : p));
}

/**
 * A view of a platform that has since been hidden has nothing left to show, so it becomes the list of
 * all games. Any other view is returned as it is (the same object, so state holding it doesn't change).
 * A platform that is simply missing from the list is left alone: that is the list still loading.
 */
export function viewAfterPlatformsChange(view: View, platforms: Platform[]): View {
  if (view.kind !== "library" || !view.platform) return view;
  const slug = view.platform.slug;
  return platforms.some((p) => p.slug === slug && p.hidden) ? { kind: "library", platform: null } : view;
}
