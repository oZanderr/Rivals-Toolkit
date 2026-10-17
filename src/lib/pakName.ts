/** Mirrors `normalize_pak_filename` in rivals-core: what a typed mod name becomes on disk. */
export function previewPakFilename(raw: string): string {
  const trimmed = raw
    .trim()
    .replace(/[<>:"/\\|?*]/g, "")
    .replace(/\.pak$/i, "")
    .replace(/_9999999_P$/i, "");
  if (!trimmed) return "";
  return `${trimmed}_9999999_P.pak`;
}

/** The same name with the extension the chosen target actually writes. An IoStore save writes a
 *  .utoc beside its .ucas and a stub .pak; the .utoc is the one that names the mod. */
export function previewContainerFilename(raw: string, target: "io_store" | "pak"): string {
  const pak = previewPakFilename(raw);
  if (!pak) return "";
  return target === "io_store" ? pak.replace(/\.pak$/, ".utoc") : pak;
}

/** A mod in `~mods`, as `get_mods_status` lists it. */
export interface ModEntry {
  full_name: string;
  display_name: string;
  enabled: boolean;
  has_companions: boolean;
  kind: "Pak" | "IoStore";
}

/** The mod names a save can be pointed at: enabled mods of the kind the save writes, IoStore
 *  containers or plain paks, at the top of `~mods` and named as the normaliser names them, so
 *  picking one really does target that mod. An IoStore mod's stub .pak is no plain pak to write
 *  over. Sorted, each once. */
export function saveTargetNames(mods: ModEntry[], target: "io_store" | "pak"): string[] {
  const stems = mods
    .filter(
      (mod) =>
        mod.enabled &&
        (target === "io_store"
          ? mod.kind === "IoStore"
          : mod.kind === "Pak" && !mod.has_companions) &&
        !mod.full_name.includes("/") &&
        /_9999999_P\.pak$/i.test(mod.display_name)
    )
    .map((mod) => mod.display_name.replace(/_9999999_P\.pak$/i, ""));
  return [...new Set(stems)].sort((a, b) => a.localeCompare(b));
}
