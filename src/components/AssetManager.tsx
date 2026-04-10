import * as React from "react";
import { useState, useRef, useMemo, useCallback } from "react";

import { useVirtualizer } from "@tanstack/react-virtual";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  FolderOpen,
  PackageOpen,
  Download,
  FileOutput,
  Search,
  Package,
  PackagePlus,
  Layers,
  List,
  CheckCircle2,
  XCircle,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";

type RepackFormat = "pak" | "iostore";
type StatusType = "ok" | "err" | "info";

interface PakFileInfo {
  path: string;
  has_utoc: boolean;
  has_ucas: boolean;
}

type ContentSource = "pak" | "utoc";

interface ContentEntry {
  path: string;
  source: ContentSource;
}

interface Props {
  gamePath: string;
}

export function AssetManager({ gamePath }: Props) {
  const [pakList, setPakList] = useState<PakFileInfo[]>([]);
  const [selectedPak, setSelectedPak] = useState<string>("");
  const [pakContents, setPakContents] = useState<ContentEntry[]>([]);
  const [filterText, setFilterText] = useState("");
  const [notice, setNotice] = useState<{ msg: string; type: StatusType } | null>(null);
  const [busy, setBusy] = useState(false);
  const [manualPaks, setManualPaks] = useState<Set<string>>(new Set());
  const [debouncedFilter, setDebouncedFilter] = useState("");
  const [repackFormat, setRepackFormat] = useState<RepackFormat>("pak");
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const filterTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const contentsScrollRef = useRef<HTMLDivElement>(null);

  const showNotice = (msg: string, type: StatusType, duration?: number) => {
    // Strip any trailing path from error messages
    const clean =
      type === "err" ? msg.replace(/:\s*[A-Za-z]:\\[^\r\n]*|:\s*\/[^\r\n]*/g, "").trim() : msg;
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    setNotice({ msg: clean, type });
    // "info" notices persist until replaced by another notice (no auto-dismiss)
    const ms = duration ?? (type === "info" ? 0 : 6000);
    if (ms > 0) {
      noticeTimer.current = setTimeout(() => setNotice(null), ms);
    }
  };

  // Pre-compute lowercase paths once when pakContents changes
  const pakContentsLower = useMemo(
    () => pakContents.map((e) => e.path.toLowerCase()),
    [pakContents]
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

  const selectedIsIoStore = useMemo(() => {
    const info = pakList.find((p) => p.path === selectedPak);
    return !!(info?.has_utoc && info?.has_ucas);
  }, [pakList, selectedPak]);

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
      const paks = await invoke<PakFileInfo[]>("list_pak_files_info", { gameRoot: gamePath });
      setPakList(paks);

      if (paks.length === 0) {
        setSelectedPak("");
        setPakContents([]);
        showNotice("No .pak files found.", "err");
      } else {
        if (selectedPak && !paks.some((p) => p.path === selectedPak)) {
          setSelectedPak("");
          setPakContents([]);
          setFilterText("");
          setDebouncedFilter("");
        }
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

    const info = pakList.find((p) => p.path === pak);
    const isIoStore = info?.has_utoc && info?.has_ucas;

    setBusy(true);
    try {
      const utocPath = isIoStore ? pak.replace(/\.pak$/i, ".utoc") : "";
      const [pakFiles, utocResult] = await Promise.all([
        invoke<string[]>("list_pak_contents", { pakPath: pak }),
        isIoStore
          ? invoke<string[]>("list_utoc_contents", { utocPath }).then(
              (files) => ({ ok: true as const, files }),
              (err) => ({ ok: false as const, err: String(err) })
            )
          : Promise.resolve({ ok: true as const, files: [] as string[] }),
      ]);

      const entries: ContentEntry[] = [];
      for (const f of pakFiles) {
        if (isIoStore && f === "chunknames") continue;
        entries.push({ path: f, source: "pak" });
      }
      if (utocResult.ok) {
        for (const f of utocResult.files) {
          entries.push({ path: f, source: "utoc" });
        }
      }

      setPakContents(entries);
      const displayName = pak.split(/[/\\]/).pop();
      if (!utocResult.ok) {
        showNotice(`${entries.length} file(s) inside ${displayName} (utoc failed to load)`, "err");
      } else {
        showNotice(`${entries.length} file(s) inside ${displayName}`, "ok");
      }
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
      const [hasUtoc, hasUcas] = await Promise.all([
        invoke<boolean>("path_exists", { path: selected.replace(/\.pak$/i, ".utoc") }),
        invoke<boolean>("path_exists", { path: selected.replace(/\.pak$/i, ".ucas") }),
      ]);
      setPakList((prev) => {
        if (prev.some((p) => p.path === selected)) return prev;
        return [...prev, { path: selected, has_utoc: hasUtoc, has_ucas: hasUcas }];
      });
      setManualPaks((prev) => new Set(prev).add(selected));
      await inspectPak(selected);
    }
  }

  async function unpackSelected() {
    if (!selectedPak) return;
    const dir = await open({ directory: true, multiple: false });
    if (!dir || typeof dir !== "string") return;

    const pakBaseName =
      selectedPak
        .replace(/\\/g, "/")
        .split("/")
        .pop()
        ?.replace(/\.pak$/i, "") ?? "output";
    const outputDir = `${dir}\\${pakBaseName}`;

    const info = pakList.find((p) => p.path === selectedPak);
    const isIoStore = info?.has_utoc && info?.has_ucas;

    setBusy(true);
    showNotice("Unpacking\u2026", "info");
    try {
      let totalFiles = 0;

      if (isIoStore) {
        // IoStore: extract raw chunks from utoc + pak contents (excluding chunknames)
        const utocPath = selectedPak.replace(/\.pak$/i, ".utoc");
        const utocFiles = await invoke<string[]>("extract_utoc", {
          utocPath,
          outputDir,
        });
        totalFiles += utocFiles.length;
      }

      // Extract pak contents (skip chunknames for IoStore companions)
      const pakFiles = await invoke<string[]>("unpack_pak", {
        pakPath: selectedPak,
        outputDir,
        skip: isIoStore ? ["chunknames"] : [],
      });
      totalFiles += pakFiles.length;

      showNotice(`Extracted ${totalFiles} file(s) to ${outputDir}`, "ok");
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function extractSingleEntry(entry: ContentEntry) {
    const outPath = await save({ defaultPath: entry.path.split("/").pop() });
    if (!outPath) return;
    setBusy(true);
    try {
      if (entry.source === "utoc") {
        const utocPath = selectedPak.replace(/\.pak$/i, ".utoc");
        await invoke("extract_utoc_file", {
          utocPath,
          fileName: entry.path,
          outputPath: outPath,
        });
      } else {
        await invoke("extract_single_file", {
          pakPath: selectedPak,
          fileName: entry.path,
          outputPath: outPath,
        });
      }
      showNotice(`Extracted: ${outPath}`, "ok");
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function exportLegacy() {
    if (!selectedPak) return;
    const dir = await open({ directory: true, multiple: false });
    if (!dir || typeof dir !== "string") return;

    const pakBaseName =
      selectedPak
        .replace(/\\/g, "/")
        .split("/")
        .pop()
        ?.replace(/\.pak$/i, "") ?? "output";
    const outputDir = `${dir}\\${pakBaseName}_legacy`;
    const utocPath = selectedPak.replace(/\.pak$/i, ".utoc");

    setBusy(true);
    showNotice("Converting to legacy format\u2026 (this may take a while)", "info");
    try {
      const files = await invoke<string[]>("extract_utoc_legacy", {
        utocPath,
        gameRoot: gamePath,
        outputDir,
        filter: [],
      });
      const warning = files.find((f) => f.startsWith("__warnings__:"));
      const exported = files.filter((f) => !f.startsWith("__warnings__:"));
      if (warning) {
        showNotice(`Exported ${exported.length} asset(s) (some failed to convert)`, "err");
      } else {
        showNotice(`Exported ${exported.length} asset(s) to legacy format`, "ok");
      }
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function openAndRepack() {
    const inputDir = await open({ directory: true, multiple: false });
    if (!inputDir || typeof inputDir !== "string") return;

    // Derive default output name from folder name
    const folderName = inputDir.replace(/\\/g, "/").split("/").pop() ?? "mod_output";
    const baseName = folderName.replace(/_9999999_P$/i, "");
    const modsDir = `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`;

    if (repackFormat === "iostore") {
      // IoStore: output is .utoc (companion .ucas and .pak are created automatically)
      const defaultPath = `${modsDir}\\${baseName}_9999999_P.utoc`;
      const outputUtoc = await save({
        defaultPath,
        filters: [{ name: "IoStore container", extensions: ["utoc"] }],
      });
      if (!outputUtoc) return;
      setBusy(true);
      showNotice("Repacking to IoStore\u2026", "info");
      try {
        await invoke("repack_iostore", { inputDir, outputUtoc });
        showNotice(`Repacked IoStore to: ${outputUtoc}`, "ok");
      } catch (e: unknown) {
        showNotice(String(e), "err");
      } finally {
        setBusy(false);
      }
    } else {
      // Pak: standard pak repack
      const defaultPath = `${modsDir}\\${baseName}_9999999_P.pak`;
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
  }

  const pakName = selectedPak ? selectedPak.split(/[/\\]/).pop() : null;
  const footerText = !selectedPak
    ? "\u00A0"
    : `${visible.length} file(s)${visible.length !== pakContents.length ? ` of ${pakContents.length}` : ""} inside ${pakName} \u2014 double-click a file to extract`;

  return (
    <div className="flex flex-1 min-h-0 flex-col gap-4">
      <div className="flex min-h-8 shrink-0 items-center gap-3">
        <h2 className="shrink-0 text-xl font-bold">Asset Manager</h2>
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
          <div className="flex items-center">
            <Select value={repackFormat} onValueChange={(v) => setRepackFormat(v as RepackFormat)}>
              <SelectTrigger
                size="sm"
                className="h-8 w-[120px] rounded-r-none border-r-0 text-sm font-medium"
              >
                <SelectValue>
                  <span className="flex items-center gap-1.5">
                    {repackFormat === "pak" ? (
                      <Package size={15} strokeWidth={2} className="text-foreground" />
                    ) : (
                      <Layers size={15} strokeWidth={2} className="text-foreground" />
                    )}
                    {repackFormat === "pak" ? "Pak" : "IoStore"}
                  </span>
                </SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="pak">
                  <Package size={13} className="text-foreground inline-block mr-1.5 -mt-px" />
                  Pak
                </SelectItem>
                <SelectItem value="iostore">
                  <Layers size={13} className="text-foreground inline-block mr-1.5 -mt-px" />
                  IoStore
                </SelectItem>
              </SelectContent>
            </Select>
            <Button
              variant="blue"
              size="sm"
              className="rounded-l-none"
              onClick={openAndRepack}
              disabled={busy}
            >
              <PackageOpen size={15} />
              Repack Folder
            </Button>
          </div>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-row gap-4">
        <div className="flex min-h-0 w-[clamp(280px,28vw,560px)] min-w-[280px] max-w-[560px] shrink-0 flex-col">
          <Card className="flex min-h-0 flex-1 flex-col gap-3 p-3 bg-card">
            <div className="flex shrink-0 items-center justify-between gap-1.5">
              <h3 className="text-sm font-semibold">Game Paks</h3>
              <div className="flex items-center gap-1.5">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={openPak}
                  disabled={busy}
                  title="Open Pak"
                >
                  <FolderOpen size={14} />
                  Browse
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={listPaks}
                  disabled={busy}
                  title="List all paks from your game's Paks folder"
                >
                  <List size={14} />
                  List Paks
                </Button>
              </div>
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
                  {pakList.map((info) => {
                    const p = info.path;
                    const isSelected = selectedPak === p;
                    const isMod = /[/\\]~mods[/\\]/i.test(p);
                    const isManual = manualPaks.has(p);
                    const isIoStore = info.has_utoc && info.has_ucas;
                    const fileName = p.split(/[/\\]/).pop();
                    const displayName = isMod ? `~mods/${fileName}` : fileName;
                    return (
                      <li
                        key={p}
                        className={cn(
                          "flex items-center gap-2 border-b border-border/50 px-3 py-2 last:border-none",
                          "cursor-pointer",
                          isSelected ? "bg-secondary text-foreground" : "hover:bg-secondary/50"
                        )}
                        onClick={() => inspectPak(p)}
                        title={fileName}
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
                        {isIoStore && (
                          <span className="shrink-0 rounded bg-purple-500/20 px-1.5 py-0.5 text-[9px] font-semibold uppercase leading-none text-purple-400">
                            IoStore
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

            <div className="flex gap-1.5">
              {selectedIsIoStore && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={exportLegacy}
                  disabled={busy || !selectedPak}
                  title="Convert IoStore assets to legacy .uasset/.uexp for UAssetGUI"
                >
                  <FileOutput size={14} />
                  Export Legacy
                </Button>
              )}
              <Button
                variant="green"
                size="sm"
                onClick={unpackSelected}
                disabled={busy || !selectedPak}
              >
                <Download size={14} />
                Extract All
              </Button>
            </div>
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
              disabled={!selectedPak}
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
            ) : visible.length === 0 ? (
              <p className="p-4 text-center text-sm text-muted-foreground">
                {filterText ? "No matching files." : "No files found."}
              </p>
            ) : (
              <div
                style={{ height: `${contentsVirtualizer.getTotalSize()}px`, position: "relative" }}
              >
                {contentsVirtualizer.getVirtualItems().map((vRow) => {
                  const entry = visible[vRow.index];
                  return (
                    <div
                      key={vRow.index}
                      className="absolute left-0 top-0 flex w-full cursor-pointer items-center gap-2 border-b border-border/50 px-3 hover:bg-secondary/50"
                      style={{
                        height: `${vRow.size}px`,
                        transform: `translateY(${vRow.start}px)`,
                      }}
                      onDoubleClick={() => extractSingleEntry(entry)}
                      title={entry.path}
                    >
                      <span className="shrink-0 text-muted-foreground">{fileIcon(entry.path)}</span>
                      <span className="truncate font-mono text-[11px]">{entry.path}</span>
                      {entry.source === "utoc" && (
                        <span className="ml-auto shrink-0 rounded bg-purple-500/20 px-1 py-0.5 text-[8px] font-semibold uppercase leading-none text-purple-400">
                          utoc
                        </span>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </div>

          {/* Footer — always rendered to avoid layout shift */}
          <p
            className="shrink-0 truncate text-[11px] text-muted-foreground"
            title={selectedPak ? footerText : undefined}
          >
            {footerText}
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
