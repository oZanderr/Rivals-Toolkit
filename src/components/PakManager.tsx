import * as React from "react";
import { useState, useRef, useMemo, useCallback } from "react";

import { useVirtualizer } from "@tanstack/react-virtual";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  FolderOpen,
  PackageOpen,
  Download,
  Search,
  Package,
  PackagePlus,
  List,
  CheckCircle2,
  XCircle,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

type StatusType = "ok" | "err" | "info";

interface Props {
  gamePath: string;
}

function isUpdatePatchPak(pakPath: string): boolean {
  const name = pakPath.split(/[/\\]/).pop() ?? "";
  return /^Patch_.*\.pak$/i.test(name);
}

export function PakManager({ gamePath }: Props) {
  const [pakList, setPakList] = useState<string[]>([]);
  const [selectedPak, setSelectedPak] = useState<string>("");
  const [pakContents, setPakContents] = useState<string[]>([]);
  const [filterText, setFilterText] = useState("");
  const [notice, setNotice] = useState<{ msg: string; type: StatusType } | null>(null);
  const [busy, setBusy] = useState(false);
  const [manualPaks, setManualPaks] = useState<Set<string>>(new Set());
  const [debouncedFilter, setDebouncedFilter] = useState("");
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const filterTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const contentsScrollRef = useRef<HTMLDivElement>(null);

  const showNotice = (msg: string, type: StatusType, duration = 6000) => {
    // Strip any trailing path from error messages
    const clean =
      type === "err" ? msg.replace(/:\s*[A-Za-z]:\\[^\r\n]*|:\s*\/[^\r\n]*/g, "").trim() : msg;
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    setNotice({ msg: clean, type });
    noticeTimer.current = setTimeout(() => setNotice(null), duration);
  };

  // Pre-compute lowercase file names once when pakContents changes
  const pakContentsLower = useMemo(() => pakContents.map((f) => f.toLowerCase()), [pakContents]);

  // Filter against pre-lowered names, debounced input
  const visible = useMemo(() => {
    if (!debouncedFilter) return pakContents;
    const needle = debouncedFilter.toLowerCase();
    return pakContents.filter((_, i) => pakContentsLower[i].includes(needle));
  }, [pakContents, pakContentsLower, debouncedFilter]);

  // Debounce filter input (150ms)
  const onFilterChange = useCallback((value: string) => {
    setFilterText(value);
    if (filterTimerRef.current) clearTimeout(filterTimerRef.current);
    filterTimerRef.current = setTimeout(() => setDebouncedFilter(value), 150);
  }, []);

  // Virtualizer for the contents list
  const contentsVirtualizer = useVirtualizer({
    count: visible.length,
    getScrollElement: () => contentsScrollRef.current,
    estimateSize: () => 30,
    overscan: 20,
  });

  async function listPaks() {
    setNotice(null);
    if (noticeTimer.current) clearTimeout(noticeTimer.current);

    if (!gamePath) {
      showNotice("Set game root on Home tab first.", "err");
      return;
    }

    setBusy(true);
    try {
      const paks = await invoke<string[]>("list_pak_files", { gameRoot: gamePath });
      setPakList(paks);

      if (paks.length === 0) {
        setSelectedPak("");
        setPakContents([]);
        showNotice("No .pak files found.", "err");
      } else {
        showNotice(`${paks.length} pak${paks.length !== 1 ? "s" : ""} found`, "ok", 4000);
      }
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function inspectPak(pak: string) {
    if (pak === selectedPak) return;
    setSelectedPak(pak);
    setFilterText("");
    setDebouncedFilter("");
    setPakContents([]);

    if (isUpdatePatchPak(pak)) {
      showNotice("Update patch pak: launch Rivals once to apply it.", "info", 6000);
      return;
    }

    setBusy(true);
    try {
      const files = await invoke<string[]>("list_pak_contents", { pakPath: pak });
      setPakContents(files);
      showNotice(`${files.length} file(s) inside ${pak.split(/[/\\]/).pop()}`, "ok");
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function openPak() {
    const selected = await open({
      multiple: false,
      filters: [{ name: "Pak files", extensions: ["pak"] }],
    });
    if (typeof selected === "string") {
      setPakList((prev) => (prev.includes(selected) ? prev : [...prev, selected]));
      setManualPaks((prev) => new Set(prev).add(selected));
      await inspectPak(selected);
    }
  }

  async function unpackSelected() {
    if (!selectedPak) return;
    const dir = await open({ directory: true, multiple: false });
    if (!dir || typeof dir !== "string") return;

    // Auto-create a subfolder named after the pak (without extension)
    const pakBaseName =
      selectedPak
        .replace(/\\/g, "/")
        .split("/")
        .pop()
        ?.replace(/\.pak$/i, "") ?? "output";
    const outputDir = `${dir}\\${pakBaseName}`;

    setBusy(true);
    showNotice("Unpacking\u2026", "info");
    try {
      const files = await invoke<string[]>("unpack_pak", {
        pakPath: selectedPak,
        outputDir,
      });
      showNotice(`Extracted ${files.length} file(s) to ${outputDir}`, "ok");
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function extractSingleFile(filePath: string) {
    const outPath = await save({ defaultPath: filePath.split("/").pop() });
    if (!outPath) return;
    setBusy(true);
    try {
      await invoke("extract_single_file", {
        pakPath: selectedPak,
        fileName: filePath,
        outputPath: outPath,
      });
      showNotice(`Extracted: ${outPath}`, "ok");
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function openAndRepack() {
    const inputDir = await open({ directory: true, multiple: false });
    if (!inputDir || typeof inputDir !== "string") return;

    // Derive default pak name from folder name
    const folderName = inputDir.replace(/\\/g, "/").split("/").pop() ?? "mod_output";
    const baseName = folderName.replace(/_9999999_P$/i, "");
    const defaultPakName = `${baseName}_9999999_P.pak`;

    // Default save location: mods folder
    const modsDir = `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`;
    const defaultPath = `${modsDir}\\${defaultPakName}`;

    const outputPak = await save({
      defaultPath,
      filters: [{ name: "Pak files", extensions: ["pak"] }],
    });
    if (!outputPak) return;
    setBusy(true);
    showNotice("Repacking\u2026", "info");
    try {
      await invoke("repack_pak", { inputDir, outputPak });
      showNotice(`Repacked to: ${outputPak}`, "ok");
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  const pakName = selectedPak ? selectedPak.split(/[/\\]/).pop() : null;
  const selectedIsPatch = selectedPak ? isUpdatePatchPak(selectedPak) : false;

  return (
    <div className="flex flex-1 min-h-0 flex-col gap-4">
      <div className="flex min-h-8 shrink-0 items-center gap-3">
        <h2 className="shrink-0 text-xl font-bold">Pak Manager</h2>
        {notice && (
          <span
            className={cn(
              "flex min-w-0 items-center gap-1.5 truncate text-[12px] font-medium",
              notice.type === "ok"
                ? "text-[var(--color-ok)]"
                : notice.type === "err"
                  ? "text-[var(--color-err)]"
                  : "text-muted-foreground"
            )}
          >
            {notice.type === "ok" ? (
              <CheckCircle2 className="shrink-0" size={14} strokeWidth={2.5} />
            ) : notice.type === "err" ? (
              <XCircle className="shrink-0" size={14} strokeWidth={2.5} />
            ) : null}
            <span className="truncate">{notice.msg}</span>
          </span>
        )}
        <div className="ml-auto flex shrink-0 items-center gap-2">
          <Button variant="outline" size="sm" onClick={openPak} disabled={busy}>
            <Package size={15} />
            Open Pak
          </Button>
          <Button variant="blue" size="sm" onClick={openAndRepack} disabled={busy}>
            <PackageOpen size={15} />
            Repack Folder → Pak
          </Button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-row gap-4">
        <div className="flex min-h-0 w-[clamp(280px,28vw,560px)] min-w-[280px] max-w-[560px] shrink-0 flex-col">
          <Card className="flex min-h-0 flex-1 flex-col gap-3 p-3 bg-card">
            <div className="flex shrink-0 items-center justify-between">
              <h3 className="text-sm font-semibold">Game Paks</h3>
              <Button variant="outline" size="sm" onClick={listPaks} disabled={busy}>
                <List size={14} />
                List Game Paks
              </Button>
            </div>

            <p className="shrink-0 text-[11px] text-muted-foreground">
              {gamePath
                ? "Reads Marvel Rivals Paks folder from your game root."
                : "Set game root on Home tab to list game paks."}
            </p>

            <div className="min-h-0 flex-1 overflow-y-auto rounded-md border border-border bg-background">
              {pakList.length === 0 ? (
                <div className="flex h-full flex-col items-center justify-center gap-2 p-4 text-center">
                  <FolderOpen size={28} className="text-muted-foreground/40" />
                  <p className="text-sm text-muted-foreground">No paks loaded yet.</p>
                  <Button variant="outline" size="sm" onClick={openPak} disabled={busy}>
                    <Package size={14} />
                    Open Pak Manually…
                  </Button>
                </div>
              ) : (
                <ul>
                  {pakList.map((p) => {
                    const isSelected = selectedPak === p;
                    const isMod = /[/\\]~mods[/\\]/i.test(p);
                    const isManual = manualPaks.has(p);
                    const isPatch = isUpdatePatchPak(p);
                    const displayName = isMod
                      ? `~mods/${p.split(/[/\\]/).pop()}`
                      : p.split(/[/\\]/).pop();
                    return (
                      <li
                        key={p}
                        className={cn(
                          "flex items-center gap-2 border-b border-border/50 px-3 py-2 last:border-none",
                          isPatch ? "cursor-not-allowed opacity-65" : "cursor-pointer",
                          isSelected
                            ? "bg-secondary text-foreground"
                            : !isPatch && "hover:bg-secondary/50"
                        )}
                        onClick={() => !isPatch && inspectPak(p)}
                        title={
                          isPatch
                            ? "Update patch pak (delta): launch game once to apply"
                            : p.split(/[/\\]/).pop()
                        }
                      >
                        {isManual ? (
                          <PackagePlus size={14} className="shrink-0 text-sky-400" />
                        ) : (
                          <Package
                            size={14}
                            className={cn(
                              "shrink-0",
                              isMod ? "text-amber-400" : "text-muted-foreground"
                            )}
                          />
                        )}
                        <span className="flex-1 truncate text-[12px]">{displayName}</span>
                        {isPatch && (
                          <span className="shrink-0 rounded border border-amber-400/40 bg-amber-400/10 px-1.5 py-0.5 text-[10px] font-semibold text-amber-300">
                            update patch
                          </span>
                        )}
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
          </Card>
        </div>

        <Card className="flex min-h-0 min-w-0 flex-1 flex-col gap-3 p-3 bg-card">
          {/* Header — fixed two-line height so toggling subtitle doesn't shift */}
          <div className="flex shrink-0 items-center justify-between gap-2 overflow-hidden">
            <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
              <h3 className="truncate text-sm font-semibold" title={selectedPak || ""}>
                {pakName ?? "Contents"}
              </h3>
              {/* Always reserve subtitle line height; invisible when nothing selected */}
              <span
                className={cn(
                  "truncate text-[11px] text-muted-foreground",
                  !selectedPak && "invisible"
                )}
                title={selectedPak || ""}
              >
                {selectedPak || "\u00A0"}
              </span>
            </div>

            <Button
              variant="green"
              size="sm"
              onClick={unpackSelected}
              disabled={busy || !selectedPak || selectedIsPatch}
            >
              <Download size={14} />
              Extract All…
            </Button>
          </div>

          {/* Filter — always rendered */}
          <div className="relative shrink-0">
            <Search
              size={13}
              className="absolute left-2.5 top-1/2 -translate-y-1/2 text-muted-foreground"
            />
            <Input
              className="pl-7 font-mono text-xs"
              placeholder="Filter files…"
              value={filterText}
              onChange={(e) => onFilterChange(e.target.value)}
              disabled={!selectedPak || selectedIsPatch}
            />
          </div>

          {/* File list — always rendered */}
          <div
            ref={contentsScrollRef}
            className="min-h-0 flex-1 overflow-y-auto rounded-md border border-border bg-background"
          >
            {!selectedPak ? (
              <div className="flex h-full flex-col items-center justify-center gap-2 p-4 text-center">
                <PackageOpen size={28} className="text-muted-foreground/40" />
                <p className="text-sm text-muted-foreground">
                  Select a pak from the left, or open one manually.
                </p>
              </div>
            ) : selectedIsPatch ? (
              <div className="flex h-full flex-col items-center justify-center gap-2 p-4 text-center">
                <PackageOpen size={28} className="text-amber-300/70" />
                <p className="text-sm text-muted-foreground">
                  This is a game update patch pak (delta).
                </p>
                <p className="text-xs text-muted-foreground">
                  Launch Marvel Rivals once after updating, then inspect regular paks.
                </p>
              </div>
            ) : visible.length === 0 ? (
              <p className="p-4 text-center text-sm text-muted-foreground">
                {filterText ? "No matching files." : "No files found."}
              </p>
            ) : (
              <div
                style={{ height: `${contentsVirtualizer.getTotalSize()}px`, position: "relative" }}
              >
                {contentsVirtualizer.getVirtualItems().map((vRow) => {
                  const f = visible[vRow.index];
                  return (
                    <div
                      key={vRow.index}
                      className="absolute left-0 top-0 flex w-full cursor-pointer items-center gap-2 border-b border-border/50 px-3 hover:bg-secondary/50"
                      style={{
                        height: `${vRow.size}px`,
                        transform: `translateY(${vRow.start}px)`,
                      }}
                      onDoubleClick={() => extractSingleFile(f)}
                      title={f}
                    >
                      <span className="shrink-0 text-muted-foreground">{fileIcon(f)}</span>
                      <span className="truncate font-mono text-[11px]">{f}</span>
                    </div>
                  );
                })}
              </div>
            )}
          </div>

          {/* Footer — always rendered to avoid layout shift */}
          <p className="shrink-0 text-[11px] text-muted-foreground">
            {!selectedPak
              ? "\u00A0"
              : selectedIsPatch
                ? "Update patch paks are applied by the game and are not browseable here."
                : `${visible.length} file(s)${visible.length !== pakContents.length ? ` of ${pakContents.length}` : ""} inside ${pakName} — double-click a file to extract`}
          </p>
        </Card>
      </div>
    </div>
  );
}

function fileIcon(name: string): React.ReactNode {
  const ext = name.split(".").pop()?.toLowerCase();
  const cls = "shrink-0";
  switch (ext) {
    case "uasset":
      return <Package size={12} className={cn(cls, "text-blue-400")} />;
    case "umap":
      return <Package size={12} className={cn(cls, "text-purple-400")} />;
    case "png":
    case "jpg":
      return <Package size={12} className={cn(cls, "text-green-400")} />;
    case "wav":
    case "ogg":
      return <Package size={12} className={cn(cls, "text-yellow-400")} />;
    case "pak":
      return <Package size={12} className={cn(cls, "text-orange-400")} />;
    default:
      return <Package size={12} className={cn(cls, "text-muted-foreground")} />;
  }
}
