import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, Square, X } from "lucide-react";

import icon from "@/assets/icon.png";

const appWindow = getCurrentWindow();

export function Titlebar() {
  const handleTitleMouseDown = async (e: React.MouseEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    try {
      await appWindow.startDragging();
    } catch {
      // Fallback: data-tauri-drag-region still handles drag on supported targets.
    }
  };

  return (
    <div
      data-tauri-drag-region
      className="flex h-8.75 w-full shrink-0 items-center justify-end border-b border-border bg-card"
    >
      <div
        data-tauri-drag-region
        onMouseDown={handleTitleMouseDown}
        className="flex flex-1 items-center gap-2 px-3 select-none"
      >
        <img src={icon} alt="" className="size-4 shrink-0" />
        <span className="text-sm font-semibold leading-none text-foreground">Rivals Toolkit</span>
        <span className="text-sm leading-none text-muted-foreground">|</span>
        <span className="text-sm leading-none text-muted-foreground">
          Marvel Rivals Modding Suite
        </span>
      </div>
      <button
        onClick={() => appWindow.minimize()}
        className="inline-flex h-8.75 w-8.75 items-center justify-center text-muted-foreground transition-colors hover:bg-secondary hover:text-foreground"
        title="Minimize"
      >
        <Minus size={14} strokeWidth={2} />
      </button>
      <button
        onClick={() => appWindow.toggleMaximize()}
        className="inline-flex h-8.75 w-8.75 items-center justify-center text-muted-foreground transition-colors hover:bg-secondary hover:text-foreground"
        title="Maximize"
      >
        <Square size={11} strokeWidth={2} />
      </button>
      <button
        onClick={() => appWindow.close()}
        className="inline-flex h-8.75 w-8.75 items-center justify-center rounded-tr text-muted-foreground transition-colors hover:bg-red-600 hover:text-white"
        title="Close"
      >
        <X size={14} strokeWidth={2} />
      </button>
    </div>
  );
}
