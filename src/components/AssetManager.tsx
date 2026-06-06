import * as React from "react";
import { startTransition, useState, useRef, useMemo, useCallback, useEffect } from "react";

import { useVirtualizer } from "@tanstack/react-virtual";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  CheckCircle2,
  CheckSquare2,
  Copy,
  Download,
  FileAudio,
  FileOutput,
  FileText,
  Folder,
  FolderOpen,
  Hammer,
  Layers,
  Loader2,
  MinusSquare,
  Package,
  PackageOpen,
  PackagePlus,
  RefreshCw,
  Search,
  Square,
  Users,
  XCircle,
} from "lucide-react";

import { HeroIcon } from "@/components/HeroIcon";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button, buttonVariants } from "@/components/ui/button";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tip } from "@/components/ui/tooltip";
import {
  ALL_CATEGORIES,
  type AssetCategory,
  CATEGORY_BG_COLOR,
  CATEGORY_LABEL,
  CATEGORY_TEXT_COLOR,
  classifyAssetPath,
} from "@/lib/assetCategory";
import { detectHeroIdsInPath } from "@/lib/heroIcons";
import { emitModsChanged, normalizeFolderPath, onModsChanged } from "@/lib/modsEvents";
import { useShowHeroIcons } from "@/lib/showHeroIcons";
import { cn } from "@/lib/utils";

type RepackFormat = "pak" | "iostore";
type StatusType = "ok" | "err" | "info";

interface PakFileInfo {
  path: string;
  has_utoc: boolean;
  has_ucas: boolean;
  optional_pak: string | null;
  optional_has_utoc: boolean;
  optional_has_ucas: boolean;
}

type ContentSource = "pak" | "utoc";

interface ContentEntry {
  path: string;
  source: ContentSource;
}

interface PakDerived {
  entries: ContentEntry[];
  lowered: string[];
  heroes: Map<string, number[]>;
  categories: Map<string, AssetCategory>;
}

interface CharacterSummary {
  id: number;
  name: string;
}

interface Props {
  gamePath: string;
  pendingPak?: string | null;
  onPendingPakConsumed?: () => void;
}

export function AssetManager({ gamePath, pendingPak, onPendingPakConsumed }: Props) {
  const [pakList, setPakList] = useState<PakFileInfo[]>([]);
  const [selectedPak, setSelectedPak] = useState<string>("");
  const [pakContents, setPakContents] = useState<ContentEntry[]>([]);
  const [filterText, setFilterText] = useState("");
  const [notice, setNotice] = useState<{
    msg: string;
    type: StatusType;
    revealPath?: string;
  } | null>(null);
  const [busy, setBusy] = useState(false);
  const [manualPaks, setManualPaks] = useState<Set<string>>(new Set());
  const [debouncedFilter, setDebouncedFilter] = useState("");
  const [repackFormat, setRepackFormat] = useState<RepackFormat>("pak");
  const [legacyConfirm, setLegacyConfirm] = useState<{
    count: number;
    utocPath: string;
    outputDir: string;
    filter?: string[];
  } | null>(null);
  const lastLegacyConfirmRef = useRef(legacyConfirm);
  if (legacyConfirm) lastLegacyConfirmRef.current = legacyConfirm;
  const displayLegacyConfirm = legacyConfirm ?? lastLegacyConfirmRef.current;
  const [legacyProgress, setLegacyProgress] = useState<{
    current: number;
    total: number;
  } | null>(null);
  const [repackProgress, setRepackProgress] = useState<{
    phase: string;
    current: number;
    total: number;
  } | null>(null);
  const [vanillaProgress, setVanillaProgress] = useState<{
    op: "extract" | "rebuild";
    phase: string;
    current: number;
    total: number;
  } | null>(null);
  const [vanillaConfirm, setVanillaConfirm] = useState<{
    kind: "rebuild";
    sourceUtoc: string;
    legacyDir: string;
    outputDir: string;
  } | null>(null);
  const [selectedEntries, setSelectedEntries] = useState<Set<string>>(new Set());
  const [knownHeroes, setKnownHeroes] = useState<CharacterSummary[]>([]);
  const [heroFilter, setHeroFilter] = useState<string>("all");
  const [categoryFilter, setCategoryFilter] = useState<Set<AssetCategory>>(
    () => new Set(ALL_CATEGORIES)
  );
  const showHeroIcons = useShowHeroIcons();
  const lastClickedIndex = useRef<number | null>(null);
  const loadGenRef = useRef(0);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const filterTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const contentsScrollRef = useRef<HTMLDivElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const listPaksRef = useRef<(silent?: boolean) => Promise<void>>(null!);
  const pakContentsCacheRef = useRef<Map<string, PakDerived>>(new Map());

  // Load game paks on mount
  useEffect(() => {
    if (gamePath) listPaks();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    invoke<CharacterSummary[]>("list_known_heroes")
      .then(setKnownHeroes)
      .catch(() => setKnownHeroes([]));
  }, []);

  // Re-list paks when ~mods composition changes elsewhere (mod install/delete, repack, recursive toggle).
  useEffect(() => {
    return onModsChanged((event) => {
      if (!gamePath) return;
      const modsFolder = `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`;
      if (normalizeFolderPath(event.modsFolder) !== normalizeFolderPath(modsFolder)) return;
      const modsPrefix = normalizeFolderPath(modsFolder);
      for (const key of [...pakContentsCacheRef.current.keys()]) {
        if (normalizeFolderPath(key).startsWith(modsPrefix)) {
          pakContentsCacheRef.current.delete(key);
        }
      }
      listPaksRef.current(true);
    });
  }, [gamePath]);

  // Listen for legacy extraction progress events
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    listen<{ current: number; total: number }>("legacy-extraction-progress", (event) => {
      setLegacyProgress(event.payload);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Listen for IoStore repack progress events
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    listen<{ phase: string; current: number; total: number }>(
      "repack-iostore-progress",
      (event) => {
        setRepackProgress(event.payload);
      }
    ).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    const unlisteners: Array<() => void> = [];
    listen<{ phase: string; current: number; total: number }>(
      "vanilla-extract-progress",
      (event) => {
        setVanillaProgress({ op: "extract", ...event.payload });
      }
    ).then((fn) => {
      if (cancelled) fn();
      else unlisteners.push(fn);
    });
    listen<{ phase: string; current: number; total: number }>(
      "vanilla-rebuild-progress",
      (event) => {
        setVanillaProgress({ op: "rebuild", ...event.payload });
      }
    ).then((fn) => {
      if (cancelled) fn();
      else unlisteners.push(fn);
    });
    return () => {
      cancelled = true;
      unlisteners.forEach((fn) => fn());
    };
  }, []);

  // Navigate to a specific pak when triggered from another tab (e.g. Mods → "View in Asset Manager")
  useEffect(() => {
    if (!pendingPak) return;
    if (pakList.length === 0) return;
    inspectPak(pendingPak);
    onPendingPakConsumed?.();
  }, [pendingPak, pakList]); // eslint-disable-line react-hooks/exhaustive-deps

  const showNotice = (
    msg: string,
    type: StatusType,
    opts?: { duration?: number; revealPath?: string }
  ) => {
    // Strip any trailing path from error messages
    const clean =
      type === "err" ? msg.replace(/:\s*[A-Za-z]:\\[^\r\n]*|:\s*\/[^\r\n]*/g, "").trim() : msg;
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    setNotice({ msg: clean, type, revealPath: opts?.revealPath });
    // "info" notices persist until replaced by another notice (no auto-dismiss)
    const ms = opts?.duration ?? (type === "info" ? 0 : 6000);
    if (ms > 0) {
      noticeTimer.current = setTimeout(() => setNotice(null), ms);
    }
  };

  const knownHeroIds = useMemo(() => new Set(knownHeroes.map((h) => h.id)), [knownHeroes]);
  const heroNameById = useMemo(() => {
    const m = new Map<number, string>();
    for (const h of knownHeroes) m.set(h.id, h.name);
    return m;
  }, [knownHeroes]);

  // Hero set changes invalidate cached heroes/derived maps for every pak.
  useEffect(() => {
    pakContentsCacheRef.current.clear();
  }, [knownHeroIds]);

  const pakContentsLower = useMemo(() => {
    const hit = selectedPak ? pakContentsCacheRef.current.get(selectedPak) : null;
    if (hit && hit.entries === pakContents) return hit.lowered;
    return pakContents.map((e) => e.path.toLowerCase());
  }, [pakContents, selectedPak]);

  const heroesByPath = useMemo(() => {
    const hit = selectedPak ? pakContentsCacheRef.current.get(selectedPak) : null;
    if (hit && hit.entries === pakContents) return hit.heroes;
    if (knownHeroIds.size === 0 || pakContents.length === 0) {
      return new Map<string, number[]>();
    }
    const map = new Map<string, number[]>();
    for (const e of pakContents) {
      const ids = detectHeroIdsInPath(e.path, knownHeroIds);
      if (ids.length > 0) map.set(e.path, ids);
    }
    return map;
  }, [pakContents, selectedPak, knownHeroIds]);

  const categoriesByPath = useMemo(() => {
    const hit = selectedPak ? pakContentsCacheRef.current.get(selectedPak) : null;
    if (hit && hit.entries === pakContents) return hit.categories;
    const map = new Map<string, AssetCategory>();
    for (const e of pakContents) map.set(e.path, classifyAssetPath(e.path));
    return map;
  }, [pakContents, selectedPak]);

  const categoryCounts = useMemo(() => {
    const counts = Object.fromEntries(ALL_CATEGORIES.map((c) => [c, 0])) as Record<
      AssetCategory,
      number
    >;
    for (const c of categoriesByPath.values()) counts[c]++;
    return counts;
  }, [categoriesByPath]);

  const allCategoriesOn = categoryFilter.size === ALL_CATEGORIES.length;

  // Filter against pre-lowered names, debounced input, hero, and category.
  const visible = useMemo(() => {
    let list: ContentEntry[] = pakContents;
    if (debouncedFilter) {
      const needle = debouncedFilter.toLowerCase();
      list = pakContents.filter((_, i) => pakContentsLower[i].includes(needle));
    }
    if (heroFilter === "unknown") {
      list = list.filter((e) => !heroesByPath.has(e.path));
    } else if (heroFilter !== "all") {
      const id = Number(heroFilter);
      list = list.filter((e) => heroesByPath.get(e.path)?.includes(id) ?? false);
    }
    if (!allCategoriesOn) {
      list = list.filter((e) => categoryFilter.has(categoriesByPath.get(e.path) ?? "other"));
    }
    return list;
  }, [
    pakContents,
    pakContentsLower,
    debouncedFilter,
    heroFilter,
    heroesByPath,
    categoryFilter,
    categoriesByPath,
    allCategoriesOn,
  ]);

  const toggleCategory = useCallback((cat: AssetCategory) => {
    setCategoryFilter((prev) => {
      const next = new Set(prev);
      if (next.has(cat)) next.delete(cat);
      else next.add(cat);
      return next;
    });
  }, []);

  const setAllCategories = useCallback((on: boolean) => {
    setCategoryFilter(on ? new Set(ALL_CATEGORIES) : new Set());
  }, []);

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

  const selectedIsVanilla = useMemo(
    () => !!selectedPak && !/[/\\]~mods[/\\]/i.test(selectedPak),
    [selectedPak]
  );

  // Virtualizer for the contents list
  const contentsVirtualizer = useVirtualizer({
    count: visible.length,
    getScrollElement: () => contentsScrollRef.current,
    estimateSize: () => 30,
    overscan: 20,
  });

  listPaksRef.current = listPaks;

  async function listPaks(silent = false) {
    if (!silent) {
      setNotice(null);
      if (noticeTimer.current) clearTimeout(noticeTimer.current);
      pakContentsCacheRef.current.clear();
    }

    if (!gamePath) {
      if (!silent) showNotice("Set game root in Settings first.", "err");
      return;
    }

    setBusy(true);
    try {
      const paks = await invoke<PakFileInfo[]>("list_pak_files_info", { gameRoot: gamePath });
      setPakList(paks);

      if (paks.length === 0) {
        setSelectedPak("");
        setPakContents([]);
        pakContentsCacheRef.current.clear();
        if (!silent) showNotice("No .pak files found.", "err");
      } else {
        const known = new Set(paks.map((p) => p.path));
        for (const key of [...pakContentsCacheRef.current.keys()]) {
          if (!known.has(key)) pakContentsCacheRef.current.delete(key);
        }
        if (selectedPak && !paks.some((p) => p.path === selectedPak)) {
          setSelectedPak("");
          setPakContents([]);
          setFilterText("");
          setDebouncedFilter("");
        }
        if (!silent)
          showNotice(`${paks.length} pak${paks.length !== 1 ? "s" : ""} found`, "ok", {
            duration: 4000,
          });
      }
    } catch (e: unknown) {
      if (!silent) showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function inspectPak(pak: string, infoOverride?: PakFileInfo) {
    if (pak === selectedPak) return;
    const gen = ++loadGenRef.current;
    setSelectedPak(pak);
    setFilterText("");
    setDebouncedFilter("");
    setSelectedEntries(new Set());
    lastClickedIndex.current = null;

    const cached = pakContentsCacheRef.current.get(pak);
    if (cached) {
      setPakContents(cached.entries);
      const displayName = pak.split(/[/\\]/).pop();
      const info = infoOverride ?? pakList.find((p) => p.path === pak);
      const optionalSuffix = info?.optional_pak ? " (incl. optional)" : "";
      showNotice(`${cached.entries.length} file(s) inside ${displayName}${optionalSuffix}`, "ok");
      // A still in-flight load from the previous selection will skip setBusy(false) via
      // its gen guard, so clear it here for the latest gen.
      setBusy(false);
      return;
    }
    setPakContents([]);

    let info = infoOverride ?? pakList.find((p) => p.path === pak);
    if (!info) {
      const [hasUtoc, hasUcas] = await Promise.all([
        invoke<boolean>("path_exists", { path: pak.replace(/\.pak$/i, ".utoc") }),
        invoke<boolean>("path_exists", { path: pak.replace(/\.pak$/i, ".ucas") }),
      ]);
      if (gen !== loadGenRef.current) return;
      info = {
        path: pak,
        has_utoc: hasUtoc,
        has_ucas: hasUcas,
        optional_pak: null,
        optional_has_utoc: false,
        optional_has_ucas: false,
      };
    }
    const isIoStore = info.has_utoc && info.has_ucas;
    const optionalPak = info.optional_pak;
    const optionalIsIoStore = !!optionalPak && info.optional_has_utoc && info.optional_has_ucas;

    setBusy(true);
    try {
      const utocPath = isIoStore ? pak.replace(/\.pak$/i, ".utoc") : "";
      const optionalUtocPath = optionalIsIoStore ? optionalPak.replace(/\.pak$/i, ".utoc") : "";
      const [pakFiles, utocResult, optionalPakFiles, optionalUtocResult] = await Promise.all([
        invoke<string[]>("list_pak_contents", { pakPath: pak }),
        isIoStore
          ? invoke<string[]>("list_utoc_contents", { utocPath }).then(
              (files) => ({ ok: true as const, files }),
              (err) => ({ ok: false as const, err: String(err) })
            )
          : Promise.resolve({ ok: true as const, files: [] as string[] }),
        optionalPak
          ? invoke<string[]>("list_pak_contents", { pakPath: optionalPak }).catch(
              () => [] as string[]
            )
          : Promise.resolve([] as string[]),
        optionalIsIoStore
          ? invoke<string[]>("list_utoc_contents", { utocPath: optionalUtocPath }).then(
              (files) => ({ ok: true as const, files }),
              (err) => ({ ok: false as const, err: String(err) })
            )
          : Promise.resolve({ ok: true as const, files: [] as string[] }),
      ]);

      if (gen !== loadGenRef.current) return;

      const entries: ContentEntry[] = [];
      const seen = new Set<string>();
      const pushUnique = (path: string, source: ContentSource) => {
        const key = `${source}:${path}`;
        if (seen.has(key)) return;
        seen.add(key);
        entries.push({ path, source });
      };
      for (const f of pakFiles) {
        if (isIoStore && f === "chunknames") continue;
        pushUnique(f, "pak");
      }
      for (const f of optionalPakFiles) {
        if (optionalIsIoStore && f === "chunknames") continue;
        pushUnique(f, "pak");
      }
      if (utocResult.ok) {
        for (const f of utocResult.files) pushUnique(f, "utoc");
      }
      if (optionalUtocResult.ok) {
        for (const f of optionalUtocResult.files) pushUnique(f, "utoc");
      }

      const displayName = pak.split(/[/\\]/).pop();
      const utocFailed = !utocResult.ok || !optionalUtocResult.ok;
      const optionalSuffix = optionalPak ? " (incl. optional)" : "";
      if (utocFailed) {
        setPakContents(entries);
        showNotice(
          `${entries.length} file(s) inside ${displayName}${optionalSuffix} (utoc failed to load)`,
          "err"
        );
      } else {
        const lowered = new Array<string>(entries.length);
        const heroes = new Map<string, number[]>();
        const categories = new Map<string, AssetCategory>();
        const haveHeroes = knownHeroIds.size > 0;
        const CHUNK = 8000;
        for (let i = 0; i < entries.length; i += CHUNK) {
          const end = Math.min(i + CHUNK, entries.length);
          for (let j = i; j < end; j++) {
            const e = entries[j];
            lowered[j] = e.path.toLowerCase();
            categories.set(e.path, classifyAssetPath(e.path));
            if (haveHeroes) {
              const ids = detectHeroIdsInPath(e.path, knownHeroIds);
              if (ids.length > 0) heroes.set(e.path, ids);
            }
          }
          if (end < entries.length) {
            await new Promise((resolve) => setTimeout(resolve, 0));
            if (gen !== loadGenRef.current) return;
          }
        }
        pakContentsCacheRef.current.set(pak, { entries, lowered, heroes, categories });
        setPakContents(entries);
        showNotice(`${entries.length} file(s) inside ${displayName}${optionalSuffix}`, "ok");
      }
    } catch (e: unknown) {
      if (gen !== loadGenRef.current) return;
      showNotice(String(e), "err");
    } finally {
      if (gen === loadGenRef.current) setBusy(false);
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
      const info: PakFileInfo = {
        path: selected,
        has_utoc: hasUtoc,
        has_ucas: hasUcas,
        optional_pak: null,
        optional_has_utoc: false,
        optional_has_ucas: false,
      };
      setPakList((prev) => {
        if (prev.some((p) => p.path === selected)) return prev;
        return [...prev, info];
      });
      setManualPaks((prev) => new Set(prev).add(selected));
      await inspectPak(selected, info);
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

      showNotice(`Extracted ${totalFiles} file(s) to ${outputDir}`, "ok", {
        revealPath: outputDir,
      });
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function copyToClipboard(text: string, label: string) {
    try {
      await navigator.clipboard.writeText(text);
      showNotice(`Copied ${label}`, "ok");
    } catch (e: unknown) {
      showNotice(`Copy failed: ${String(e)}`, "err");
    }
  }

  function parentDir(p: string): string {
    const idx = p.lastIndexOf("/");
    return idx > 0 ? p.slice(0, idx) : "";
  }

  async function revealPak(pak: string) {
    try {
      await revealItemInDir(pak);
    } catch (e: unknown) {
      showNotice(String(e), "err");
    }
  }

  function removeManualPak(pak: string) {
    setPakList((prev) => prev.filter((info) => info.path !== pak));
    setManualPaks((prev) => {
      const next = new Set(prev);
      next.delete(pak);
      return next;
    });
    pakContentsCacheRef.current.delete(pak);
    if (selectedPak === pak) {
      setSelectedPak("");
      setPakContents([]);
      setSelectedEntries(new Set());
    }
  }

  function selectByExtension(ext: string) {
    const lowered = `.${ext.toLowerCase()}`;
    const next = new Set<string>();
    for (const e of visible) {
      if (e.path.toLowerCase().endsWith(lowered)) next.add(e.path);
    }
    if (next.size === 0) {
      showNotice(`No ${ext.toUpperCase()} files in view`, "info", { duration: 3000 });
      return;
    }
    startTransition(() => setSelectedEntries(next));
    showNotice(`Selected ${next.size} ${ext.toUpperCase()} file(s)`, "ok", { duration: 3000 });
  }

  function clearEntrySelection() {
    setSelectedEntries(new Set());
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
      showNotice(`Extracted: ${outPath}`, "ok", { revealPath: outPath });
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function extractSingleEntryWithFolders(entry: ContentEntry) {
    const dir = await open({ directory: true, multiple: false });
    if (!dir || typeof dir !== "string") return;

    const pakBaseName =
      selectedPak
        .replace(/\\/g, "/")
        .split("/")
        .pop()
        ?.replace(/\.pak$/i, "") ?? "output";
    const outputDir = `${dir}\\${pakBaseName}`;

    setBusy(true);
    showNotice("Extracting…", "info");
    try {
      let extracted: string[] = [];
      if (entry.source === "utoc") {
        const utocPath = selectedPak.replace(/\.pak$/i, ".utoc");
        extracted = await invoke<string[]>("extract_utoc_files", {
          utocPath,
          fileNames: [entry.path],
          outputDir,
        });
      } else {
        extracted = await invoke<string[]>("extract_pak_files", {
          pakPath: selectedPak,
          fileNames: [entry.path],
          outputDir,
        });
      }
      const revealPath = extracted[0]
        ? `${outputDir}\\${extracted[0].replace(/\//g, "\\")}`
        : outputDir;
      showNotice(`Extracted to ${outputDir}`, "ok", { revealPath });
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
    const outputDir = `${dir}\\${pakBaseName}`;
    const utocPath = selectedPak.replace(/\.pak$/i, ".utoc");

    // Count packages first to warn on large extractions
    setBusy(true);
    showNotice("Counting packages\u2026", "info");
    try {
      const count = await invoke<number>("count_utoc_legacy_packages", {
        utocPath,
        gameRoot: gamePath,
        filter: [],
      });
      setBusy(false);
      setNotice(null);

      if (count > 500) {
        setLegacyConfirm({ count, utocPath, outputDir });
        return;
      }

      await runLegacyExtraction(utocPath, outputDir);
    } catch (e: unknown) {
      showNotice(String(e), "err");
      setBusy(false);
    }
  }

  async function runLegacyExtraction(utocPath: string, outputDir: string, filter: string[] = []) {
    setBusy(true);
    setLegacyProgress(null);
    showNotice("Converting to legacy format\u2026", "info");
    try {
      const files = await invoke<string[]>("extract_utoc_legacy", {
        utocPath,
        gameRoot: gamePath,
        outputDir,
        filter,
      });
      const warning = files.find((f) => f.startsWith("__warnings__:"));
      const exported = files.filter((f) => !f.startsWith("__warnings__:"));
      if (warning) {
        setLegacyProgress(null);
        showNotice(
          `Exported ${exported.length} asset(s) to ${outputDir} (some failed to convert)`,
          "err",
          { revealPath: outputDir }
        );
      } else {
        setLegacyProgress(null);
        showNotice(`Exported ${exported.length} asset(s) to ${outputDir}`, "ok", {
          revealPath: outputDir,
        });
      }
    } catch (e: unknown) {
      setLegacyProgress(null);
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function cancelLegacyExtraction() {
    try {
      await invoke("cancel_legacy_extraction");
    } catch {
      // Best-effort — extraction loop will stop on next batch regardless
    }
  }

  async function cancelRepackIostore() {
    try {
      await invoke("cancel_repack_iostore");
    } catch {
      // Best-effort
    }
  }

  function handleEntryClick(index: number, e: React.MouseEvent) {
    setSelectedEntries((prev) => {
      const next = new Set(prev);
      if (e.shiftKey && lastClickedIndex.current !== null) {
        const start = Math.min(lastClickedIndex.current, index);
        const end = Math.max(lastClickedIndex.current, index);
        for (let i = start; i <= end; i++) {
          next.add(visible[i].path);
        }
      } else if (e.ctrlKey || e.metaKey) {
        const path = visible[index].path;
        if (next.has(path)) next.delete(path);
        else next.add(path);
      } else {
        next.clear();
        next.add(visible[index].path);
      }
      return next;
    });
    lastClickedIndex.current = index;
  }

  function toggleSelectAll() {
    startTransition(() => {
      setSelectedEntries((prev) => {
        if (prev.size === visible.length && visible.every((e) => prev.has(e.path))) {
          return new Set();
        }
        return new Set(visible.map((e) => e.path));
      });
    });
  }

  // Ctrl/Cmd+A selects all visible asset rows. Skips when focus is inside an
  // input/textarea/contentEditable so native text-select still works, and
  // skips when AssetManager is in a hidden tab (offsetParent === null).
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (!(e.ctrlKey || e.metaKey) || e.key !== "a") return;
      const root = rootRef.current;
      if (!root || root.offsetParent === null) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      if (visible.length === 0) return;
      e.preventDefault();
      startTransition(() => {
        setSelectedEntries(new Set(visible.map((entry) => entry.path)));
      });
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [visible]);

  const selectedPakEntries = useMemo(
    () => pakContents.filter((e) => selectedEntries.has(e.path) && e.source === "pak"),
    [pakContents, selectedEntries]
  );
  const selectedUtocEntries = useMemo(
    () =>
      pakContents.filter(
        (e) => selectedEntries.has(e.path) && e.source === "utoc" && !e.path.endsWith(".ubulk")
      ),
    [pakContents, selectedEntries]
  );
  const selectedUtocEntriesAll = useMemo(
    () => pakContents.filter((e) => selectedEntries.has(e.path) && e.source === "utoc"),
    [pakContents, selectedEntries]
  );

  const allVisibleSelected = useMemo(
    () =>
      selectedEntries.size > 0 &&
      selectedEntries.size === visible.length &&
      visible.every((e) => selectedEntries.has(e.path)),
    [selectedEntries, visible]
  );

  const hasSelectedBnk = useMemo(() => {
    for (const p of selectedEntries) {
      if (p.split("/").pop()?.toLowerCase() === "bnk_ui_battle.bnk") return true;
    }
    return false;
  }, [selectedEntries]);

  async function extractSelected() {
    if (selectedEntries.size === 0 || !selectedPak) return;
    const dir = await open({ directory: true, multiple: false });
    if (!dir || typeof dir !== "string") return;

    const pakBaseName =
      selectedPak
        .replace(/\\/g, "/")
        .split("/")
        .pop()
        ?.replace(/\.pak$/i, "") ?? "output";
    const outputDir = `${dir}\\${pakBaseName}`;

    setBusy(true);
    showNotice("Extracting selected\u2026", "info");
    try {
      let totalFiles = 0;

      if (selectedPakEntries.length > 0) {
        const names = selectedPakEntries.map((e) => e.path);
        const extracted = await invoke<string[]>("extract_pak_files", {
          pakPath: selectedPak,
          fileNames: names,
          outputDir,
        });
        totalFiles += extracted.length;
      }

      if (selectedUtocEntriesAll.length > 0) {
        const utocPath = selectedPak.replace(/\.pak$/i, ".utoc");
        const names = selectedUtocEntriesAll.map((e) => e.path);
        const extracted = await invoke<string[]>("extract_utoc_files", {
          utocPath,
          fileNames: names,
          outputDir,
        });
        totalFiles += extracted.length;
      }

      showNotice(`Extracted ${totalFiles} file(s) to ${outputDir}`, "ok", {
        revealPath: outputDir,
      });
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function extractSoundWavs() {
    if (!selectedPak) return;
    const dir = await open({ directory: true, multiple: false });
    if (!dir || typeof dir !== "string") return;

    setBusy(true);
    showNotice("Extracting sound WAVs\u2026", "info");
    try {
      const result = await invoke<string>("extract_sound_wavs", {
        gameRoot: gamePath,
        pakPath: selectedPak,
        outputDir: dir,
      });
      // Derive subfolder name matching backend logic: strip .pak, then _N_P suffix
      const stem = (selectedPak.split(/[\\/]/).pop() ?? "").replace(/\.pak$/i, "");
      const folderName = stem.replace(/_\d+_P$/i, "");
      const subfolderPath = `${dir}\\${folderName}`;
      showNotice(result, "ok", { revealPath: subfolderPath });
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
    }
  }

  async function exportLegacySingle(entry: ContentEntry) {
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
    const utocPath = selectedPak.replace(/\.pak$/i, ".utoc");
    const filter = [entry.path];

    setBusy(true);
    showNotice("Counting packages\u2026", "info");
    try {
      const count = await invoke<number>("count_utoc_legacy_packages", {
        utocPath,
        gameRoot: gamePath,
        filter,
      });
      setBusy(false);
      setNotice(null);

      if (count > 500) {
        setLegacyConfirm({ count, utocPath, outputDir, filter });
        return;
      }

      await runLegacyExtraction(utocPath, outputDir, filter);
    } catch (e: unknown) {
      showNotice(String(e), "err");
      setBusy(false);
    }
  }

  async function exportLegacySelected() {
    if (selectedUtocEntries.length === 0 || !selectedPak) return;
    const dir = await open({ directory: true, multiple: false });
    if (!dir || typeof dir !== "string") return;

    const pakBaseName =
      selectedPak
        .replace(/\\/g, "/")
        .split("/")
        .pop()
        ?.replace(/\.pak$/i, "") ?? "output";
    const outputDir = `${dir}\\${pakBaseName}`;
    const utocPath = selectedPak.replace(/\.pak$/i, ".utoc");
    const filter = selectedUtocEntries.map((e) => e.path);

    setBusy(true);
    showNotice("Counting packages\u2026", "info");
    try {
      const count = await invoke<number>("count_utoc_legacy_packages", {
        utocPath,
        gameRoot: gamePath,
        filter,
      });
      setBusy(false);
      setNotice(null);

      if (count > 500) {
        setLegacyConfirm({ count, utocPath, outputDir, filter });
        return;
      }

      await runLegacyExtraction(utocPath, outputDir, filter);
    } catch (e: unknown) {
      showNotice(String(e), "err");
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
        setRepackProgress(null);
        showNotice(`Repacked IoStore to: ${outputUtoc}`, "ok", { revealPath: outputUtoc });
        emitModsChanged({
          modsFolder: outputUtoc.replace(/[\\/][^\\/]+$/, ""),
          source: "AssetManager",
        });
      } catch (e: unknown) {
        setRepackProgress(null);
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
        showNotice(`Repacked to: ${outputPak}`, "ok", { revealPath: outputPak });
        emitModsChanged({
          modsFolder: outputPak.replace(/[\\/][^\\/]+$/, ""),
          source: "AssetManager",
        });
      } catch (e: unknown) {
        showNotice(String(e), "err");
      } finally {
        setBusy(false);
      }
    }
  }

  async function extractVanilla() {
    if (!selectedPak || !gamePath || !selectedIsVanilla || !selectedIsIoStore) return;
    const dir = await open({
      directory: true,
      multiple: false,
      title: "Choose extract output folder",
    });
    if (typeof dir !== "string") return;
    const pakBaseName =
      selectedPak
        .replace(/\\/g, "/")
        .split("/")
        .pop()
        ?.replace(/\.pak$/i, "") ?? "output";
    const outputDir = `${dir}\\${pakBaseName}`;
    const sourceUtoc = selectedPak.replace(/\.pak$/i, ".utoc");
    setBusy(true);
    setVanillaProgress({ op: "extract", phase: "starting", current: 0, total: 0 });
    showNotice("Extracting vanilla container…", "info");
    try {
      const report = await invoke<{
        container_name: string;
        optional_container_name: string | null;
        package_count: number;
        shader_library_count: number;
        pak_entry_count: number;
        uasset_count: number;
        umap_count: number;
        uexp_count: number;
        ubulk_count: number;
        uptnl_count: number;
        memory_mapped_count: number;
        script_objects_count: number;
        total_files: number;
      }>("extract_vanilla_container", {
        gameRoot: gamePath,
        sourceUtoc,
        outputDir,
      });
      const optTag = report.optional_container_name ? "+opt" : "";
      showNotice(`Extracted ${report.container_name}${optTag} to ${outputDir}`, "ok", {
        revealPath: outputDir,
      });
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
      setVanillaProgress(null);
    }
  }

  async function rebuildVanillaPrompt() {
    if (!selectedPak || !gamePath || !selectedIsVanilla || !selectedIsIoStore) return;
    const legacyDir = await open({
      directory: true,
      multiple: false,
      title: "Choose legacy folder to rebuild from",
    });
    if (typeof legacyDir !== "string") return;
    const outputDir = await open({
      directory: true,
      multiple: false,
      title: "Choose output folder for rebuilt container",
    });
    if (typeof outputDir !== "string") return;
    const sourceUtoc = selectedPak.replace(/\.pak$/i, ".utoc");
    setVanillaConfirm({ kind: "rebuild", sourceUtoc, legacyDir, outputDir });
  }

  async function runRebuildVanilla() {
    if (!vanillaConfirm) return;
    const { sourceUtoc, legacyDir, outputDir } = vanillaConfirm;
    setVanillaConfirm(null);
    setBusy(true);
    setVanillaProgress({ op: "rebuild", phase: "starting", current: 0, total: 0 });
    showNotice("Rebuilding vanilla container…", "info");
    try {
      const report = await invoke<{
        container_name: string;
        optional_container_name: string | null;
        package_count: number;
        uasset_count: number;
        umap_count: number;
        ubulk_routed: number;
        uptnl_routed: number;
        memory_mapped_routed: number;
        shader_library_count: number;
        pak_entry_count: number;
      }>("rebuild_vanilla_container", {
        gameRoot: gamePath,
        sourceUtoc,
        legacyDir,
        outputDir,
      });
      const optTag = report.optional_container_name ? "+opt" : "";
      showNotice(`Rebuilt ${report.container_name}${optTag} to ${outputDir}`, "ok", {
        revealPath: outputDir,
      });
    } catch (e: unknown) {
      showNotice(String(e), "err");
    } finally {
      setBusy(false);
      setVanillaProgress(null);
    }
  }

  async function cancelVanillaOp() {
    if (!vanillaProgress) return;
    try {
      if (vanillaProgress.op === "extract") {
        await invoke("cancel_vanilla_extract");
      } else {
        await invoke("cancel_vanilla_rebuild");
      }
    } catch (e) {
      console.error(e);
    }
  }

  const pakName = selectedPak ? selectedPak.split(/[/\\]/).pop() : null;
  const footerText = !selectedPak
    ? "\u00A0"
    : selectedEntries.size > 0
      ? `${selectedEntries.size} selected \u2014 ${visible.length} file(s)${visible.length !== pakContents.length ? ` of ${pakContents.length}` : ""} inside ${pakName}`
      : `${visible.length} file(s)${visible.length !== pakContents.length ? ` of ${pakContents.length}` : ""} inside ${pakName} \u2014 click to select, double-click to extract`;

  return (
    <div ref={rootRef} className="flex flex-1 min-h-0 flex-col gap-4">
      <div className="flex min-h-8 shrink-0 items-center gap-3">
        <h2 className="shrink-0 text-xl font-bold">Asset Manager</h2>
        {notice && !vanillaProgress && (
          <Tip content="Click to reveal in explorer" disabled={!notice.revealPath}>
            <span
              className={cn(
                "flex min-w-0 items-center gap-1.5 truncate text-[12px] font-medium",
                notice.type === "ok"
                  ? "text-ok"
                  : notice.type === "err"
                    ? "text-err"
                    : "text-muted-foreground",
                notice.revealPath && "cursor-pointer hover:underline"
              )}
              onClick={notice.revealPath ? () => revealItemInDir(notice.revealPath!) : undefined}
            >
              {notice.type === "ok" ? (
                <CheckCircle2 className="shrink-0" size={14} strokeWidth={2.5} />
              ) : notice.type === "err" ? (
                <XCircle className="shrink-0" size={14} strokeWidth={2.5} />
              ) : null}
              <span className="truncate">{notice.msg}</span>
            </span>
          </Tip>
        )}
        {legacyProgress && (
          <div className="flex min-w-0 items-center gap-2">
            <Progress
              value={(legacyProgress.current / legacyProgress.total) * 100}
              className="h-2 w-32"
            />
            <span className="shrink-0 text-[11px] text-muted-foreground">
              {legacyProgress.current}/{legacyProgress.total}
            </span>
            <Button
              variant="outline"
              size="sm"
              className="h-6 px-2 text-[11px]"
              onClick={cancelLegacyExtraction}
            >
              Cancel
            </Button>
          </div>
        )}
        {repackProgress && (
          <div className="flex min-w-0 items-center gap-2">
            <Progress
              value={(repackProgress.current / repackProgress.total) * 100}
              className="h-2 w-32"
            />
            <span className="shrink-0 text-[11px] text-muted-foreground">
              {repackProgress.current}/{repackProgress.total}
            </span>
            <Button
              variant="outline"
              size="sm"
              className="h-6 px-2 text-[11px]"
              onClick={cancelRepackIostore}
            >
              Cancel
            </Button>
          </div>
        )}
        {vanillaProgress && (
          <div className="flex min-w-0 items-center gap-2">
            <span className="shrink-0 text-[11px] text-muted-foreground">
              {vanillaProgress.op} · {vanillaProgress.phase}
            </span>
            {vanillaProgress.total > 0 ? (
              <>
                <Progress
                  value={(vanillaProgress.current / vanillaProgress.total) * 100}
                  className="h-2 w-32"
                />
                <span className="shrink-0 text-[11px] text-muted-foreground">
                  {vanillaProgress.current}/{vanillaProgress.total}
                </span>
              </>
            ) : (
              <Loader2 size={12} className="shrink-0 animate-spin text-muted-foreground" />
            )}
            <Button
              variant="outline"
              size="sm"
              className="h-6 px-2 text-[11px]"
              onClick={cancelVanillaOp}
            >
              Cancel
            </Button>
          </div>
        )}
        <div className="ml-auto flex shrink-0 items-center gap-2">
          <div className="flex items-center">
            <Select value={repackFormat} onValueChange={(v) => setRepackFormat(v as RepackFormat)}>
              <SelectTrigger
                size="sm"
                className="h-8 w-30 rounded-r-none border-r-0 text-sm font-medium"
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
        <div className="flex min-h-0 w-[clamp(280px,28vw,560px)] min-w-70 max-w-140 shrink-0 flex-col">
          <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-md border border-border">
            <div className="flex shrink-0 items-center justify-between gap-1.5 border-b border-border bg-card px-3 py-2">
              <h3 className="text-sm font-semibold">Game Paks</h3>
              <div className="flex items-center gap-1">
                <Tip content="Browse for a pak file">
                  <Button variant="ghost" size="icon-sm" onClick={openPak} disabled={busy}>
                    <FolderOpen size={15} />
                  </Button>
                </Tip>
                <Tip content="Refresh game paks">
                  <Button variant="ghost" size="icon-sm" onClick={() => listPaks()} disabled={busy}>
                    <RefreshCw size={15} />
                  </Button>
                </Tip>
              </div>
            </div>

            <div className="min-h-0 flex-1 overflow-y-auto">
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
                    const modsIdx = p.search(/[/\\]~mods[/\\]/i);
                    const displayName =
                      isMod && modsIdx !== -1 ? p.slice(modsIdx + 1).replace(/\\/g, "/") : fileName;
                    return (
                      <ContextMenu key={p}>
                        <Tip content={fileName} side="top" align="end">
                          <ContextMenuTrigger asChild>
                            <li
                              className={cn(
                                "flex items-center gap-2 border-b border-border/50 px-3 py-2 last:border-none",
                                "cursor-pointer",
                                isSelected
                                  ? "bg-secondary text-foreground"
                                  : "hover:bg-secondary/50"
                              )}
                              onClick={() => inspectPak(p)}
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
                                <span className="shrink-0 rounded bg-ok/15 px-1.5 py-0.5 text-[9px] font-semibold uppercase leading-none text-ok">
                                  IoStore
                                </span>
                              )}
                            </li>
                          </ContextMenuTrigger>
                        </Tip>
                        <ContextMenuContent>
                          <ContextMenuItem onSelect={() => inspectPak(p)}>
                            <PackageOpen size={14} />
                            Open contents
                          </ContextMenuItem>
                          <ContextMenuItem onSelect={() => revealPak(p)}>
                            <FolderOpen size={14} />
                            Reveal in File Explorer
                          </ContextMenuItem>
                          <ContextMenuSeparator />
                          <ContextMenuItem onSelect={() => copyToClipboard(p, "path")}>
                            <Copy size={14} />
                            Copy path
                          </ContextMenuItem>
                          <ContextMenuItem
                            onSelect={() => copyToClipboard(fileName ?? p, "file name")}
                          >
                            <FileText size={14} />
                            Copy file name
                          </ContextMenuItem>
                          {isManual && (
                            <>
                              <ContextMenuSeparator />
                              <ContextMenuItem destructive onSelect={() => removeManualPak(p)}>
                                <XCircle size={14} />
                                Remove from list
                              </ContextMenuItem>
                            </>
                          )}
                        </ContextMenuContent>
                      </ContextMenu>
                    );
                  })}
                </ul>
              )}
            </div>
          </div>
        </div>

        <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-md border border-border">
          {/* Header zone */}
          <div className="flex shrink-0 flex-col gap-2 border-b border-border bg-card px-3 py-2">
            {/* Title + actions */}
            <div className="flex items-center justify-between gap-2">
              <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
                <Tip
                  content={selectedPak || ""}
                  disabled={!selectedPak}
                  side="bottom"
                  align="start"
                >
                  <div className="flex min-w-0 items-center gap-2">
                    <h3 className="truncate text-sm font-semibold">{pakName ?? "Contents"}</h3>
                    {selectedPak && (
                      <span className="shrink-0 rounded bg-ok/15 px-1.5 py-0.5 text-[9px] font-semibold uppercase leading-none text-ok">
                        {selectedIsIoStore ? "iostore" : "pak"}
                      </span>
                    )}
                  </div>
                </Tip>
                <Tip
                  content={selectedPak || ""}
                  disabled={!selectedPak}
                  side="bottom"
                  align="start"
                >
                  <span
                    className={cn(
                      "truncate text-[11px] text-muted-foreground",
                      !selectedPak && "invisible"
                    )}
                  >
                    {selectedPak || "\u00A0"}
                  </span>
                </Tip>
              </div>

              <div className="flex shrink-0 items-center gap-1 overflow-visible py-1 -my-1 pr-1 -mr-1">
                {selectedIsVanilla && selectedIsIoStore && (
                  <>
                    <Tip content="Extract this container to an editable legacy folder">
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        onClick={extractVanilla}
                        disabled={busy || !selectedPak || !gamePath}
                      >
                        <PackageOpen size={15} />
                      </Button>
                    </Tip>
                    <Tip content="Rebuild this container from an edited legacy folder">
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        onClick={rebuildVanillaPrompt}
                        disabled={busy || !selectedPak || !gamePath}
                      >
                        <Hammer size={15} />
                      </Button>
                    </Tip>
                  </>
                )}
                {hasSelectedBnk && (
                  <Tip content="Extract sound WAVs from this mod's soundbank">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      onClick={extractSoundWavs}
                      disabled={busy || !selectedPak || !gamePath}
                    >
                      <FileAudio size={15} />
                    </Button>
                  </Tip>
                )}
                {selectedEntries.size > 0 &&
                  selectedIsIoStore &&
                  !selectedIsVanilla &&
                  selectedUtocEntries.length > 0 && (
                    <Tip
                      content={`Convert ${selectedUtocEntries.length} selected mod assets to editable .uasset/.uexp legacy`}
                    >
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        className="relative"
                        onClick={exportLegacySelected}
                        disabled={busy || !selectedPak}
                      >
                        <FileOutput size={15} />
                        <span className="absolute -right-1 -top-1 flex h-3.5 min-w-3.5 items-center justify-center rounded-full bg-foreground px-1 text-[8px] font-bold leading-none text-background">
                          {selectedUtocEntries.length}
                        </span>
                      </Button>
                    </Tip>
                  )}
                {selectedEntries.size > 0 ? (
                  <Tip content={`Extract ${selectedEntries.size} selected files`}>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      className="relative"
                      onClick={extractSelected}
                      disabled={busy || !selectedPak}
                    >
                      <Download size={15} />
                      <span className="absolute -right-1 -top-1 flex h-3.5 min-w-3.5 items-center justify-center rounded-full bg-foreground px-1 text-[8px] font-bold leading-none text-background">
                        {selectedEntries.size}
                      </span>
                    </Button>
                  </Tip>
                ) : (
                  <>
                    {selectedIsIoStore && !selectedIsVanilla && (
                      <Tip content="Convert this mod's IoStore assets to editable .uasset/.uexp/.ubulk legacy">
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          onClick={exportLegacy}
                          disabled={busy || !selectedPak}
                        >
                          <FileOutput size={15} />
                        </Button>
                      </Tip>
                    )}
                    <Tip content="Extract all files">
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        onClick={unpackSelected}
                        disabled={busy || !selectedPak}
                      >
                        <Download size={15} />
                      </Button>
                    </Tip>
                  </>
                )}
              </div>
            </div>

            {/* Filter + select-all */}
            <div className="flex items-center gap-2">
              {selectedPak && (
                <Tip content={allVisibleSelected ? "Deselect all" : "Select all visible"}>
                  <button
                    className={cn(
                      "flex shrink-0 items-center text-muted-foreground hover:text-foreground",
                      visible.length === 0 && "invisible"
                    )}
                    onClick={toggleSelectAll}
                  >
                    {selectedEntries.size === 0 ? (
                      <Square size={14} />
                    ) : allVisibleSelected ? (
                      <CheckSquare2 size={14} />
                    ) : (
                      <MinusSquare size={14} />
                    )}
                  </button>
                </Tip>
              )}
              <div className="relative min-w-0 flex-1">
                <Search
                  size={13}
                  className="absolute left-2.5 top-1/2 -translate-y-1/2 text-muted-foreground"
                />
                <Input
                  className="h-8 pl-7 font-mono text-xs"
                  placeholder="Filter files…"
                  value={filterText}
                  onChange={(e) => onFilterChange(e.target.value)}
                  disabled={!selectedPak}
                />
              </div>
              <Select value={heroFilter} onValueChange={setHeroFilter} disabled={!selectedPak}>
                <Tip content="Filter assets by hero">
                  <SelectTrigger size="sm" className="w-42.5 px-2 text-[12px]">
                    <Users size={12} className="text-muted-foreground" />
                    <SelectValue placeholder="All heroes" />
                  </SelectTrigger>
                </Tip>
                <SelectContent className="max-h-72">
                  <SelectItem value="all">All heroes</SelectItem>
                  <SelectItem value="unknown">Unknown / no match</SelectItem>
                  {knownHeroes.map((h) => (
                    <SelectItem key={h.id} value={String(h.id)}>
                      <span className="flex items-center gap-2">
                        <HeroIcon characterId={h.id} name={h.name} size={16} />
                        {h.name}
                      </span>
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            {/* Category chip row */}
            {selectedPak && (
              <div className="flex flex-wrap items-center gap-1">
                <button
                  className="rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase text-muted-foreground hover:text-foreground"
                  onClick={() => setAllCategories(true)}
                  disabled={allCategoriesOn}
                >
                  All
                </button>
                <button
                  className="rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase text-muted-foreground hover:text-foreground"
                  onClick={() => setAllCategories(false)}
                  disabled={categoryFilter.size === 0}
                >
                  None
                </button>
                <span className="mx-1 h-3 w-px bg-border" />
                {ALL_CATEGORIES.filter((c) => categoryCounts[c] > 0).map((cat) => {
                  const active = categoryFilter.has(cat);
                  return (
                    <button
                      key={cat}
                      onClick={() => toggleCategory(cat)}
                      className={cn(
                        "flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase transition-opacity",
                        CATEGORY_BG_COLOR[cat],
                        CATEGORY_TEXT_COLOR[cat],
                        active ? "opacity-100" : "opacity-30 hover:opacity-60"
                      )}
                      title={`${CATEGORY_LABEL[cat]} (${categoryCounts[cat]})`}
                    >
                      <span>{CATEGORY_LABEL[cat]}</span>
                      <span className="text-[9px] opacity-70">{categoryCounts[cat]}</span>
                    </button>
                  );
                })}
              </div>
            )}
          </div>

          {/* File list — always rendered */}
          <div ref={contentsScrollRef} className="min-h-0 flex-1 overflow-y-auto">
            {!selectedPak ? (
              <div className="flex h-full flex-col items-center justify-center gap-2 p-4 text-center">
                <PackageOpen size={28} className="text-muted-foreground/40" />
                <p className="text-sm text-muted-foreground">
                  Select a pak from the left, or open one manually.
                </p>
              </div>
            ) : busy && visible.length === 0 ? (
              <div className="flex h-full flex-col items-center justify-center gap-2 p-4 text-center">
                <Loader2 size={20} className="animate-spin text-muted-foreground/60" />
                <p className="text-sm text-muted-foreground">Reading contents…</p>
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
                  const isChecked = selectedEntries.has(entry.path);
                  const fileName = entry.path.split("/").pop() ?? entry.path;
                  const folderPath = parentDir(entry.path);
                  const ext = fileName.includes(".") ? (fileName.split(".").pop() ?? "") : "";
                  const showExtractSelected = isChecked && selectedEntries.size > 1;
                  return (
                    <ContextMenu key={vRow.index}>
                      <ContextMenuTrigger asChild>
                        <div
                          className={cn(
                            "absolute left-0 top-0 flex w-full cursor-pointer items-center gap-2 border-b border-border/50 px-3",
                            isChecked ? "bg-secondary/80" : "hover:bg-secondary/50"
                          )}
                          style={{
                            height: `${vRow.size}px`,
                            transform: `translateY(${vRow.start}px)`,
                          }}
                          onClick={(e) => handleEntryClick(vRow.index, e)}
                          onDoubleClick={() => extractSingleEntry(entry)}
                          title={entry.path}
                        >
                          {isChecked ? (
                            <CheckSquare2 size={13} className="shrink-0 text-foreground" />
                          ) : (
                            <Square size={13} className="shrink-0 text-muted-foreground/50" />
                          )}
                          {(() => {
                            const cat = categoriesByPath.get(entry.path) ?? "other";
                            const heroIds = showHeroIcons ? heroesByPath.get(entry.path) : null;
                            if (heroIds && heroIds.length > 0) {
                              return (
                                <span className="flex shrink-0 items-center -space-x-1">
                                  {heroIds.slice(0, 2).map((id) => (
                                    <HeroIcon
                                      key={id}
                                      characterId={id}
                                      name={heroNameById.get(id)}
                                      size={16}
                                      className="ring-1 ring-background"
                                    />
                                  ))}
                                </span>
                              );
                            }
                            return (
                              <Tip content={CATEGORY_LABEL[cat]} side="top" align="start">
                                <span className={cn("shrink-0", CATEGORY_TEXT_COLOR[cat])}>
                                  <Package size={12} />
                                </span>
                              </Tip>
                            );
                          })()}
                          <span className="truncate font-mono text-[11px]">{entry.path}</span>
                        </div>
                      </ContextMenuTrigger>
                      <ContextMenuContent>
                        <ContextMenuItem
                          onSelect={() => {
                            if (showExtractSelected) {
                              const paths = [...selectedEntries].sort().join("\n");
                              copyToClipboard(paths, `${selectedEntries.size} paths`);
                            } else {
                              copyToClipboard(entry.path, "path");
                            }
                          }}
                        >
                          <Copy />
                          {showExtractSelected ? `Copy ${selectedEntries.size} Paths` : "Copy Path"}
                        </ContextMenuItem>
                        <ContextMenuItem
                          onSelect={() => {
                            if (showExtractSelected) {
                              const names = [...selectedEntries]
                                .sort()
                                .map((p) => p.split("/").pop() ?? p)
                                .join("\n");
                              copyToClipboard(names, `${selectedEntries.size} file names`);
                            } else {
                              copyToClipboard(fileName, "file name");
                            }
                          }}
                        >
                          <FileText />
                          {showExtractSelected
                            ? `Copy ${selectedEntries.size} File Names`
                            : "Copy File Name"}
                        </ContextMenuItem>
                        <ContextMenuItem
                          onSelect={() => {
                            if (showExtractSelected) {
                              const folders = [
                                ...new Set(
                                  [...selectedEntries].map((p) => parentDir(p)).filter(Boolean)
                                ),
                              ]
                                .sort()
                                .join("\n");
                              copyToClipboard(folders, "folder paths");
                            } else {
                              copyToClipboard(folderPath, "folder path");
                            }
                          }}
                          disabled={!showExtractSelected && !folderPath}
                        >
                          <Folder />
                          {showExtractSelected ? "Copy Folder Paths" : "Copy Folder Path"}
                        </ContextMenuItem>
                        <ContextMenuSeparator />
                        {showExtractSelected ? (
                          <ContextMenuItem onSelect={() => extractSelected()}>
                            <PackageOpen />
                            Extract {selectedEntries.size} Selected…
                          </ContextMenuItem>
                        ) : (
                          <ContextMenuSub>
                            <ContextMenuSubTrigger>
                              <Download />
                              Extract
                            </ContextMenuSubTrigger>
                            <ContextMenuSubContent>
                              <ContextMenuItem onSelect={() => extractSingleEntry(entry)}>
                                <FileText />
                                File only…
                              </ContextMenuItem>
                              <ContextMenuItem
                                onSelect={() => extractSingleEntryWithFolders(entry)}
                              >
                                <Folder />
                                With folder structure…
                              </ContextMenuItem>
                              {fileName.toLowerCase() === "bnk_ui_battle.bnk" && (
                                <ContextMenuItem onSelect={() => extractSoundWavs()}>
                                  <FileAudio />
                                  WAVs from BNK…
                                </ContextMenuItem>
                              )}
                            </ContextMenuSubContent>
                          </ContextMenuSub>
                        )}
                        {entry.source === "utoc" &&
                          !entry.path.endsWith(".ubulk") &&
                          !selectedIsVanilla && (
                            <ContextMenuItem
                              onSelect={() =>
                                selectedUtocEntries.length > 0
                                  ? exportLegacySelected()
                                  : exportLegacySingle(entry)
                              }
                            >
                              <FileOutput />
                              {selectedUtocEntries.length > 1
                                ? `Export ${selectedUtocEntries.length} Legacy (.uasset/.uexp)…`
                                : "Export Legacy (.uasset/.uexp)…"}
                            </ContextMenuItem>
                          )}
                        <ContextMenuSeparator />
                        <ContextMenuItem onSelect={() => selectByExtension(ext)} disabled={!ext}>
                          <Layers />
                          Select All .{ext || "ext"}
                        </ContextMenuItem>
                        {selectedEntries.size > 0 && (
                          <ContextMenuItem onSelect={clearEntrySelection}>
                            <XCircle />
                            Clear Selection
                          </ContextMenuItem>
                        )}
                      </ContextMenuContent>
                    </ContextMenu>
                  );
                })}
              </div>
            )}
          </div>

          {/* Footer */}
          {selectedPak && footerText && (
            <div className="shrink-0 border-t border-border bg-card px-3 py-1.5">
              <Tip content={footerText} side="top" align="start">
                <p className="truncate text-[11px] text-muted-foreground">{footerText}</p>
              </Tip>
            </div>
          )}
        </div>
      </div>

      <AlertDialog
        open={!!vanillaConfirm}
        onOpenChange={(open) => {
          if (!open) setVanillaConfirm(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Rebuild vanilla container?</AlertDialogTitle>
            <AlertDialogDescription>
              Builds a new <span className="font-mono">.utoc/.ucas/.pak</span> set (plus optional
              sibling if present) into the chosen output folder. Swap the files into Paks/ while the
              game is closed, with the signature bypass installed.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              autoFocus
              className={buttonVariants({ variant: "blue" })}
              onClick={runRebuildVanilla}
            >
              Rebuild
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={!!legacyConfirm}
        onOpenChange={(open) => {
          if (!open) setLegacyConfirm(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Large Extraction</AlertDialogTitle>
            <AlertDialogDescription>
              This container has{" "}
              <span className="font-semibold text-foreground">
                {displayLegacyConfirm?.count.toLocaleString()}
              </span>{" "}
              assets to convert. Legacy conversion decompresses every asset and may use significant
              disk space. Are you sure you want to continue?
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className={buttonVariants({ variant: "blue" })}
              onClick={() => {
                if (legacyConfirm) {
                  runLegacyExtraction(
                    legacyConfirm.utocPath,
                    legacyConfirm.outputDir,
                    legacyConfirm.filter
                  );
                }
                setLegacyConfirm(null);
              }}
            >
              Continue
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
