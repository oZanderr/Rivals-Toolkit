import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import type { Extension } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";

/**
 * The look every code editor in the app shares: the app's own colours, a monospace face on fixed
 * rows, and no outline. `gutters` shows the line numbers a text with errors by line wants.
 */
export function editorTheme({ gutters = false }: { gutters?: boolean } = {}): Extension {
  return EditorView.theme({
    "&": {
      height: "100%",
      fontSize: "13px",
      backgroundColor: "var(--color-background)",
    },
    ".cm-content": {
      fontFamily: "ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, monospace",
      caretColor: "var(--color-foreground)",
      color: "var(--color-foreground)",
      // Pinned to an integer pixel value so every row renders at the same height; a unitless
      // multiplier (e.g. 1.625) yields 21.125px which the browser rounds inconsistently between
      // rows.
      lineHeight: "21px",
      padding: "16px 0",
    },
    ".cm-line": {
      padding: "0 16px",
    },
    "&.cm-focused .cm-cursor": {
      borderLeftColor: "var(--color-foreground)",
    },
    "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": {
      backgroundColor: "hsl(215 60% 40% / 0.4)",
    },
    ".cm-gutters": gutters
      ? {
          backgroundColor: "var(--color-background)",
          color: "var(--color-muted-foreground)",
          border: "none",
          fontFamily: "ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, monospace",
        }
      : { display: "none" },
    ".cm-gutterElement": {
      lineHeight: "21px",
    },
    ".cm-scroller": {
      overflow: "auto",
    },
    "&.cm-focused": {
      outline: "none",
    },
    ".cm-search-match": {
      backgroundColor: "hsl(210 80% 60% / 0.35)",
    },
    ".cm-search-match-current": {
      backgroundColor: "hsl(210 80% 60% / 0.7)",
    },
  });
}

/** Typing, undo and redo, as every editor here has them. */
export function editingKeys(): Extension {
  return [keymap.of([...defaultKeymap, ...historyKeymap]), history()];
}
