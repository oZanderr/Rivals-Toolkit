import * as React from "react";
import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  FolderOpen,
  RefreshCw,
  Shield,
  ShieldCheck,
  ShieldX,
  Package,
  CheckCircle2,
  XCircle,
  SlidersHorizontal,
  Wrench,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { PakTweaks } from "./PakTweaks";

interface ModsStatus {
  mods_folder_exists: boolean;
  mods_folder_path: string;
  sig_bypass_installed: boolean;
  mod_paks: string[];
}

interface Props {
  gamePath: string;
}

type StatusType = "ok" | "err" | "info";
type SubTab = "general" | "pak-config";

export function ModTools({ gamePath }: Props) {
  const [subTab, setSubTab] = useState<SubTab>("general");
  const [modsStatus, setModsStatus] = useState<ModsStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<{ msg: string; type: StatusType } | null>(null);
  const [showBadge, setShowBadge] = useState(false);
  const [badgeMsg, setBadgeMsg] = useState("");
  const badgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const showStatus = (msg: string, type: StatusType = "info") =>
    setStatus({ msg, type });

  const flashBadge = (msg: string) => {
    if (badgeTimer.current) clearTimeout(badgeTimer.current);
    setBadgeMsg(msg);
    setShowBadge(true);
    badgeTimer.current = setTimeout(() => setShowBadge(false), 4000);
  };

  useEffect(() => {
    if (gamePath) refresh();
  }, [gamePath]);

  async function refresh() {
    if (!gamePath) return;
    try {
      const s = await invoke<ModsStatus>("get_mods_status", { gameRoot: gamePath });
      setModsStatus(s);
    } catch (e: any) {
      showStatus(String(e), "err");
    }
  }

  async function installBypass() {
    if (!gamePath) return showStatus("Set game root on the Home tab first.", "err");
    setBusy(true);
    try {
      const msg = await invoke<string>("install_signature_bypass", { gameRoot: gamePath });
      flashBadge(msg);
      await refresh();
    } catch (e: any) {
      showStatus(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function openFolder() {
    if (!gamePath) return;
    try {
      await invoke("open_mods_folder", { gameRoot: gamePath });
    } catch (e: any) {
      showStatus(String(e), "err");
    }
  }

  return (
    <div className="flex w-full max-w-4xl flex-col gap-6">
      {/* Header */}
      <div className="flex items-center gap-3">
        <h2 className="text-xl font-bold">Mod Tools</h2>
        {showBadge && (
          <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-ok)]">
            <CheckCircle2 size={14} strokeWidth={2.5} />
            {badgeMsg}
          </span>
        )}
        {!gamePath && (
          <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-warn)]">
            <XCircle size={14} strokeWidth={2.5} />
            Set game root on Home tab first
          </span>
        )}
      </div>

      {/* Sub-tabs */}
      <div className="flex gap-1 rounded-md border border-border bg-card p-1">
        <button
          onClick={() => setSubTab("general")}
          className={cn(
            "flex items-center gap-1.5 rounded-sm px-3 py-1.5 text-[13px] text-muted-foreground transition-colors hover:text-foreground",
            subTab === "general" && "bg-secondary font-semibold text-foreground",
          )}
        >
          <Wrench size={13} />
          General
        </button>
        <button
          onClick={() => setSubTab("pak-config")}
          className={cn(
            "flex items-center gap-1.5 rounded-sm px-3 py-1.5 text-[13px] text-muted-foreground transition-colors hover:text-foreground",
            subTab === "pak-config" && "bg-secondary font-semibold text-foreground",
          )}
        >
          <SlidersHorizontal size={13} />
          PAK Config Editor
        </button>
      </div>

      {subTab === "pak-config" && (
        <PakTweaks gamePath={gamePath} />
      )}

      {subTab === "general" && (<>

      {/* Status cards */}
      <div className="grid gap-3 sm:grid-cols-3">
        <StatusCard
          label="~mods Folder"
          ok={modsStatus?.mods_folder_exists ?? false}
          loading={!modsStatus}
          okText="Exists"
          failText="Missing"
          okIcon={<FolderOpen size={14} />}
          failIcon={<FolderOpen size={14} />}
        />
        <StatusCard
          label="Signature Bypass"
          ok={modsStatus?.sig_bypass_installed ?? false}
          loading={!modsStatus}
          okText="dsound.dll present"
          failText="Not installed"
          okIcon={<ShieldCheck size={14} />}
          failIcon={<ShieldX size={14} />}
        />
        <Card className="flex flex-col gap-1 p-4 bg-card">
          <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
            Active Mods
          </span>
          <span className="text-2xl font-bold">
            {modsStatus ? modsStatus.mod_paks.length : "—"}
          </span>
          <span className="text-[11px] text-muted-foreground">PAK files in ~mods</span>
        </Card>
      </div>

      {/* Actions */}
      <Card className="flex flex-col gap-4 p-4 bg-card">
        <div>
          <h3 className="text-sm font-semibold">Signature Bypass</h3>
          <p className="mt-1 text-[12px] text-muted-foreground">
            Installs <Code>dsound.dll</Code> (ASI loader) and the bypass plugin into{" "}
            <Code>MarvelGame\Marvel\Binaries\Win64</Code>, and creates the <Code>~mods</Code> folder
            so the game loads unsigned PAKs.
          </p>
        </div>
        <div className="flex flex-wrap gap-2">
          <Button variant="green" size="sm" onClick={installBypass} disabled={!gamePath}>
            <Shield size={14} />
            Install Bypass
          </Button>
          <Button variant="outline" size="sm" onClick={openFolder} disabled={!gamePath}>
            <FolderOpen size={14} />
            Open mods Folder
          </Button>
          <Button variant="ghost" size="sm" onClick={refresh} disabled={!gamePath}>
            <RefreshCw size={14} />
            Refresh
          </Button>
        </div>
      </Card>

      {/* Installed mods list */}
      {modsStatus && modsStatus.mod_paks.length > 0 && (
        <Card className="flex flex-col gap-3 p-4 bg-card">
          <h3 className="text-sm font-semibold">Installed Mods</h3>
          <ul className="flex flex-col divide-y divide-border/50 rounded-md border border-border bg-background">
            {modsStatus.mod_paks.map((p) => (
              <li key={p} className="flex items-center gap-2 px-3 py-2">
                <Package size={13} className="shrink-0 text-muted-foreground" />
                <span className="truncate font-mono text-[12px]">{p}</span>
              </li>
            ))}
          </ul>
        </Card>
      )}

      {/* How-to */}
      <Card className="flex flex-col gap-3 p-4 bg-card">
        <h3 className="text-sm font-semibold">How to Install a Mod</h3>
        <ol className="flex flex-col gap-3 text-[12px] text-muted-foreground">
          {[
            <>
              Click <strong className="text-foreground">Install Bypass</strong> once. This places{" "}
              <Code>dsound.dll</Code> + <Code>plugins/bypass.asi</Code> in Binaries and creates the{" "}
              <Code>~mods</Code> folder.
            </>,
            <>
              Copy your mod <Code>.pak</Code> into the <Code>~mods</Code> folder. Rename it so it
              ends with <Code>_9999999_P.pak</Code> for correct load priority.
            </>,
            <>Launch Marvel Rivals — your mods will be active.</>,
          ].map((step, i) => (
            <li key={i} className="flex gap-2.5">
              <span className="flex size-5 shrink-0 items-center justify-center rounded-full bg-secondary text-[10px] font-bold text-foreground">
                {i + 1}
              </span>
              <span className="pt-0.5 leading-relaxed">{step}</span>
            </li>
          ))}
        </ol>
      </Card>

      {/* Status bar */}
      {status && (
        <p
          className={cn(
            "text-[12px]",
            status.type === "ok"
              ? "text-[var(--color-ok)]"
              : status.type === "err"
                ? "text-[var(--color-err)]"
                : "text-muted-foreground",
          )}
        >
          {status.msg}
        </p>
      )}

      </>)}
    </div>
  );
}

function StatusCard({
  label,
  ok,
  loading,
  okText,
  failText,
  okIcon,
  failIcon,
}: {
  label: string;
  ok: boolean;
  loading: boolean;
  okText: string;
  failText: string;
  okIcon: React.ReactNode;
  failIcon: React.ReactNode;
}) {
  return (
    <Card className="flex flex-col gap-1 p-4 bg-card">
      <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
        {label}
      </span>
      {loading ? (
        <span className="text-sm text-muted-foreground">—</span>
      ) : (
        <span
          className={cn(
            "flex items-center gap-1.5 text-sm font-medium",
            ok ? "text-[var(--color-ok)]" : "text-[var(--color-warn)]",
          )}
        >
          {ok ? okIcon : failIcon}
          {ok ? okText : failText}
        </span>
      )}
    </Card>
  );
}

function Code({ children }: { children: React.ReactNode }) {
  return (
    <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px] text-foreground">
      {children}
    </code>
  );
}
