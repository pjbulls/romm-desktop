import { useCallback, useEffect, useState } from "react";
import { api, errMsg, pickFile } from "../api";
import { useStore } from "../store";
import type { Bios, Emulator, Platform } from "../types";
import { PlatformChoice } from "./PlatformChoice";

export function Emulators({ platforms, onChanged }: { platforms: Platform[]; onChanged: () => void }) {
  const { push } = useStore();
  const [emus, setEmus] = useState<Emulator[] | null>(null);
  const [bios, setBios] = useState<Record<string, { busy: boolean; files?: Bios[]; error?: string }>>({});

  const load = useCallback(async () => {
    try {
      setEmus(await api.emulators());
    } catch (err) {
      push("error", "Couldn't load emulators", [errMsg(err)]);
    }
  }, [push]);
  useEffect(() => { load(); }, [load]);

  const changed = async () => { await load(); onChanged(); };

  // A pick is saved locally at once, but the platform list that carries it is refetched from the
  // RomM server afterwards. Show the pick meanwhile instead of snapping back to the old choice.
  const [picked, setPicked] = useState<Record<string, string>>({});
  useEffect(() => {
    setPicked((m) => {
      const done = Object.keys(m).filter((slug) => platforms.find((p) => p.slug === slug)?.selected === m[slug]);
      if (done.length === 0) return m;
      const next = { ...m };
      for (const slug of done) delete next[slug];
      return next;
    });
  }, [platforms]);

  async function choose(e: Emulator) {
    const p = await pickFile(`Choose the ${e.name} executable`);
    if (!p) return;
    try { await api.setEmulatorPath(e.id, p); await changed(); } catch (err) { push("error", `Couldn't use that file for ${e.name}`, [errMsg(err)]); }
  }

  async function reset(e: Emulator) {
    try { await api.setEmulatorPath(e.id, null); await changed(); } catch (err) { push("error", "Couldn't reset", [errMsg(err)]); }
  }

  async function mapPlatform(slug: string, value: string) {
    setPicked((m) => ({ ...m, [slug]: value }));
    try { await api.setPlatformChoice(slug, value || null); await changed(); } catch (err) {
      setPicked((m) => { const { [slug]: _dropped, ...rest } = m; return rest; });
      push("error", "Couldn't change the emulator", [errMsg(err)]);
    }
  }

  async function installBios(slug: string) {
    setBios((b) => ({ ...b, [slug]: { busy: true } }));
    try {
      const files = await api.installBios(slug);
      setBios((b) => ({ ...b, [slug]: { busy: false, files } }));
    } catch (err) {
      setBios((b) => ({ ...b, [slug]: { busy: false, error: errMsg(err) } }));
    }
  }

  if (!emus) return <div className="loading-row"><span className="spinner sm" /> Loading…</div>;
  const withBios = platforms.filter((p) => p.firmwareCount > 0 && p.emulator);
  const mappable = platforms.filter((p) => p.romCount > 0).sort((a, b) => a.name.localeCompare(b.name));

  return (
    <div className="page">
      <header className="topbar"><h1>Emulators &amp; BIOS</h1></header>
      <p className="muted lead">RomM Desktop finds installed emulators automatically (including Flatpak on Linux). If one isn't found, point it at the executable.</p>

      <section className="stack">
        {emus.map((e) => (
          <div className="card emu" key={e.id} data-testid={`emu-${e.id}`}>
            <div className="emu-head">
              <h3>{e.name}</h3>
              <span className={`chip chip-${e.installed ? "ok" : "warn"}`}>{e.installed ? (e.flatpak ? "Found (Flatpak)" : "Found") : "Not found"}</span>
            </div>
            {e.installed ? (
              <dl className="kv">
                <dt>Runs</dt><dd className="mono">{e.path}</dd>
                <dt>Data folder</dt><dd className="mono">{e.dataDir}</dd>
              </dl>
            ) : <p className="muted small">{e.problem}</p>}
            <div className="emu-platforms">
              {e.platforms.slice(0, 14).map((s) => <span className="chip chip-muted" key={s}>{s}</span>)}
              {e.platforms.length > 14 && <span className="muted small">+{e.platforms.length - 14} more</span>}
            </div>
            <div className="actions tight">
              <button className="btn" onClick={() => choose(e)}>Choose executable…</button>
              {e.overridePath && <button className="btn btn-ghost" onClick={() => reset(e)}>Use auto-detect</button>}
            </div>
          </div>
        ))}
      </section>

      <section>
        <h2>BIOS files</h2>
        {withBios.length === 0 ? (
          <p className="muted">None of your platforms have BIOS files on the server. Add them to a platform's <code>bios</code> folder in RomM and rescan.</p>
        ) : (
          <div className="card">
            <ul className="rows">
              {withBios.map((p) => (
                <li key={p.id} className="col">
                  <div className="row-flex">
                    <div className="row-main"><span className="row-name">{p.name}</span><span className="muted small">{p.firmwareCount} file{p.firmwareCount === 1 ? "" : "s"} → {p.emulatorName}</span></div>
                    <button className="btn" disabled={bios[p.slug]?.busy || !p.emulatorInstalled} onClick={() => installBios(p.slug)} title={!p.emulatorInstalled ? `${p.emulatorName} isn't installed` : undefined} data-testid={`bios-${p.slug}`}>
                      {bios[p.slug]?.busy ? "Downloading…" : "Install BIOS"}
                    </button>
                  </div>
                  {bios[p.slug]?.error && <div className="banner banner-error">{bios[p.slug]!.error}</div>}
                  {bios[p.slug]?.files && <ul className="bios-files">{bios[p.slug]!.files!.map((f) => <li key={f.file}><span className="dot dot-ok" /> {f.file} <span className="muted small">{f.status}</span></li>)}</ul>}
                </li>
              ))}
            </ul>
          </div>
        )}
      </section>

      <section>
        <h2>Which emulator plays what</h2>
        <div className="card">
          <ul className="rows">
            {mappable.map((p) => <PlatformChoice key={p.id} platform={p.slug in picked ? { ...p, selected: picked[p.slug] } : p} onChange={mapPlatform} />)}
          </ul>
        </div>
      </section>
    </div>
  );
}
