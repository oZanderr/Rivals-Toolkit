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
