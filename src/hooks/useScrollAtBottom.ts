import { useEffect, useState } from "react";

/**
 * Returns callback refs for the scroll container and a sentinel placed at the
 * very end of scrollable content. `atBottom` flips when the sentinel is in view.
 *
 * Callback refs (not useRef) so the IntersectionObserver re-initializes when
 * the elements actually mount, including after async-load early returns.
 */
export function useScrollAtBottom(): {
  atBottom: boolean;
  scrollRef: (node: HTMLElement | null) => void;
  sentinelRef: (node: HTMLDivElement | null) => void;
} {
  const [atBottom, setAtBottom] = useState(false);
  const [scrollEl, setScrollEl] = useState<HTMLElement | null>(null);
  const [sentinelEl, setSentinelEl] = useState<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!scrollEl || !sentinelEl) return;
    const io = new IntersectionObserver(
      (entries) => {
        const entry = entries[0];
        if (entry) setAtBottom(entry.isIntersecting);
      },
      { root: scrollEl, threshold: 0 }
    );
    io.observe(sentinelEl);
    return () => io.disconnect();
  }, [scrollEl, sentinelEl]);

  return { atBottom, scrollRef: setScrollEl, sentinelRef: setSentinelEl };
}
