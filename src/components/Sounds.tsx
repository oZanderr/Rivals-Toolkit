import { useState, useCallback, useEffect, useRef } from "react";

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  ChevronDown,
  CheckCircle2,
  Crosshair,
  Crown,
  Flame,
  HandHelping,
  Heart,
  HeartHandshake,
  HeartPulse,
  Package,
  Shield,
  Skull,
  Sparkles,
  Star,
  Target,
  Trash2,
  Trophy,
  UploadCloud,
  UserCheck,
  UserX,
  XCircle,
  Zap,
} from "lucide-react";

import { GAIN_DEFAULT_DB, type SlotState, type WavValidation } from "./sounds/slot";
import { SoundRow } from "./sounds/SoundRow";

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
import { Badge } from "@/components/ui/badge";
import { Button, buttonVariants } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tip } from "@/components/ui/tooltip";
import { emitModsChanged } from "@/lib/modsEvents";
import { cn } from "@/lib/utils";

const SLOT_CONFIGS = [
  { key: "bodyshot_hit", label: "Bodyshot", category: "Combat", icon: <Shield size={15} /> },
  { key: "headshot_hit", label: "Headshot", category: "Combat", icon: <Crosshair size={15} /> },
  { key: "bodyshot_kill", label: "Bodyshot Kill", category: "Combat", icon: <Skull size={15} /> },
  { key: "headshot_kill", label: "Headshot Kill", category: "Combat", icon: <Target size={15} /> },
  {
    key: "killstreak_2k",
    label: "Double Kill",
    category: "Killstreaks",
    icon: <Flame size={15} />,
  },
  {
    key: "killstreak_3k",
    label: "Triple Kill",
    category: "Killstreaks",
    icon: <Zap size={15} />,
  },
  {
    key: "killstreak_4k",
    label: "Quad Kill",
    category: "Killstreaks",
    icon: <Sparkles size={15} />,
  },
  { key: "killstreak_5k", label: "Penta Kill", category: "Killstreaks", icon: <Star size={15} /> },
  { key: "killstreak_6k", label: "Hexa Kill", category: "Killstreaks", icon: <Trophy size={15} /> },
  { key: "killstreak_7k", label: "Septa Kill", category: "Killstreaks", icon: <Crown size={15} /> },
  { key: "heal_direct", label: "Heal Tick", category: "Healing", icon: <Heart size={15} /> },
  {
    key: "heal_pack_pickup",
    label: "Health Pack",
    category: "Healing",
    icon: <HeartPulse size={15} />,
  },
  {
    key: "kf_assist",
    label: "Kill Assist",
    category: "Kill Feed",
    icon: <HandHelping size={15} />,
  },
  {
    key: "kf_heal_to_kill",
    label: "Heal Assist",
    category: "Kill Feed",
    icon: <HeartHandshake size={15} />,
  },
  {
    key: "kf_teammate_kill",
    label: "Teammate Kill",
    category: "Kill Feed",
    icon: <UserCheck size={15} />,
  },
  {
    key: "kf_teammate_died",
    label: "Teammate Killed",
    category: "Kill Feed",
    icon: <UserX size={15} />,
  },
] as const;

type SlotKey = (typeof SLOT_CONFIGS)[number]["key"];
type SlotConfig = (typeof SLOT_CONFIGS)[number];

const SLOT_KEYS: SlotKey[] = SLOT_CONFIGS.map((c) => c.key);

const CATEGORIES: string[] = Array.from(new Set(SLOT_CONFIGS.map((c) => c.category)));

const EMPTY_SLOTS: Record<SlotKey, SlotState | null> = Object.fromEntries(
  SLOT_KEYS.map((k) => [k, null])
) as Record<SlotKey, SlotState | null>;

interface Props {
  gamePath: string;
  isActive: boolean;
}

type ResultState = { msg: string; ok: boolean; revealPath?: string } | null;

export function Sounds({ gamePath, isActive }: Props) {
  const [slots, setSlots] = useState<Record<SlotKey, SlotState | null>>(() => ({
    ...EMPTY_SLOTS,
  }));
  const [modName, setModName] = useState("");
  const [building, setBuilding] = useState(false);
  const [isDragging, setIsDragging] = useState(false);
  const [hoveredDropSlot, setHoveredDropSlot] = useState<SlotKey | null>(null);
  const [hoveredDropCategory, setHoveredDropCategory] = useState<string | null>(null);
  const [buildResult, setBuildResult] = useState<ResultState>(null);
  const [replaceConfirm, setReplaceConfirm] = useState<{
    modName: string;
    outputDir: string;
    pakPath: string;
  } | null>(null);
  const [expandedCategories, setExpandedCategories] = useState<Set<string>>(
    () => new Set(["Combat"])
  );

  function expandCategoryForSlot(key: SlotKey) {
    const cat = SLOT_CONFIGS.find((c) => c.key === key)?.category;
    if (!cat) return;
    setExpandedCategories((prev) => (prev.has(cat) ? prev : new Set(prev).add(cat)));
  }
  const lastReplaceConfirmRef = useRef(replaceConfirm);
  const displayReplaceConfirm = replaceConfirm ?? lastReplaceConfirmRef.current;
  const isActiveRef = useRef(isActive);
  const dropProcessingRef = useRef(false);
  const hoveredDropSlotRef = useRef<SlotKey | null>(null);
  const slotsRef = useRef(slots);

  useEffect(() => {
    slotsRef.current = slots;
  }, [slots]);

  useEffect(() => {
    isActiveRef.current = isActive;
  }, [isActive]);

  useEffect(() => {
    if (replaceConfirm) lastReplaceConfirmRef.current = replaceConfirm;
  }, [replaceConfirm]);

  function setSlot(key: SlotKey, value: SlotState | null) {
    setSlots((prev) => ({ ...prev, [key]: value }));
    if (value !== null) expandCategoryForSlot(key);
  }

  function setHoveredTarget(slot: SlotKey | null, cat: string | null) {
    hoveredDropSlotRef.current = slot;
    setHoveredDropSlot((prev) => (prev === slot ? prev : slot));
    setHoveredDropCategory((prev) => (prev === cat ? prev : cat));
  }

  const slotFromPosition = useCallback((position: { x: number; y: number }): SlotKey | null => {
    const dpr = window.devicePixelRatio || 1;
    const el = document.elementFromPoint(position.x / dpr, position.y / dpr) as HTMLElement | null;
    const attr = el?.closest("[data-drop-slot]")?.getAttribute("data-drop-slot") as SlotKey | null;
    return attr && SLOT_KEYS.includes(attr) ? attr : null;
  }, []);

  const categoryFromPosition = useCallback((position: { x: number; y: number }): string | null => {
    const dpr = window.devicePixelRatio || 1;
    const el = document.elementFromPoint(position.x / dpr, position.y / dpr) as HTMLElement | null;
    return el?.closest("[data-drop-category]")?.getAttribute("data-drop-category") ?? null;
  }, []);

  function normalizeModName(input: string): string {
    const trimmed = input.trim();
    const withoutExt = trimmed.replace(/\.pak$/i, "");
    const withoutVersionedSuffix = withoutExt.replace(/_9999999_P$/i, "");
    const withoutSuffix = withoutVersionedSuffix.replace(/_P$/i, "");
    return withoutSuffix.trim();
  }

  const validateAndSet = useCallback(async (path: string, key: SlotKey) => {
    const name = path.split(/[\\/]/).pop() ?? path;
    try {
      const validation = await invoke<WavValidation>("validate_wav", { path });
      const isCompatible =
        (validation.channels === 1 || validation.channels === 2) &&
        validation.bits_per_sample === 16;
      setSlots((prev) => ({
        ...prev,
        [key]: {
          path,
          name,
          validation,
          error: isCompatible
            ? null
            : `Incompatible format (${validation.bits_per_sample}-bit, ${validation.channels}ch)`,
          gainDb: prev[key]?.gainDb ?? GAIN_DEFAULT_DB,
        },
      }));
    } catch (e) {
      setSlots((prev) => ({
        ...prev,
        [key]: {
          path,
          name,
          validation: null,
          error: String(e),
          gainDb: prev[key]?.gainDb ?? GAIN_DEFAULT_DB,
        },
      }));
    }
    const cat = SLOT_CONFIGS.find((c) => c.key === key)?.category;
    if (cat) {
      setExpandedCategories((prev) => (prev.has(cat) ? prev : new Set(prev).add(cat)));
    }
  }, []);

  const distributeToCategory = useCallback(
    async (cat: string, paths: string[]) => {
      const catKeys = SLOT_CONFIGS.filter((c) => c.category === cat).map((c) => c.key);
      const empty = catKeys.filter((k) => !slotsRef.current[k]);
      if (empty.length === 0) return;

      const assignments: [SlotKey, string][] = [];
      const usedPaths = new Set<string>();
      const usedKeys = new Set<SlotKey>();

      for (const path of paths) {
        const base = (path.split(/[\\/]/).pop() ?? "").toLowerCase().replace(/\.(wav|ogg)$/, "");
        const match = empty.find((k) => !usedKeys.has(k) && base.includes(k));
        if (match) {
          assignments.push([match, path]);
          usedKeys.add(match);
          usedPaths.add(path);
        }
      }

      let cursor = 0;
      for (const path of paths) {
        if (usedPaths.has(path)) continue;
        while (cursor < empty.length && usedKeys.has(empty[cursor])) cursor++;
        if (cursor >= empty.length) break;
        assignments.push([empty[cursor], path]);
        usedKeys.add(empty[cursor]);
        cursor++;
      }

      for (const [key, path] of assignments) {
        await validateAndSet(path, key);
      }
    },
    [validateAndSet]
  );

  function setSlotGain(key: SlotKey, gainDb: number) {
    setSlots((prev) => {
      const slot = prev[key];
      if (!slot) return prev;
      return { ...prev, [key]: { ...slot, gainDb } };
    });
  }

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onDragDropEvent(async (event) => {
        if (event.payload.type === "enter" || event.payload.type === "over") {
          if (!isActiveRef.current) return;
          if (event.payload.type === "enter") setIsDragging(true);
          const slot = slotFromPosition(event.payload.position);
          const cat = slot ? null : categoryFromPosition(event.payload.position);
          setHoveredTarget(slot, cat);
        } else if (event.payload.type === "leave") {
          setIsDragging(false);
          setHoveredTarget(null, null);
        } else if (event.payload.type === "drop") {
          setIsDragging(false);
          const targetSlot = slotFromPosition(event.payload.position) ?? hoveredDropSlotRef.current;
          const targetCat = targetSlot ? null : categoryFromPosition(event.payload.position);
          setHoveredTarget(null, null);
          if (!isActiveRef.current || dropProcessingRef.current) return;

          const wavPaths = event.payload.paths.filter((p) => {
            const lower = p.toLowerCase();
            return lower.endsWith(".wav") || lower.endsWith(".ogg");
          });
          if (wavPaths.length === 0) return;
          if (!targetSlot && !targetCat) return;

          dropProcessingRef.current = true;
          try {
            if (targetSlot) {
              await validateAndSet(wavPaths[0], targetSlot);
            } else if (targetCat) {
              await distributeToCategory(targetCat, wavPaths);
            }
          } finally {
            dropProcessingRef.current = false;
          }
        }
      })
      .then((fn) => {
        unlisten = fn;
      });

    return () => {
      unlisten?.();
    };
  }, [validateAndSet, distributeToCategory, slotFromPosition, categoryFromPosition]);

  async function pickWav(key: SlotKey) {
    const selected = await open({
      multiple: false,
      filters: [{ name: "Audio Files", extensions: ["wav", "ogg"] }],
    });
    if (selected) {
      await validateAndSet(selected, key);
    }
  }

  async function buildMod() {
    const normalizedModName = normalizeModName(modName);
    const defaultOutputDir = `${gamePath}\\MarvelGame\\Marvel\\Content\\Paks\\~mods`;
    const selectedOutputDir = await open({
      directory: true,
      multiple: false,
      defaultPath: defaultOutputDir,
    });
    if (!selectedOutputDir || typeof selectedOutputDir !== "string") {
      return;
    }

    setBuildResult(null);

    const outputPakPath = `${selectedOutputDir}\\${normalizedModName}_9999999_P.pak`;
    const alreadyExists = await invoke<boolean>("path_exists", { path: outputPakPath });
    if (alreadyExists) {
      setReplaceConfirm({
        modName: normalizedModName,
        outputDir: selectedOutputDir,
        pakPath: outputPakPath,
      });
      return;
    }

    await runBuild(normalizedModName, selectedOutputDir, outputPakPath);
  }

  async function runBuild(normalizedModName: string, outputDir: string, outputPakPath: string) {
    const wavs: Record<string, { path: string; gain_db: number }> = {};
    for (const key of SLOT_KEYS) {
      const slot = slots[key];
      if (slot && !slot.error) {
        wavs[key] = { path: slot.path, gain_db: slot.gainDb };
      }
    }

    setBuilding(true);
    try {
      const result = await invoke<string>("build_sound_mod", {
        gameRoot: gamePath,
        wavs,
        modName: normalizedModName,
        outputDir,
      });
      setBuildResult({ msg: result, ok: true, revealPath: outputPakPath });
      emitModsChanged({ modsFolder: outputDir, source: "Sounds" });
    } catch (e) {
      setBuildResult({ msg: String(e), ok: false });
    } finally {
      setBuilding(false);
    }
  }

  useEffect(() => {
    if (!buildResult || !buildResult.ok) return;
    const timeoutId = window.setTimeout(() => setBuildResult(null), 4000);
    return () => window.clearTimeout(timeoutId);
  }, [buildResult]);

  const filledSlots = SLOT_KEYS.filter((k) => slots[k] !== null);
  const validCount = filledSlots.filter((k) => !slots[k]?.error).length;
  const hasAnyValid = validCount > 0;
  const hasErrors = filledSlots.some((k) => slots[k]?.error);
  const totalSlots = SLOT_KEYS.length;

  const canBuild = gamePath && hasAnyValid && !hasErrors && normalizeModName(modName).length > 0;

  const slotsByCategory: Record<string, SlotConfig[]> = CATEGORIES.reduce(
    (acc, cat) => {
      acc[cat] = SLOT_CONFIGS.filter((c) => c.category === cat);
      return acc;
    },
    {} as Record<string, SlotConfig[]>
  );

  function toggleCategory(cat: string) {
    setExpandedCategories((prev) => {
      const next = new Set(prev);
      if (next.has(cat)) next.delete(cat);
      else next.add(cat);
      return next;
    });
  }

  function clearCategory(cat: string) {
    setSlots((prev) => {
      const next = { ...prev };
      for (const c of slotsByCategory[cat]) next[c.key] = null;
      return next;
    });
  }

  return (
    <div className="flex min-h-0 w-full flex-1 flex-col gap-3">
      {/* Page header */}
      <div className="flex shrink-0 items-center justify-between gap-4 pt-0.5">
        <h2 className="text-xl font-bold">Sounds</h2>
        <div className="flex shrink-0 items-center gap-2">
          <Badge
            variant="outline"
            className={cn(
              "min-w-27.5 justify-center rounded-full px-2.5 py-1",
              hasErrors
                ? "border-red-accent-border bg-red-accent text-red-accent-foreground"
                : hasAnyValid
                  ? "border-green-accent-border bg-green-accent text-green-accent-foreground"
                  : "border-border bg-background text-muted-foreground"
            )}
          >
            {hasErrors
              ? "Validation issues"
              : hasAnyValid
                ? `${validCount}/${totalSlots} ready`
                : "Awaiting audio"}
          </Badge>
        </div>
      </div>

      {/* Category sections (scrollable middle) */}
      <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto pr-1">
        {CATEGORIES.map((cat) => {
          const configs = slotsByCategory[cat];
          const filled = configs.filter((c) => slots[c.key] !== null).length;
          const total = configs.length;
          const isExpanded = expandedCategories.has(cat);
          const isKillstreaks = cat === "Killstreaks";
          const showCategoryDrop = isDragging && hoveredDropCategory === cat && !hoveredDropSlot;
          const emptyInCat = configs.filter((c) => !slots[c.key]).length;
          return (
            <div
              key={cat}
              data-drop-category={cat}
              className={cn(
                "relative flex shrink-0 flex-col overflow-hidden rounded-md border border-border transition-colors",
                showCategoryDrop && "border-blue-accent-border ring-1 ring-blue-accent-border"
              )}
            >
              {showCategoryDrop && (
                <div className="pointer-events-none absolute inset-0 z-30 flex items-center justify-center gap-2 bg-background/92 backdrop-blur-sm">
                  <UploadCloud size={16} className="text-foreground" />
                  <span className="text-xs font-semibold text-foreground">
                    {emptyInCat > 0
                      ? `Drop audio to fill ${emptyInCat} empty ${cat} slot${emptyInCat === 1 ? "" : "s"}`
                      : `${cat} has no empty slots`}
                  </span>
                </div>
              )}
              <button
                type="button"
                onClick={() => toggleCategory(cat)}
                className="flex items-center justify-between gap-2 border-b border-border bg-card px-3 py-2 text-left transition-colors hover:bg-secondary/50"
              >
                <div className="flex items-center gap-2">
                  <ChevronDown
                    size={14}
                    className={cn(
                      "shrink-0 text-muted-foreground transition-transform",
                      !isExpanded && "-rotate-90"
                    )}
                  />
                  <h3 className="text-sm font-semibold">{cat}</h3>
                  <Badge
                    variant="outline"
                    className={cn(
                      "rounded-full px-2 py-0 text-[10px]",
                      filled > 0
                        ? "border-green-accent-border bg-green-accent text-green-accent-foreground"
                        : "border-border bg-background text-muted-foreground"
                    )}
                  >
                    {filled}/{total}
                  </Badge>
                </div>
                <Tip content="Clear all in this category" disabled={filled === 0}>
                  <Button
                    size="icon-xs"
                    variant="ghost"
                    onClick={(e) => {
                      e.stopPropagation();
                      clearCategory(cat);
                    }}
                    disabled={building || filled === 0}
                    className={cn(
                      "text-muted-foreground hover:text-err",
                      filled === 0 && "invisible"
                    )}
                  >
                    <Trash2 size={13} />
                  </Button>
                </Tip>
              </button>
              {isExpanded && (
                <div
                  className={cn(
                    isKillstreaks
                      ? "grid grid-cols-1 xl:grid-cols-2 xl:divide-x xl:divide-border/50"
                      : "flex flex-col"
                  )}
                >
                  {configs.map((config) => (
                    <SoundRow
                      key={config.key}
                      slotKey={config.key}
                      label={config.label}
                      icon={config.icon}
                      slot={slots[config.key]}
                      onPick={() => pickWav(config.key)}
                      onClear={() => setSlot(config.key, null)}
                      onGainChange={(db) => setSlotGain(config.key, db)}
                      disabled={building}
                      showDropOverlay={isDragging && hoveredDropSlot === config.key}
                      onDragOverRow={() => setHoveredTarget(config.key, null)}
                    />
                  ))}
                </div>
              )}
            </div>
          );
        })}
      </div>

      {/* Build section (fixed at bottom) */}
      <div className="flex shrink-0 flex-col overflow-hidden rounded-md border border-border">
        <div className="border-b border-border bg-card px-3 py-2">
          <h3 className="text-sm font-semibold">Build</h3>
        </div>
        <div className="flex flex-col gap-3 p-3">
          <div className="relative">
            <Input
              value={modName}
              onChange={(e) => {
                setModName(e.target.value);
                setBuildResult(null);
              }}
              className="h-9 pr-28"
              placeholder="Enter mod name"
              disabled={building}
            />
            <span className="pointer-events-none absolute right-3 top-1/2 -translate-y-1/2 select-none text-xs text-muted-foreground">
              _9999999_P.pak
            </span>
          </div>
          <div>
            <Button
              variant="blue"
              disabled={!canBuild || building}
              onClick={buildMod}
              className="h-10 w-full gap-2"
            >
              {building ? <Package size={14} className="animate-spin" /> : <Package size={14} />}
              {building ? "Building..." : "Build Sound Mod"}
            </Button>
          </div>
          {buildResult && (
            <Tip content="Click to reveal in explorer" disabled={!buildResult.revealPath}>
              <div
                className={cn(
                  "flex items-center gap-1.5 text-xs",
                  buildResult.ok ? "text-green-accent-foreground" : "text-red-accent-foreground",
                  buildResult.revealPath && "cursor-pointer hover:underline"
                )}
                onClick={
                  buildResult.revealPath
                    ? () => revealItemInDir(buildResult.revealPath!)
                    : undefined
                }
              >
                {buildResult.ok ? (
                  <CheckCircle2 size={13} className="shrink-0" />
                ) : (
                  <XCircle size={13} className="shrink-0" />
                )}
                <span className="truncate">{buildResult.msg}</span>
              </div>
            </Tip>
          )}
        </div>
      </div>

      {/* Game not detected warning */}
      {!gamePath && (
        <div className="flex items-center gap-2 rounded-md border border-red-accent-border bg-red-accent px-3 py-2.5 text-xs text-red-accent-foreground">
          Game not detected. Set your install path in Settings first.
        </div>
      )}

      <AlertDialog
        open={!!replaceConfirm}
        onOpenChange={(isOpen) => {
          if (!isOpen) setReplaceConfirm(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Replace Existing Mod</AlertDialogTitle>
            <AlertDialogDescription>
              <span className="font-semibold text-foreground">
                {displayReplaceConfirm?.modName}_9999999_P.pak
              </span>{" "}
              already exists in this folder. Do you want to replace it?
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className={buttonVariants({ variant: "blue" })}
              onClick={() => {
                if (replaceConfirm) {
                  runBuild(
                    replaceConfirm.modName,
                    replaceConfirm.outputDir,
                    replaceConfirm.pakPath
                  );
                }
                setReplaceConfirm(null);
              }}
            >
              Replace
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
