import { useMemo, useState } from "react";
import { api, errMsg } from "../api";
import { useStore } from "../store";
import type { Platform, Status, View } from "../types";

interface Props {
  status: Status;
  platforms: Platform[];
  view: View;
  onView: (v: View) => void;
  onDisconnected: (s: Status) => void;
}

export function Sidebar({ status, platforms, view, onView, onDisconnected }: Props) {
  const { syncingAll, push } = useStore();
  const [filter, setFilter] = useState("");
  const shown = useMemo(
    () => platforms.filter((p) => p.romCount > 0 && p.name.toLowerCase().includes(filter.toLowerCase())).sort((a, b) => a.name.localeCompare(b.name)),
    [platforms, filter],
  );
  const total = useMemo(() => platforms.reduce((n, p) => n + p.romCount, 0), [platforms]);
  const isLib = (p: Platform | null) => view.kind === "library" && (view.platform?.id ?? null) === (p?.id ?? null);

  async function syncAll() {
    try {
      await api.syncAll();
    } catch (e) {
      push("error", "Couldn't start sync", [errMsg(e)]);
    }
  }

  return (
    <nav className="sidebar" aria-label="Main">
      <div className="brand"><span className="logo" aria-hidden>🎮</span> RomM Desktop</div>

      <div className="nav-group">
        <button className={`nav ${isLib(null) ? "active" : ""}`} onClick={() => onView({ kind: "library", platform: null })} data-testid="nav-all">
          <span>All games</span><span className="count">{total}</span>
        </button>
        <button className={`nav ${view.kind === "installed" ? "active" : ""}`} onClick={() => onView({ kind: "installed" })} data-testid="nav-installed">
          <span>Installed</span>
        </button>
      </div>

      <div className="nav-label">Platforms</div>
      <input className="side-filter" placeholder="Filter platforms" value={filter} onChange={(e) => setFilter(e.target.value)} aria-label="Filter platforms" />
      <div className="platform-list">
        {shown.map((p) => (
          <button key={p.id} className={`nav ${isLib(p) ? "active" : ""}`} onClick={() => onView({ kind: "library", platform: p })} title={p.emulatorName ? `Plays in ${p.emulatorName}` : "No emulator set up for this platform"}>
            <span className="nav-name">{p.name}</span>
            {!p.emulatorInstalled && <span className="dot dot-warn" aria-label="No emulator available" />}
            <span className="count">{p.romCount}</span>
          </button>
        ))}
        {shown.length === 0 && <div className="muted small pad">No platforms</div>}
      </div>

      <div className="nav-group bottom">
        <button className={`nav ${view.kind === "emulators" ? "active" : ""}`} onClick={() => onView({ kind: "emulators" })} data-testid="nav-emulators">Emulators &amp; BIOS</button>
        <button className={`nav ${view.kind === "settings" ? "active" : ""}`} onClick={() => onView({ kind: "settings" })} data-testid="nav-settings">Settings</button>
        <button className="btn btn-block" onClick={syncAll} disabled={syncingAll} data-testid="sync-all">
          {syncingAll ? <><span className="spinner sm" /> Syncing…</> : "Sync all saves"}
        </button>
        <div className="server-line" title={status.serverUrl ?? ""}>
          <span className="dot dot-ok" /> {status.serverUrl?.replace(/^https?:\/\//, "")}
        </div>
        <button className="link-btn" onClick={async () => onDisconnected(await api.logout())}>Disconnect</button>
      </div>
    </nav>
  );
}
