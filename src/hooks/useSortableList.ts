import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent,
} from "react";

/** How far a press moves before it is a drag rather than a click. */
const THRESHOLD = 4;

/** What a drag in progress shows. */
interface Drag {
  from: number;
  to: number;
  /** How far the dragged row has moved. */
  dy: number;
  /** One row and the gap after it, which the rows it passes over move aside by. */
  size: number;
}

/** A press on a handle, read only by the handlers. */
interface Press {
  pointerId: number;
  index: number;
  startY: number;
  /** Where each row sat when the press began. */
  rects: DOMRect[];
  started: boolean;
  to: number;
}

export interface SortableList {
  /** Props for row `index`, the element that moves. */
  item: (index: number) => {
    ref: (el: HTMLElement | null) => void;
    style: CSSProperties | undefined;
    "data-dragging": boolean | undefined;
  };
  /** Props for the element row `index` is dragged by. */
  handle: (index: number) => {
    ref: (el: HTMLElement | null) => void;
    onPointerDown: (e: PointerEvent<HTMLElement>) => void;
    onPointerMove: (e: PointerEvent<HTMLElement>) => void;
    onPointerUp: (e: PointerEvent<HTMLElement>) => void;
    onPointerCancel: () => void;
    onLostPointerCapture: () => void;
    onClickCapture: (e: MouseEvent<HTMLElement>) => void;
    onKeyDown: (e: KeyboardEvent<HTMLElement>) => void;
  };
  draggingIndex: number | null;
}

/**
 * Reorders a list of `count` rows by dragging a handle up or down, or with Alt+Up and Alt+Down on
 * it, telling `onMove` where a row went. Pointer events rather than HTML drag and drop, which the
 * window's own file drop takes over.
 */
export function useSortableList(
  count: number,
  onMove: (from: number, to: number) => void
): SortableList {
  const items = useRef<(HTMLElement | null)[]>([]);
  const handles = useRef<(HTMLElement | null)[]>([]);
  const press = useRef<Press | null>(null);
  const swallowClick = useRef(false);
  const focusAfterMove = useRef<number | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);

  const cancel = useCallback(() => {
    press.current = null;
    setDrag(null);
  }, []);

  useEffect(() => {
    if (!drag) return;
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      swallowClick.current = true;
      cancel();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [drag, cancel]);

  // A row moved from the keyboard keeps the focus, which React's reordering can drop.
  useLayoutEffect(() => {
    if (focusAfterMove.current === null) return;
    handles.current[focusAfterMove.current]?.focus();
    focusAfterMove.current = null;
  });

  const item: SortableList["item"] = (index) => {
    let style: CSSProperties | undefined;
    if (drag) {
      const { from, to, dy, size } = drag;
      if (index === from) {
        style = { transform: `translateY(${dy}px)`, position: "relative", zIndex: 10 };
      } else {
        const shift =
          from < to && index > from && index <= to
            ? -size
            : from > to && index >= to && index < from
              ? size
              : 0;
        style = { transform: `translateY(${shift}px)`, transition: "transform 150ms ease" };
      }
    }
    return {
      ref: (el) => {
        items.current[index] = el;
      },
      style,
      "data-dragging": drag?.from === index || undefined,
    };
  };

  const handle: SortableList["handle"] = (index) => ({
    ref: (el) => {
      handles.current[index] = el;
    },
    onPointerDown: (e) => {
      if (e.button !== 0 || count < 2) return;
      swallowClick.current = false;
      press.current = {
        pointerId: e.pointerId,
        index,
        startY: e.clientY,
        rects: items.current
          .slice(0, count)
          .map((el) => el?.getBoundingClientRect() ?? new DOMRect()),
        started: false,
        to: index,
      };
      e.currentTarget.setPointerCapture(e.pointerId);
    },
    onPointerMove: (e) => {
      const held = press.current;
      if (!held || held.pointerId !== e.pointerId) return;
      const moved = e.clientY - held.startY;
      if (!held.started && Math.abs(moved) < THRESHOLD) return;
      held.started = true;
      const { rects } = held;
      const own = rects[held.index];
      const dy = Math.max(
        rects[0].top - own.top,
        Math.min(rects[rects.length - 1].bottom - own.bottom, moved)
      );
      const centre = own.top + own.height / 2 + dy;
      held.to = rects.filter((r, at) => at !== held.index && r.top + r.height / 2 < centre).length;
      const size = rects.length > 1 ? rects[1].top - rects[0].top : own.height;
      setDrag({ from: held.index, to: held.to, dy, size });
    },
    onPointerUp: (e) => {
      const held = press.current;
      if (!held || held.pointerId !== e.pointerId) return;
      press.current = null;
      if (!held.started) return;
      swallowClick.current = true;
      setDrag(null);
      if (held.to !== held.index) onMove(held.index, held.to);
    },
    onPointerCancel: cancel,
    onLostPointerCapture: () => {
      if (press.current) cancel();
    },
    onClickCapture: (e) => {
      if (!swallowClick.current) return;
      swallowClick.current = false;
      e.preventDefault();
      e.stopPropagation();
    },
    onKeyDown: (e) => {
      if (!e.altKey || (e.key !== "ArrowUp" && e.key !== "ArrowDown")) return;
      e.preventDefault();
      const to = e.key === "ArrowUp" ? index - 1 : index + 1;
      if (to < 0 || to >= count) return;
      focusAfterMove.current = to;
      onMove(index, to);
    },
  });

  return { item, handle, draggingIndex: drag?.from ?? null };
}
