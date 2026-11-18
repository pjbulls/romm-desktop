import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { StoreProvider } from "../store";
import { GameCard } from "./GameCard";
import { GameDrawer } from "./GameDrawer";
import type { Game } from "../types";

const game = (o: Partial<Game> = {}): Game => ({
  id: 10, name: "Pokemon Ruby", platformId: 1, platformSlug: "gba", installed: false, sizeBytes: 8 * 1024 * 1024, multiFile: false, coverPath: null,
  versions: [], versionLabel: null, ...o,
});

/** Primary 10 (Europe), others 11 (USA) and 12 (Japan). */
const multi = (installedIds: number[] = []): Game =>
  game({
    installed: installedIds.length > 0,
    versions: [
      { id: 10, label: "Europe · En,Fr,De", isDefault: true, installed: installedIds.includes(10) },
      { id: 11, label: "USA", isDefault: false, installed: installedIds.includes(11) },
      { id: 12, label: "Japan", isDefault: false, installed: installedIds.includes(12) },
    ],
  });

const card = (g: Game) => renderToStaticMarkup(<StoreProvider><GameCard game={g} onOpen={() => {}} /></StoreProvider>);
const drawer = (g: Game) => renderToStaticMarkup(<StoreProvider><GameDrawer game={g} onClose={() => {}} onGoEmulators={() => {}} /></StoreProvider>);
const options = (html: string) => [...html.matchAll(/<option value="(\d+)"[^>]*>([^<]*)<\/option>/g)].map((m) => [m[1], m[2]]);

describe("game card", () => {
  it("is one card with no badge for a game that has a single version", () => {
    const html = card(game());
    expect(html).not.toContain("versions-badge");
    expect(html).toContain('aria-label="Pokemon Ruby"');
  });

  it("says that other versions exist", () => {
    const html = card(multi());
    expect(html).toContain('data-testid="versions-badge"');
    expect(html).toContain(">3 versions<");
    expect(html).toContain('aria-label="Pokemon Ruby, 3 versions"');
  });

  it("shows the game as installed when only a non-primary version is", () => {
    const html = card(multi([12]));
    expect(html).toContain('data-installed="true"');
    expect(html).toContain('aria-label="Pokemon Ruby, 3 versions, installed"');
  });

  it("tells two installed copies apart by their label", () => {
    expect(card(game({ installed: true, versionLabel: "Japan · Rev 1" }))).toContain("Japan · Rev 1");
  });
});

describe("game drawer", () => {
  it("has no version picker for a game with a single version", () => {
    expect(drawer(game())).not.toContain("version-select");
  });

  it("offers every version, the server's primary first and marked", () => {
    const html = drawer(multi());
    expect(options(html)).toEqual([
      ["10", "Europe · En,Fr,De — default"],
      ["11", "USA"],
      ["12", "Japan"],
    ]);
    expect(html).toContain("3 versions of this game");
  });

  it("opens on the primary and offers Install when nothing is installed", () => {
    const html = drawer(multi());
    expect(html).toMatch(/<option value="10"[^>]*selected=""/);
    expect(html).toContain('data-testid="install"');
    expect(html).not.toContain('data-testid="play"');
  });

  it("opens on the installed version so Play is there, and marks it", () => {
    const html = drawer(multi([12]));
    expect(html).toMatch(/<option value="12"[^>]*selected=""/);
    expect(html).not.toMatch(/<option value="10"[^>]*selected=""/);
    expect(html).toContain('data-testid="play"');
    expect(html).not.toContain('data-testid="install"');
    expect(options(html)).toContainEqual(["12", "Japan — installed"]);
  });

  it("stays on the primary when it is installed along with another", () => {
    expect(drawer(multi([10, 12]))).toMatch(/<option value="10"[^>]*selected=""/);
  });

  it("is labelled for assistive tech and the gamepad", () => {
    const html = drawer(multi());
    expect(html).toMatch(/<label[^>]*for="version-select"[^>]*>Version<\/label>/);
    expect(html).toContain('id="version-select"');
  });
});
