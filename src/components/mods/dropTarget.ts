/** The folder below `~mods` a drop at `position` lands in: the folder row under it, the folder of
 *  the mod row under it, or "" for `~mods` itself. Tauri gives the position in physical pixels. */
export function dropFolderAt(position: { x: number; y: number }): string {
  const scale = window.devicePixelRatio || 1;
  const hit = document.elementFromPoint(position.x / scale, position.y / scale);
  return hit?.closest<HTMLElement>("[data-drop-folder]")?.dataset.dropFolder ?? "";
}
