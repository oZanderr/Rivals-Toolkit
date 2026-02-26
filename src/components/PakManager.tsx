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
  const [status, setStatus] = useState<{ msg: string; type: StatusType } | null>(null);
  const [busy, setBusy] = useState(false);
  const [showPaksBadge, setShowPaksBadge] = useState(false);
  const [paksFoundCount, setPaksFoundCount] = useState(0);
  const [listError, setListError] = useState<string | null>(null);
  const [debouncedFilter, setDebouncedFilter] = useState("");
  const paksBadgeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const filterTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const contentsScrollRef = useRef<HTMLDivElement>(null);

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

  const showStatus = (msg: string, type: StatusType = "info") =>
    setStatus({ msg, type });

  async function listPaks() {
    if (paksBadgeTimer.current) clearTimeout(paksBadgeTimer.current);
    setShowPaksBadge(false);
    setListError(null);

    if (!gamePath) {
      setListError("Set game root on Home tab first.");
      return;
    }

    setBusy(true);
    try {
      const paks = await invoke<string[]>("list_pak_files", { gameRoot: gamePath });
      setPakList(paks);

      if (paks.length === 0) {
        setSelectedPak("");
        setPakContents([]);
        setListError("No .pak files found.");
      } else {
        setPaksFoundCount(paks.length);
        setShowPaksBadge(true);
        paksBadgeTimer.current = setTimeout(() => setShowPaksBadge(false), 4000);
      }
    } catch (e: any) {
      setListError(String(e));
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
      showStatus(`${files.length} file(s) inside ${pak.split(/[/\\]/).pop()}`, "ok");
    } catch (e: any) {
      showStatus(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function openPak() {
    const selected = await open({
      multiple: false,
      filters: [{ name: "PAK files", extensions: ["pak"] }],
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
    setBusy(true);
    showStatus("Unpacking…", "info");
    try {
      const files = await invoke<string[]>("unpack_pak", {
        pakPath: selectedPak,
        outputDir: dir,
      });
      showStatus(`Extracted ${files.length} file(s) to ${dir}`, "ok");
    } catch (e: any) {
      showStatus(String(e), "err");
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
      showStatus(`Extracted: ${outPath}`, "ok");
    } catch (e: any) {
      showStatus(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function openAndRepack() {
    const inputDir = await open({ directory: true, multiple: false });
    if (!inputDir || typeof inputDir !== "string") return;
    const outputPak = await save({
      defaultPath: "mod_output.pak",
      filters: [{ name: "PAK files", extensions: ["pak"] }],
    });
    if (!outputPak) return;
    setBusy(true);
    showStatus("Repacking…", "info");
    try {
      await invoke("repack_pak", { inputDir, outputPak });
      showStatus(`Repacked to: ${outputPak}`, "ok");
    } catch (e: any) {
      showStatus(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  const pakName = selectedPak ? selectedPak.split(/[/\\]/).pop() : null;

  return (
    <div className="flex h-full flex-col gap-4">
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-3">
          <h2 className="text-xl font-bold">PAK Manager</h2>
          {showPaksBadge && (
            <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-ok)]">
              <CheckCircle2 size={14} strokeWidth={2.5} />
              {paksFoundCount} PAK{paksFoundCount !== 1 ? "s" : ""} found
            </span>
          )}
          {listError && (
            <span className="flex items-center gap-1.5 text-[12px] font-medium text-[var(--color-err)]">
              <XCircle size={14} strokeWidth={2.5} />
              {listError}
            </span>
          )}
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <Button variant="outline" size="sm" onClick={openPak} disabled={busy}>
            <Package size={15} />
            Open PAK
          </Button>
          <Button variant="blue" size="sm" onClick={openAndRepack} disabled={busy}>
            <PackageOpen size={15} />
            Repack Folder → PAK
          </Button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 gap-4">
        <Card className="flex w-[300px] shrink-0 min-h-0 flex-col gap-3 p-3 bg-card">
          <div className="flex shrink-0 items-center justify-between">
            <h3 className="text-sm font-semibold">Game PAKs</h3>
            <Button variant="blue" size="sm" onClick={listPaks} disabled={busy}>
              <List size={14} />
              List Game PAKs
            </Button>
          </div>

          <p className="shrink-0 text-[11px] text-muted-foreground">
            {gamePath ? "Reads Marvel Rivals Paks folder from your game root." : "Set game root on Home tab to list game PAKs."}
          </p>

          <div className="min-h-0 flex-1 overflow-y-auto rounded-md border border-border bg-background">
            {pakList.length === 0 ? (
              <div className="flex h-full flex-col items-center justify-center gap-2 p-4 text-center">
                <FolderOpen size={28} className="text-muted-foreground/40" />
                <p className="text-sm text-muted-foreground">No PAKs loaded yet.</p>
                <Button variant="outline" size="sm" onClick={openPak} disabled={busy}>
                  <Package size={14} />
                  Open PAK Manually…
                </Button>
              </div>
            ) : (
              <ul>
                {pakList.map((p) => {
                  const isSelected = selectedPak === p;
                  return (
                    <li
                      key={p}
                      className={cn(
                        "flex cursor-pointer items-center gap-2 border-b border-border/50 px-3 py-2 last:border-none",
                        isSelected ? "bg-secondary text-foreground" : "hover:bg-secondary/50",
                      )}
                      onClick={() => inspectPak(p)}
                      title={p}
                    >
                      <Package size={14} className="shrink-0 text-muted-foreground" />
                      <span className="truncate text-[12px]">{p.split(/[/\\]/).pop()}</span>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
        </Card>

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
                <p className="text-sm text-muted-foreground">Select a PAK from the left, or open one manually.</p>
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
                      title="Double-click to extract"
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
          <p className={cn(
            "shrink-0 text-[11px]",
            !selectedPak ? "text-muted-foreground" :
            status ? (
              status.type === "ok" ? "text-[var(--color-ok)]" :
              status.type === "err" ? "text-[var(--color-err)]" :
              "text-muted-foreground"
            ) : "text-muted-foreground"
          )}>
            {!selectedPak
              ? "\u00A0"
              : status
                ? status.msg
                : `${visible.length} of ${pakContents.length} file(s) — double-click a file to extract it`}
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
