import { useState } from "react";
import { api, errMsg } from "../api";
import { normalizeCode } from "../format";
import type { Status } from "../types";

export function Login({ onConnected }: { onConnected: (s: Status) => void }) {
  const [server, setServer] = useState("");
  const [code, setCode] = useState("");
  const [token, setToken] = useState("");
  const [useToken, setUseToken] = useState(false);
  const [deviceName, setDeviceName] = useState("");
  const [insecure, setInsecure] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);

  const ready = server.trim() !== "" && (useToken ? token.trim() !== "" : normalizeCode(code).length >= 8);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!ready || busy) return;
    setBusy(true);
    setError(null);
    try {
      const s = await api.login({
        server: server.trim(),
        code: useToken ? undefined : normalizeCode(code),
        token: useToken ? token.trim() : undefined,
        insecure,
        deviceName: deviceName.trim() || undefined,
      });
      if (s.warnings.length) {
        setWarnings(s.warnings);
        setTimeout(() => onConnected(s), 3500);
      } else onConnected(s);
    } catch (err) {
      setError(errMsg(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="center-screen">
      <form className="card login" onSubmit={submit}>
        <div className="brand-lg"><span className="logo" aria-hidden>🎮</span><h1>RomM Desktop</h1></div>
        <p className="muted">Connect to your RomM server to browse your library, download games, and keep saves and save states in sync.</p>

        <label>
          Server address
          <input data-testid="server" autoFocus placeholder="http://romm.local:8080" value={server} onChange={(e) => setServer(e.target.value)} spellCheck={false} />
        </label>

        {!useToken ? (
          <label>
            Pairing code
            <input data-testid="code" inputMode="numeric" placeholder="1234-5678" value={code} onChange={(e) => setCode(e.target.value)} autoComplete="off" />
            <span className="hint">In RomM open <b>Profile → Client API Tokens → Pair</b> and enter the 8-digit code. Codes are single-use and expire after 5 minutes.</span>
          </label>
        ) : (
          <label>
            API token
            <input data-testid="token" type="password" placeholder="rmm_…" value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" />
            <span className="hint">Needs the scopes: me.read, platforms.read, roms.read, firmware.read, assets.read, assets.write, devices.read, devices.write.</span>
          </label>
        )}

        <details className="advanced">
          <summary>Advanced</summary>
          <label>
            This device's name
            <input placeholder="Shown in RomM's device list" value={deviceName} onChange={(e) => setDeviceName(e.target.value)} />
          </label>
          <label className="check"><input type="checkbox" checked={useToken} onChange={(e) => setUseToken(e.target.checked)} /> Use an API token instead of a pairing code</label>
          <label className="check"><input type="checkbox" checked={insecure} onChange={(e) => setInsecure(e.target.checked)} /> Accept a self-signed HTTPS certificate</label>
        </details>

        {error && <div className="banner banner-error" role="alert" data-testid="login-error">{error}</div>}
        {warnings.map((w, i) => <div key={i} className="banner banner-warn" role="status">{w}</div>)}

        <button className="btn btn-primary btn-lg" type="submit" disabled={!ready || busy} data-testid="connect">
          {busy ? "Connecting…" : "Connect"}
        </button>
      </form>
    </div>
  );
}
