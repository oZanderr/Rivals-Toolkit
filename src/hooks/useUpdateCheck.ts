import { useEffect, useState } from "react";

import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";

export interface UpdateInfo {
  update_available: boolean;
  latest_version: string;
  current_version: string;
  release_url: string;
  release_notes: string | null;
}

export function useUpdateCheck() {
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const autoCheck = await invoke<boolean>("get_auto_check_updates");
        if (!autoCheck || cancelled) return;
        const currentVersion = await getVersion();
        const info = await invoke<UpdateInfo>("check_for_update", {
          currentVersion,
        });
        if (!cancelled && info.update_available) {
          setUpdateInfo(info);
        }
      } catch {
        // Network failure or API error — silent skip
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  return updateInfo;
}
