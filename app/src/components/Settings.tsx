import { useEffect, useState } from "react";
import { api, errMsg, pickFolder } from "../api";
import { dirtyTabs } from "../emulation";
import { POLICY_LABEL } from "../format";
import { useGamepad } from "../gamepad/GamepadProvider";
import { useStore } from "../store";
import type { ConflictPolicy, Settings as S, SettingsTab, Status } from "../types";
import { TabPanel, Tabs, type TabDef } from "./Tabs";

export function Settings({ status, onDisconnected }: { status: Status; onDisconnected: (s: Status) => void }) {
  const { push } = useStore();
  const [saved, setSaved] = useState<S | null>(null);
  const [form, setForm] = useState<S | null>(null);
  const [busy, setBusy] = useState(false);
  const [tab, setTab] = useState<SettingsTab>("general");
  const pad = useGamepad();

  useEffect(() => {
    api.settings().then((s) => { setSaved(s); setForm(s); }).catch((e) => push("error", "Couldn't load settings", [errMsg(e)]));
  }, [push]);

  if (!form || !saved) return <div className="loading-row"><span className="spinner sm" /> Loading…</div>;
  const dirtyIn = dirtyTabs(form, saved);
  const dirty = dirtyIn.size > 0;
  const tabs: TabDef<SettingsTab>[] = [
    { id: "general", label: "General", dirty: dirtyIn.has("general") },
    { id: "controller", label: "Controller" },
    { id: "connection", label: "Connection", dirty: dirtyIn.has("connection") },
  ];
  const set = <K extends keyof S>(k: K, v: S[K]) => setForm({ ...form, [k]: v });

  async function browse() {
    const p = await pickFolder("Choose where games are downloaded");
    if (p) set("romDir", p);
  }

  async function save() {
    if (!form) return;
    setBusy(true);
    try {
      await api.saveSettings(form);
      setSaved(form);
      push("success", "Settings saved");
    } catch (e) {
      push("error", "Couldn't save settings", [errMsg(e)]);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="page narrow">
      <header className="topbar"><h1>Settings</h1></header>
      <Tabs tabs={tabs} active={tab} onChange={setTab} label="Settings sections" idPrefix="settings" />

      <TabPanel id={tab} idPrefix="settings">
      {tab === "general" && <>
      <section className="card form">
        <h3>Downloads</h3>
        <label>
          ROM folder
          <div className="input-row">
            <input value={form.romDir} onChange={(e) => set("romDir", e.target.value)} data-testid="rom-dir" />
            <button className="btn" type="button" onClick={browse}>Browse…</button>
          </div>
        </label>
      </section>

      <section className="card form">
        <h3>Saves &amp; save states</h3>
        <label className="check">
          <input type="checkbox" checked={form.syncStates} onChange={(e) => set("syncStates", e.target.checked)} data-testid="sync-states" />
          Sync save states
        </label>
        <label>
          Skip states larger than
          <div className="input-row short">
            <input type="number" min={1} value={form.maxStateMb} disabled={!form.syncStates} onChange={(e) => set("maxStateMb", Math.max(1, Number(e.target.value) || 1))} />
            <span>MB</span>
          </div>
        </label>
        <span className="hint">Saves are always synced.</span>

        <fieldset>
          <legend>When both this PC and the server changed the same file</legend>
          {(Object.keys(POLICY_LABEL) as ConflictPolicy[]).map((k) => (
            <label className="radio" key={k}>
              <input type="radio" name="policy" checked={form.conflictPolicy === k} onChange={() => set("conflictPolicy", k)} />
              <span><b>{POLICY_LABEL[k].title}</b><span className="hint block">{POLICY_LABEL[k].help}</span></span>
            </label>
          ))}
        </fieldset>
      </section>
      </>}

      {tab === "controller" &&
      <section className="card form" data-testid="controller-settings">
        <h3>Game controller</h3>
        <label className="check">
          <input type="checkbox" checked={pad.enabled} onChange={(e) => pad.setEnabled(e.target.checked)} data-testid="pad-enabled" />
          Navigate with a game controller
        </label>
        <span className="hint" data-testid="pad-status">
          {pad.controller ? `Connected: ${pad.controller}` : "No controller detected."}
          {" "}Input is ignored while a game is running.
        </span>
        <dl className="kv pad-legend">
          <dt>D-pad / left stick</dt><dd>Move around</dd>
          <dt>{pad.labels.accept}</dt><dd>Select (opens an on-screen keyboard on text fields)</dd>
          <dt>{pad.labels.back}</dt><dd>Back / close</dd>
          <dt>{pad.labels.start}</dt><dd>Play the focused game</dd>
          <dt>{pad.labels.lb} / {pad.labels.rb}</dt><dd>Previous / next platform, or tab on pages with tabs</dd>
          <dt>{pad.labels.lt} / {pad.labels.rt}</dt><dd>Page up / down</dd>
          <dt>{pad.labels.y}</dt><dd>Search</dd>
          <dt>{pad.labels.x}</dt><dd>Dismiss notifications</dd>
          <dt>{pad.labels.select}</dt><dd>Switch between the menu and the games</dd>
          <dt>Right stick</dt><dd>Scroll</dd>
        </dl>
      </section>}

      {tab === "connection" &&
      <section className="card form">
        <h3>Connection</h3>
        <dl className="kv">
          <dt>Server</dt><dd className="mono">{status.serverUrl}</dd>
          <dt>This device</dt><dd>{status.deviceName ?? "Registered automatically"}</dd>
          <dt>App version</dt><dd>{status.version}</dd>
        </dl>
        <label className="check">
          <input type="checkbox" checked={form.acceptInvalidCerts} onChange={(e) => set("acceptInvalidCerts", e.target.checked)} />
          Accept a self-signed HTTPS certificate
        </label>
        <div><button className="btn btn-danger" onClick={async () => onDisconnected(await api.logout())}>Disconnect</button></div>
      </section>}
      </TabPanel>

      <div className="savebar">
        <button className="btn btn-primary" disabled={!dirty || busy} onClick={save} data-testid="save-settings">{busy ? "Saving…" : "Save changes"}</button>
        {dirty && <button className="btn btn-ghost" onClick={() => setForm(saved)}>Discard</button>}
      </div>
    </div>
  );
}
