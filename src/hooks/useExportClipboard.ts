import { useCallback, useEffect, useState } from "react";

/**
 * An export marked for pasting into another package. Kept in `sessionStorage` so it survives
 * moving between assets, which is the whole point: the source package is closed by the time the
 * destination is open.
 */
export interface ExportClipboard {
  container: string;
  entry: string;
  export: number;
  /** For showing what is held without reopening the source. */
  name: string;
  className: string;
  path: string;
}

const KEY = "rivals-toolkit.export-clipboard";
const CHANGED = "rivals-toolkit.export-clipboard-changed";

function read(): ExportClipboard | null {
  try {
    const held = sessionStorage.getItem(KEY);
    return held ? (JSON.parse(held) as ExportClipboard) : null;
  } catch {
    return null;
  }
}

/** What is marked for pasting, and the two ways to change it. */
export function useExportClipboard(): {
  held: ExportClipboard | null;
  copy: (entry: ExportClipboard) => void;
  clear: () => void;
} {
  const [held, setHeld] = useState<ExportClipboard | null>(read);

  // Another view of the same window changes it too, and storage events do not fire on the window
  // that wrote them, so the write announces itself.
  useEffect(() => {
    const onChanged = () => setHeld(read());
    window.addEventListener(CHANGED, onChanged);
    window.addEventListener("storage", onChanged);
    return () => {
      window.removeEventListener(CHANGED, onChanged);
      window.removeEventListener("storage", onChanged);
    };
  }, []);

  const copy = useCallback((entry: ExportClipboard) => {
    try {
      sessionStorage.setItem(KEY, JSON.stringify(entry));
    } catch {
      // A browser with storage turned off keeps it for this view only, which is still useful.
    }
    setHeld(entry);
    window.dispatchEvent(new Event(CHANGED));
  }, []);

  const clear = useCallback(() => {
    try {
      sessionStorage.removeItem(KEY);
    } catch {
      // Nothing to undo.
    }
    setHeld(null);
    window.dispatchEvent(new Event(CHANGED));
  }, []);

  return { held, copy, clear };
}
