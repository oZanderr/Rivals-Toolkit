import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import { FolderOpen, RefreshCw, CheckCircle2, XCircle } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card } from "@/components/ui/card";
import { cn } from "@/lib/utils";

interface InstallInfo {
  path: string;
  source: string;
}

interface Props {
  gamePath: string;
  setGamePath: (p: string) => void;
}

export function Home({ gamePath, setGamePath }: Props) {
  const [info, setInfo] = useState<InstallInfo | null | undefined>(undefined);
  const [detecting, setDetecting] = useState(false);
  const [showBadge, setShowBadge] = useState(false);
  const badgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  async function detect() {
    if (badgeTimer.current) clearTimeout(badgeTimer.current);
    setDetecting(true);
    setShowBadge(false);
    try {
      const result = await invoke<InstallInfo | null>("detect_install_path");
      setInfo(result);
      if (result) {
        setGamePath(result.path);
        setShowBadge(true);
        badgeTimer.current = setTimeout(() => setShowBadge(false), 4000);
      }
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
    <div className="flex w-full max-w-4xl flex-col gap-6">
      <div className="flex items-center gap-3">
        <h2 className="text-xl font-bold">Installation</h2>
        {showBadge && info && (
          <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-ok)]">
            <CheckCircle2 size={14} strokeWidth={2.5} />
            Found via {info.source}
          </span>
        )}
        {info === null && (
          <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-warn)]">
            <XCircle size={14} strokeWidth={2.5} />
            Not detected
          </span>
        )}
      </div>

      <div className="flex flex-col gap-1.5">
        <label className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
          Game Root
        </label>
        <div className="flex gap-2">
          <Input
            value={gamePath}
            onChange={(e) => setGamePath(e.target.value)}
            placeholder={`e.g. C:\\Program Files (x86)\\Steam\\steamapps\\common\\MarvelRivals`}
            title={gamePath}
            className="flex-1 font-mono text-xs"
          />
          <Button variant="outline" onClick={browse} className="shrink-0">
            <FolderOpen size={15} />
            Browse
          </Button>
          <Button onClick={detect} disabled={detecting} variant="blue" className="shrink-0">
            <RefreshCw size={15} className={cn(detecting && "animate-spin")} />
            Redetect
          </Button>
        </div>
      </div>

      {gamePath && (
        <div className="grid gap-3 sm:grid-cols-2">
          <Card className="bg-card">
            <div className="flex flex-col gap-2 p-4">
              <div className="flex items-center justify-between">
                <span className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Paks Folder</span>
                <Button variant="ghost" size="icon" className="h-5 w-5 text-muted-foreground hover:text-foreground" onClick={() => openPath(`${gamePath}\\MarvelGame\\Marvel\\Content\\Paks`)}>
                  <FolderOpen size={12} />
                </Button>
              </div>
              <code className="break-all text-[11px] text-foreground">
                {gamePath}\MarvelGame\Marvel\Content\Paks
              </code>
            </div>
          </Card>
          <Card className="bg-card">
            <div className="flex flex-col gap-2 p-4">
              <div className="flex items-center justify-between">
                <span className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Mods Folder</span>
                <Button variant="ghost" size="icon" className="h-5 w-5 text-muted-foreground hover:text-foreground" onClick={() => openPath(`${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`)}>
                  <FolderOpen size={12} />
                </Button>
              </div>
              <code className="break-all text-[11px] text-foreground">
                {gamePath}\MarvelGame\Marvel\Content\Paks\~mods
              </code>
            </div>
          </Card>
        </div>
      )}
    </div>
  );
}
