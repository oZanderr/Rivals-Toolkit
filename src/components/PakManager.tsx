import * as React from "react";
import { useState, useRef, useMemo, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { useVirtualizer } from "@tanstack/react-virtual";
import { FolderOpen, PackageOpen, Download, Search, Package, List, CheckCircle2, XCircle } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card } from "@/components/ui/card";
import { cn } from "@/lib/utils";

type StatusType = "ok" | "err" | "info";

interface Props {
  gamePath: string;
}



export function PakManager({ gamePath }: Props) {
  const [pakList, setPakList] = useState<string[]>([]);
  const [selectedPak, setSelectedPak] = useState<string>("");
  const [pakContents, setPakContents] = useState<string[]>([]);
  const [filterText, setFilterText] = useState("");
  const [notice, setNotice] = useState<{ msg: string; type: StatusType } | null>(null);
  const [busy, setBusy] = useState(false);
  const [debouncedFilter, setDebouncedFilter] = useState("");
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const filterTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const contentsScrollRef = useRef<HTMLDivElement>(null);

  const showNotice = (msg: string, type: StatusType, duration = 6000) => {
    // Strip any trailing path from error messages
    const clean = type === "err"
      ? msg.replace(/:\s*[A-Za-z]:\\[^\r\n]*|:\s*\/[^\r\n]*/g, "").trim()
      : msg;
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    setNotice({ msg: clean, type });
    noticeTimer.current = setTimeout(() => setNotice(null), duration);
  };

  // Pre-compute lowercase file names once when pakContents changes
  const pakContentsLower = useMemo(
    () => pakContents.map((f) => f.toLowerCase()),
    [pakContents],
  );

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
    } catch (e: any) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function inspectPak(pak: string) {
    setSelectedPak(pak);
    setFilterText("");
    setBusy(true);
    try {
      const files = await invoke<string[]>("list_pak_contents", { pakPath: pak });
      setPakContents(files);
      showNotice(`${files.length} file(s) inside ${pak.split(/[/\\]/).pop()}`, "ok");
    } catch (e: any) {
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
      setPakList((prev) => (prev.includes(selected) ? prev : [selected, ...prev]));
      await inspectPak(selected);
    }
  }

  async function unpackSelected() {
    if (!selectedPak) return;
    const dir = await open({ directory: true, multiple: false });
    if (!dir || typeof dir !== "string") return;

    // Auto-create a subfolder named after the pak (without extension)
    const pakBaseName = selectedPak.replace(/\\/g, "/").split("/").pop()?.replace(/\.pak$/i, "") ?? "output";
    const outputDir = `${dir}\\${pakBaseName}`;

    setBusy(true);
    showNotice("Unpacking\u2026", "info");
    try {
      const files = await invoke<string[]>("unpack_pak", {
        pakPath: selectedPak,
        outputDir,
      });
      showNotice(`Extracted ${files.length} file(s) to ${outputDir}`, "ok");
    } catch (e: any) {
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
    } catch (e: any) {
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
    } catch (e: any) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  const pakName = selectedPak ? selectedPak.split(/[/\\]/).pop() : null;

  return (
    <div className="flex h-full flex-col gap-4">
      <div className="flex shrink-0 items-center gap-3">
        <h2 className="text-xl font-bold">Pak Manager</h2>
        {notice && (
          <span
            className={cn(
              "flex items-center gap-1.5 text-[12px] font-medium",
              notice.type === "ok"
                ? "text-[var(--color-ok)]"
                : notice.type === "err"
                  ? "text-[var(--color-err)]"
                  : "text-muted-foreground",
            )}
          >
            {notice.type === "ok" ? (
              <CheckCircle2 size={14} strokeWidth={2.5} />
            ) : notice.type === "err" ? (
              <XCircle size={14} strokeWidth={2.5} />
            ) : null}
            {notice.msg}
          </span>
        )}
      </div>

      <div className="flex min-h-0 flex-1 gap-4">
        <div className="flex w-[320px] shrink-0 flex-col gap-2 min-h-0">
          <div className="flex shrink-0 items-center gap-2">
            <Button className="flex-1" variant="outline" size="sm" onClick={openPak} disabled={busy}>
              <Package size={15} />
              Open Pak
            </Button>
            <Button className="flex-1" variant="blue" size="sm" onClick={openAndRepack} disabled={busy}>
              <PackageOpen size={15} />
              Repack Folder → Pak
            </Button>
          </div>

        <Card className="flex min-h-0 flex-1 flex-col gap-3 p-3 bg-card">
          <div className="flex shrink-0 items-center justify-between">
            <h3 className="text-sm font-semibold">Game Paks</h3>
            <Button variant="outline" size="sm" onClick={listPaks} disabled={busy}>
              <List size={14} />
              List Game Paks
            </Button>
          </div>

          <p className="shrink-0 text-[11px] text-muted-foreground">
            {gamePath ? "Reads Marvel Rivals Paks folder from your game root." : "Set game root on Home tab to list game paks."}
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
                  const displayName = isMod
                    ? `~mods/${p.split(/[/\\]/).pop()}`
                    : p.split(/[/\\]/).pop();
                  return (
                    <li
                      key={p}
                      className={cn(
                        "flex cursor-pointer items-center gap-2 border-b border-border/50 px-3 py-2 last:border-none",
                        isSelected ? "bg-secondary text-foreground" : "hover:bg-secondary/50",
                      )}
                      onClick={() => inspectPak(p)}
                      title={displayName}
                    >
                      <Package
                        size={14}
                        className={cn("shrink-0", isMod ? "text-amber-400" : "text-muted-foreground")}
                      />
                      <span className="truncate text-[12px]">{displayName}</span>
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
              <h3 className="truncate text-sm font-semibold" title={selectedPak || ""}>{pakName ?? "Contents"}</h3>
              {/* Always reserve subtitle line height; invisible when nothing selected */}
              <span
                className={cn(
                  "truncate text-[11px] text-muted-foreground",
                  !selectedPak && "invisible",
                )}
                title={selectedPak || ""}
              >
                {selectedPak || "\u00A0"}
              </span>
            </div>

              <Button variant="green" size="sm" onClick={unpackSelected} disabled={busy || !selectedPak}>
              <Download size={14} />
              Extract All…
            </Button>
          </div>

          {/* Filter — always rendered */}
          <div className="relative shrink-0">
            <Search size={13} className="absolute left-2.5 top-1/2 -translate-y-1/2 text-muted-foreground" />
            <Input
              className="pl-7 font-mono text-xs"
              placeholder="Filter files…"
              value={filterText}
              onChange={(e) => onFilterChange(e.target.value)}
              disabled={!selectedPak}
            />
          </div>

          {/* File list — always rendered */}
          <div ref={contentsScrollRef} className="min-h-0 flex-1 overflow-y-auto rounded-md border border-border bg-background">
            {!selectedPak ? (
              <div className="flex h-full flex-col items-center justify-center gap-2 p-4 text-center">
                <PackageOpen size={28} className="text-muted-foreground/40" />
                <p className="text-sm text-muted-foreground">Select a pak from the left, or open one manually.</p>
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
    case "uasset": return <Package size={12} className={cn(cls, "text-blue-400")} />;
    case "umap":   return <Package size={12} className={cn(cls, "text-purple-400")} />;
    case "png":
    case "jpg":    return <Package size={12} className={cn(cls, "text-green-400")} />;
    case "wav":
    case "ogg":    return <Package size={12} className={cn(cls, "text-yellow-400")} />;
    case "pak":    return <Package size={12} className={cn(cls, "text-orange-400")} />;
    default:       return <Package size={12} className={cn(cls, "text-muted-foreground")} />;
  }
}
