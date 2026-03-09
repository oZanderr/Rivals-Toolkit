import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, X } from "lucide-react";

const appWindow = getCurrentWindow();

export function Titlebar() {
  return (
    <div
      data-tauri-drag-region
      className="fixed left-0 right-0 top-0 z-50 flex h-[30px] items-center justify-end"
    >
      <button
        onClick={() => appWindow.minimize()}
        className="inline-flex h-[30px] w-[30px] items-center justify-center text-muted-foreground transition-colors hover:bg-secondary hover:text-foreground"
        title="Minimize"
      >
        <Minus size={14} strokeWidth={2} />
      </button>
      <button
        onClick={() => appWindow.close()}
        className="inline-flex h-[30px] w-[30px] items-center justify-center rounded-tr-[8px] text-muted-foreground transition-colors hover:bg-red-600 hover:text-white"
        title="Close"
      >
        <X size={14} strokeWidth={2} />
      </button>
    </div>
  );
}
