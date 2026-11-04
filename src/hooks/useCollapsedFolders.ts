import { useCallback, useEffect, useRef, useState } from "react";

import { invoke } from "@tauri-apps/api/core";

/**
 * The Mods page's collapsed folders, by their paths below `~mods`. Every folder shows open until
 * the stored set loads, and a change shows at once and is saved behind it.
 */
export function useCollapsedFolders(): {
  collapsed: ReadonlySet<string>;
  updateCollapsed: (change: (prev: ReadonlySet<string>) => ReadonlySet<string>) => void;
} {
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  const current = useRef<ReadonlySet<string>>(collapsed);
  const changed = useRef(false);

  useEffect(() => {
    let cancelled = false;
    invoke<string[]>("get_collapsed_mod_folders")
      .then((stored) => {
        // A folder toggled before the stored set arrived is the newer word.
        if (cancelled || changed.current) return;
        current.current = new Set(stored);
        setCollapsed(current.current);
      })
      .catch(console.error);
    return () => {
      cancelled = true;
    };
  }, []);

  const updateCollapsed = useCallback(
    (change: (prev: ReadonlySet<string>) => ReadonlySet<string>) => {
      const next = change(current.current);
      const prev = current.current;
      if (next.size === prev.size && [...next].every((path) => prev.has(path))) return;
      changed.current = true;
      current.current = next;
      setCollapsed(next);
      invoke("set_collapsed_mod_folders", { folders: [...next].sort() }).catch(console.error);
    },
    []
  );

  return { collapsed, updateCollapsed };
}
