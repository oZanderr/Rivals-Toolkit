import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

interface InstallInfo {
  found: boolean;
  path: string;
  source: string;
}

interface Props {
  gamePath: string;
  setGamePath: (p: string) => void;
}

export function Home({ gamePath, setGamePath }: Props) {
  const [info, setInfo] = useState<InstallInfo | null>(null);
  const [detecting, setDetecting] = useState(false);

  async function detect() {
    setDetecting(true);
    try {
      const result = await invoke<InstallInfo>("detect_install_path");
      setInfo(result);
      if (result.found) setGamePath(result.path);
    } catch (e) {
      console.error(e);
    } finally {
      setDetecting(false);
    }
  }

  async function browse() {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") setGamePath(selected);
  }

  useEffect(() => {
    if (!gamePath) detect();
  }, []);

  return (
    <div className="panel">
      <h2 className="panel-title">Installation</h2>

      {info && (
        <div className={`status-badge ${info.found ? "ok" : "warn"}`}>
          {info.found
            ? `✓ Found via ${info.source}`
            : "✗ Auto-detection failed — set path manually"}
        </div>
      )}

      <div className="field-row">
        <label>Game Root</label>
        <div className="path-row">
          <input
            className="path-input"
            value={gamePath}
            onChange={(e) => setGamePath(e.target.value)}
            placeholder="e.g. C:\Program Files (x86)\Steam\steamapps\common\MarvelRivals"
          />
          <button onClick={browse} className="btn-secondary">Browse…</button>
        </div>
      </div>

      <button onClick={detect} className="btn-primary" disabled={detecting}>
        {detecting ? "Detecting…" : "Re-detect Installation"}
      </button>

      {gamePath && (
        <div className="info-grid">
          <div className="info-card">
            <span className="info-label">Paks Folder</span>
            <span className="info-value path-chip">
              {gamePath}\MarvelGame\Marvel\Content\Paks
            </span>
          </div>
          <div className="info-card">
            <span className="info-label">Mods Folder</span>
            <span className="info-value path-chip">
              {gamePath}\MarvelGame\Marvel\Content\Paks\~mods
            </span>
          </div>
        </div>
      )}
    </div>
  );
}
