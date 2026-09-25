import { useEffect, useState } from "react";

import { invoke } from "@tauri-apps/api/core";

import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { cn } from "@/lib/utils";

interface PackageReport {
  path: string;
  kind: string;
  overrides_game: boolean;
  error?: string;
}

type ByPackage = Record<string, string[]>;

interface ModReport {
  container: string;
  patch_priority: number | null;
  packages: PackageReport[];
  runtime_natives: ByPackage;
  python_classes: ByPackage;
  files: ByPackage;
  save_slots: ByPackage;
  urls: ByPackage;
}

interface Props {
  gamePath: string;
  modName: string;
  onClose: () => void;
}

const SECTIONS: { key: keyof ModReport; title: string; hint: string }[] = [
  {
    key: "runtime_natives",
    title: "Runtime natives",
    hint: "Functions the game registers only at runtime, such as its pak hot-patch utility.",
  },
  {
    key: "python_classes",
    title: "Python classes",
    hint: "Game classes backed by its embedded Python, which a game update can rename.",
  },
  {
    key: "files",
    title: "Files",
    hint: "Loose files a script names, read or written next to the game.",
  },
  { key: "save_slots", title: "Save slots", hint: "SaveGame slots the mod keeps state in." },
  { key: "urls", title: "Links", hint: "Web addresses a script opens." },
];

/** Mount with `key={modName}` so each mod starts from an empty report. */
export function ModReportDialog({ gamePath, modName, onClose }: Props) {
  const [report, setReport] = useState<ModReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    invoke<ModReport>("get_mod_report", { gameRoot: gamePath, modName })
      .then((r) => {
        if (!cancelled) setReport(r);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [gamePath, modName]);

  const overrides = report?.packages.filter((p) => p.overrides_game) ?? [];
  const added = report?.packages.filter((p) => !p.overrides_game) ?? [];

  return (
    <AlertDialog open onOpenChange={(open) => !open && onClose()}>
      <AlertDialogContent className="max-w-2xl max-h-[80vh] flex flex-col">
        <AlertDialogHeader>
          <AlertDialogTitle>Mod contents</AlertDialogTitle>
          <AlertDialogDescription className="truncate">
            {modName}
            {report?.patch_priority != null && ` · patch priority ${report.patch_priority}`}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <div className="flex-1 overflow-y-auto -mx-6 px-6 text-[12px]">
          {error && <p className="text-err">{error}</p>}
          {!error && !report && <p className="text-muted-foreground">Reading the mod…</p>}
          {report && (
            <div className="flex flex-col gap-4">
              <PackageList
                title={`Replaces game assets (${overrides.length})`}
                packages={overrides}
              />
              <PackageList title={`Adds (${added.length})`} packages={added} />
              {SECTIONS.map(({ key, title, hint }) => {
                const section = report[key] as ByPackage;
                const entries = Object.entries(section);
                if (entries.length === 0) return null;
                return (
                  <section key={key}>
                    <h3 className="font-semibold">{title}</h3>
                    <p className="mb-1 text-[11px] text-muted-foreground">{hint}</p>
                    {entries.map(([pkg, values]) => (
                      <div key={pkg} className="mb-1.5">
                        <p className="truncate font-mono text-[11px] text-muted-foreground">
                          {shortPath(pkg)}
                        </p>
                        {values.map((v) => (
                          <p key={v} className="truncate pl-3 font-mono text-[11px]">
                            {v}
                          </p>
                        ))}
                      </div>
                    ))}
                  </section>
                );
              })}
            </div>
          )}
        </div>
        <AlertDialogFooter>
          <AlertDialogCancel>Close</AlertDialogCancel>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

function PackageList({ title, packages }: { title: string; packages: PackageReport[] }) {
  if (packages.length === 0) return null;
  return (
    <section>
      <h3 className="mb-1 font-semibold">{title}</h3>
      {packages.map((p) => (
        <div key={p.path} className="flex items-center gap-2">
          <span
            className={cn(
              "w-32 shrink-0 truncate text-[11px]",
              p.error ? "text-warn" : "text-muted-foreground"
            )}
            title={p.error}
          >
            {p.kind}
          </span>
          <span className="truncate font-mono text-[11px]" title={p.path}>
            {shortPath(p.path)}
          </span>
        </div>
      ))}
    </section>
  );
}

/** Drops the `<Project>/Content/` prefix every cooked path starts with. */
function shortPath(path: string): string {
  const at = path.indexOf("/Content/");
  return at >= 0 ? path.slice(at + "/Content/".length) : path;
}
