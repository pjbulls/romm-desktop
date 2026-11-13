import { useCallback, useEffect, useMemo, useState } from "react";
import { api, errMsg } from "./api";
import { Emulators } from "./components/Emulators";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { Library } from "./components/Library";
import { Login } from "./components/Login";
import { Settings } from "./components/Settings";
import { Sidebar } from "./components/Sidebar";
import { Toasts } from "./components/Toasts";
import { viewAfterPlatformsChange, visiblePlatforms, withHidden } from "./platforms";
import { useStore } from "./store";
import type { Platform, Status, View } from "./types";

export default function App() {
  const [status, setStatus] = useState<Status | null>(null);
  const [fatal, setFatal] = useState<string | null>(null);
  const [platforms, setPlatforms] = useState<Platform[]>([]);
  const [view, setView] = useState<View>({ kind: "library", platform: null });
  const { push } = useStore();

  useEffect(() => {
    api.status().then(setStatus).catch((e) => setFatal(errMsg(e)));
  }, []);

  const loadPlatforms = useCallback(() => {
    api.platforms().then(setPlatforms).catch((e) => push("error", "Couldn't load platforms", [errMsg(e)]));
  }, [push]);

  useEffect(() => {
    if (!status?.loggedIn) return;
    loadPlatforms();
    // Load the whole library now, so the first list is ready, and count games in the sidebar once
    // it is (until then the server's count of files is shown). A failure surfaces in the list.
    api.loadLibrary().then(loadPlatforms).catch(() => {});
  }, [status?.loggedIn, loadPlatforms]);

  // Platforms the user hid from the game browser are left out of the sidebar (and its counts); the Emulators
  // page gets the whole list so they can be shown again. A hidden platform that is open has nothing left to show.
  const shownPlatforms = useMemo(() => visiblePlatforms(platforms), [platforms]);
  useEffect(() => { setView((v) => viewAfterPlatformsChange(v, platforms)); }, [platforms]);
  // Shown at once, as the sidebar can't wait for the RomM server to be asked for the platform list again.
  const setPlatformHidden = useCallback((slug: string, hidden: boolean) => setPlatforms((ps) => withHidden(ps, slug, hidden)), []);

  if (fatal) return <div className="center-screen"><div className="card"><h1>Something went wrong</h1><p className="error-text">{fatal}</p></div></div>;
  if (!status) return <div className="center-screen"><div className="spinner" aria-label="Loading" /></div>;

  if (!status.loggedIn) {
    return (
      <>
        <Login onConnected={(s) => { setStatus(s); setView({ kind: "library", platform: null }); }} />
        <Toasts />
      </>
    );
  }

  return (
    <div className="shell">
      <Sidebar
        status={status}
        platforms={shownPlatforms}
        view={view}
        onView={setView}
        onDisconnected={(s) => { setStatus(s); setPlatforms([]); }}
      />
      <main className="main">
        <ErrorBoundary key={view.kind}>
        {view.kind === "library" && <Library key={view.platform?.id ?? "all"} mode={{ kind: "library", platform: view.platform }} platforms={platforms} onGoEmulators={() => setView({ kind: "emulators" })} onLibraryChanged={loadPlatforms} />}
        {view.kind === "installed" && <Library key="installed" mode={{ kind: "installed" }} platforms={platforms} onGoEmulators={() => setView({ kind: "emulators" })} onLibraryChanged={loadPlatforms} />}
        {view.kind === "emulators" && <Emulators platforms={platforms} onChanged={loadPlatforms} onHiddenChange={setPlatformHidden} />}
        {view.kind === "settings" && <Settings status={status} onDisconnected={(s) => { setStatus(s); setPlatforms([]); }} />}
        </ErrorBoundary>
      </main>
      <Toasts />
    </div>
  );
}
