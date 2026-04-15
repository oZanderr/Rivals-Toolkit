import { useCallback, useEffect, useRef, useState } from "react";

import { invoke } from "@tauri-apps/api/core";

const POLL_MS = 3000;
const OPTIMISTIC_TIMEOUT_MS = 60_000;

export function useGameRunning(active: boolean) {
  const [polled, setPolled] = useState<boolean | null>(null);
  const [optimistic, setOptimistic] = useState(false);
  const optimisticTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearOptimistic = useCallback(() => {
    if (optimisticTimer.current) {
      clearTimeout(optimisticTimer.current);
      optimisticTimer.current = null;
    }
    setOptimistic(false);
  }, []);

  const markLaunched = useCallback(() => {
    setOptimistic(true);
    if (optimisticTimer.current) clearTimeout(optimisticTimer.current);
    optimisticTimer.current = setTimeout(() => {
      optimisticTimer.current = null;
      setOptimistic(false);
    }, OPTIMISTIC_TIMEOUT_MS);
  }, []);

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    async function poll() {
      try {
        const running = await invoke<boolean>("get_game_running");
        if (cancelled) return;
        setPolled(running);
        if (running) clearOptimistic();
      } catch {
        // ignore — backend guards are source of truth
      }
    }
    poll();
    const id = setInterval(poll, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [active, clearOptimistic]);

  useEffect(
    () => () => {
      if (optimisticTimer.current) clearTimeout(optimisticTimer.current);
    },
    []
  );

  // Suppress isRunning until the first poll has completed to avoid a flash of
  // "game is running" UI before the backend has actually been queried.
  const ready = polled !== null;
  return { isRunning: ready && (polled === true || optimistic), ready, markLaunched };
}
