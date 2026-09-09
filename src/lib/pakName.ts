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
