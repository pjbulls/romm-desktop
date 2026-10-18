import type { Game, Version } from "./types";

/** This session's installs and uninstalls (the store's `installed`), by ROM id. */
type Overrides = Record<number, boolean>;

/** The ROM ids of every copy of a game; just its own when there is nothing to choose. */
export function copyIds(game: Game): number[] {
  return game.versions.length > 0 ? game.versions.map((v) => v.id) : [game.id];
}

/** Whether one copy is installed. What happened this session wins over what the list said. */
export function isCopyInstalled(game: Game, id: number, overrides: Overrides): boolean {
  const o = overrides[id];
  if (o !== undefined) return o;
  const v = game.versions.find((x) => x.id === id);
  return v ? v.installed : id === game.id && game.installed;
}

/** A game counts as installed when any copy of it is. */
export function isGameInstalled(game: Game, overrides: Overrides): boolean {
  return copyIds(game).some((id) => isCopyInstalled(game, id, overrides));
}

/**
 * The copy the panel opens on: one that is installed, so Play is right there (the primary if it
 * is one of them), otherwise the primary, which is what the library shows and the server chose.
 */
export function initialCopy(game: Game, overrides: Overrides): number {
  if (isCopyInstalled(game, game.id, overrides)) return game.id;
  return copyIds(game).find((id) => isCopyInstalled(game, id, overrides)) ?? game.id;
}

/** One line of the version dropdown: `Europe · En,Fr,De — default, installed`. */
export function versionOptionText(v: Version, installed: boolean): string {
  const notes = [v.isDefault && "default", installed && "installed"].filter(Boolean).join(", ");
  return notes ? `${v.label} — ${notes}` : v.label;
}

/** "3 versions"; empty for a game with nothing to choose. */
export function versionCountText(game: Game): string {
  return game.versions.length > 1 ? `${game.versions.length} versions` : "";
}
