import { useCallback, useEffect, useState } from "react";

import { invoke } from "@tauri-apps/api/core";

import { DEFAULT_TAB_LAYOUT, normalizeTabLayout, type TabLayout } from "@/lib/tabs";

/**
 * The sidebar's tab layout: `null` until the stored one has loaded, then the user's, or the
 * default when none reads. A change shows at once and is saved behind it.
 */
export function useTabLayout(): {
  layout: TabLayout | null;
  setLayout: (next: TabLayout) => void;
} {
  const [layout, setLayoutState] = useState<TabLayout | null>(null);

  useEffect(() => {
    let cancelled = false;
    invoke<{ order?: unknown; hidden?: unknown }>("get_tab_layout")
      .then((stored) => {
        if (!cancelled) setLayoutState(normalizeTabLayout(stored ?? {}));
      })
      .catch((e: unknown) => {
        console.error(e);
        if (!cancelled) setLayoutState(DEFAULT_TAB_LAYOUT);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const setLayout = useCallback((next: TabLayout) => {
    setLayoutState(next);
    invoke("set_tab_layout", { layout: next }).catch(console.error);
  }, []);

  return { layout, setLayout };
}
