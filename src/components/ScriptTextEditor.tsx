import { useEffect, useRef, useState } from "react";

import { RangeSetBuilder, StateEffect, StateField } from "@codemirror/state";
import {
  Decoration,
  EditorView,
  MatchDecorator,
  ViewPlugin,
  lineNumbers,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle, CheckCircle2, CornerUpLeft, Plus, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { editingKeys, editorTheme } from "@/lib/codeEditor";

/** A problem with the text, where it is. */
export interface TextDiagnostic {
  line: number;
  column: number;
  message: string;
}

/** One change a save makes, as it reports it. */
interface AppliedChange {
  name: string;
  before: string;
  after: string;
}

/** What writing a function anew from text would do: the text's problems by line, or the changes
 *  a save would make, why it would refuse, and what it would need leave for. */
export interface TextPreview {
  diagnostics: TextDiagnostic[];
  warnings: TextDiagnostic[];
  refused?: string;
  unchecked?: string;
  applied: AppliedChange[];
}

/** How long typing has to stop before the text is assembled again. */
const PREVIEW_DELAY_MS = 600;
/** How long typing has to stop before the text is kept as a draft. */
const DRAFT_DELAY_MS = 300;

// ── Highlighting ────────────────────────────────────────────────────

/** A comment, a label, a string, a quoted name, and a value written raw, in that order. */
const TOKEN =
  /(;[^\n]*)|(@[A-Za-z0-9_]+:?)|(u?"(?:[^"\\\n]|\\.)*")|('(?:[^'\\\n]|\\.)*')|(#-?\w+)/g;

const marks = {
  comment: Decoration.mark({ class: "cm-st-comment" }),
  label: Decoration.mark({ class: "cm-st-label" }),
  string: Decoration.mark({ class: "cm-st-string" }),
  name: Decoration.mark({ class: "cm-st-name" }),
  raw: Decoration.mark({ class: "cm-st-raw" }),
};

const tokens = new MatchDecorator({
  regexp: TOKEN,
  decoration: (match) =>
    match[1]
      ? marks.comment
      : match[2]
        ? marks.label
        : match[3]
          ? marks.string
          : match[4]
            ? marks.name
            : marks.raw,
});

const highlight = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    constructor(view: EditorView) {
      this.decorations = tokens.createDeco(view);
    }
    update(update: ViewUpdate) {
      this.decorations = tokens.updateDeco(update, this.decorations);
    }
  },
  { decorations: (plugin) => plugin.decorations }
);

// ── The lines the assembler refuses ─────────────────────────────────

const setErrorLines = StateEffect.define<number[]>();
const errorLine = Decoration.line({ class: "cm-st-error-line" });

const errorLines = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(value, tr) {
    let next = value.map(tr.changes);
    for (const effect of tr.effects) {
      if (!effect.is(setErrorLines)) continue;
      const builder = new RangeSetBuilder<Decoration>();
      const lines = [...new Set(effect.value)].sort((a, b) => a - b);
      for (const number of lines) {
        if (number < 1 || number > tr.state.doc.lines) continue;
        const line = tr.state.doc.line(number);
        builder.add(line.from, line.from, errorLine);
      }
      next = builder.finish();
    }
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});

const scriptTheme = EditorView.theme({
  ".cm-st-comment": { color: "var(--color-muted-foreground)" },
  ".cm-st-label": { color: "var(--color-blue-accent-foreground)" },
  ".cm-st-string": { color: "hsl(140 45% 60%)" },
  ".cm-st-name": { color: "hsl(35 80% 65%)" },
  ".cm-st-raw": { color: "hsl(0 70% 70%)" },
  ".cm-st-error-line": { backgroundColor: "hsl(0 70% 50% / 0.15)" },
});

/**
 * A function's script as assembler text, assembled again a moment after typing stops: the
 * assembler's errors are marked by line and listed below, and once the text assembles, what a save
 * would change is shown instead.
 */
export function ScriptTextEditor({
  gamePath,
  container,
  entry,
  exportIndex,
  initial,
  keepsLayout,
  onText,
  onDiscard,
  onDone,
  newFunction,
}: {
  gamePath: string;
  container: string;
  entry: string;
  exportIndex: number;
  /** A function being added rather than one rewritten: the text is read against the function the
   *  save makes first, and done adds it. */
  newFunction?: { name: string; signature: string; class?: number; blocked: string | null };
  /** The text to start from: the draft's, or the script's own. */
  initial: string;
  /** Why the function has to keep its layout, when it has to. */
  keepsLayout: string | null;
  /** Called with the text a moment after typing stops. */
  onText: (text: string) => void;
  onDiscard: () => void;
  onDone: () => void;
}) {
  const parent = useRef<HTMLDivElement | null>(null);
  // Held as plain values, so a parent drawing the same function again does not ask again.
  const adding = newFunction !== undefined;
  const addingName = newFunction?.name;
  const addingSignature = newFunction?.signature;
  const addingClass = newFunction?.class;
  const view = useRef<EditorView | null>(null);
  const [text, setText] = useState(initial);
  const [answer, setAnswer] = useState<{
    asked: string;
    result: TextPreview | string;
  } | null>(null);
  const onTextRef = useRef(onText);
  useEffect(() => {
    onTextRef.current = onText;
  }, [onText]);

  useEffect(() => {
    if (!parent.current) return;
    const editor = new EditorView({
      doc: initial,
      parent: parent.current,
      extensions: [
        editorTheme({ gutters: true }),
        scriptTheme,
        lineNumbers(),
        editingKeys(),
        highlight,
        errorLines,
        EditorView.lineWrapping,
        EditorView.updateListener.of((update) => {
          if (update.docChanged) setText(update.state.doc.toString());
        }),
      ],
    });
    view.current = editor;
    return () => {
      editor.destroy();
      view.current = null;
    };
    // The editor owns the document from here on; `initial` only seeds it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const timer = setTimeout(() => onTextRef.current(text), DRAFT_DELAY_MS);
    return () => clearTimeout(timer);
  }, [text]);

  useEffect(() => {
    let cancelled = false;
    const timer = setTimeout(() => {
      const asked = adding
        ? invoke<TextPreview>("new_function_preview", {
            gameRoot: gamePath,
            container,
            entry,
            class: addingClass ?? null,
            name: addingName,
            signature: addingSignature,
            text,
          })
        : invoke<TextPreview>("assemble_preview", {
            gameRoot: gamePath,
            container,
            entry,
            export: exportIndex,
            text,
          });
      asked
        .then((result) => {
          if (!cancelled) setAnswer({ asked: text, result });
        })
        .catch((e: unknown) => {
          if (!cancelled) setAnswer({ asked: text, result: String(e) });
        });
    }, PREVIEW_DELAY_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [
    gamePath,
    container,
    entry,
    exportIndex,
    text,
    adding,
    addingName,
    addingSignature,
    addingClass,
  ]);

  const result = answer?.asked === text ? answer.result : null;
  const preview = typeof result === "object" ? result : null;

  useEffect(() => {
    view.current?.dispatch({
      effects: setErrorLines.of(preview ? preview.diagnostics.map((d) => d.line) : []),
    });
  }, [preview]);

  const goTo = (line: number, column: number) => {
    const editor = view.current;
    if (!editor) return;
    const at = editor.state.doc.line(Math.min(Math.max(line, 1), editor.state.doc.lines));
    editor.dispatch({
      selection: { anchor: Math.min(at.from + Math.max(column - 1, 0), at.to) },
      scrollIntoView: true,
    });
    editor.focus();
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-2 border-b border-border/60 px-3 py-1.5 text-[11px] text-muted-foreground">
        {newFunction ? (
          <span className="font-mono">
            New function {newFunction.name}
            {newFunction.signature}
          </span>
        ) : (
          <span>Editing as text</span>
        )}
        {keepsLayout && (
          <span className="flex items-center gap-1 text-amber-400">
            <AlertTriangle size={10} /> keeps its layout: its statements can change, not move
          </span>
        )}
        {newFunction ? (
          <span className="ml-auto flex gap-1.5">
            <Button
              size="sm"
              variant="ghost"
              className="h-6 px-2 text-[11px]"
              disabled={
                newFunction.blocked !== null ||
                !preview ||
                preview.diagnostics.length > 0 ||
                preview.refused !== undefined
              }
              title={newFunction.blocked ?? undefined}
              onClick={onDone}
            >
              <Plus size={12} /> Add function
            </Button>
            <Button size="sm" variant="ghost" className="h-6 px-2 text-[11px]" onClick={onDiscard}>
              <Trash2 size={12} /> Cancel
            </Button>
          </span>
        ) : (
          <span className="ml-auto flex gap-1.5">
            <Button size="sm" variant="ghost" className="h-6 px-2 text-[11px]" onClick={onDone}>
              <CornerUpLeft size={12} /> Listing
            </Button>
            <Button size="sm" variant="ghost" className="h-6 px-2 text-[11px]" onClick={onDiscard}>
              <Trash2 size={12} /> Discard text
            </Button>
          </span>
        )}
      </div>
      <div ref={parent} className="min-h-0 flex-1 overflow-hidden" data-testid="script-text" />
      <div className="max-h-48 shrink-0 overflow-auto border-t border-border/60 px-3 py-1.5 text-[11px]">
        {result === null ? (
          <p className="text-muted-foreground">Assembling…</p>
        ) : typeof result === "string" ? (
          <p className="text-red-400">{result}</p>
        ) : result.diagnostics.length > 0 ? (
          <ul className="space-y-0.5">
            {result.diagnostics.map((d, i) => (
              <li key={`${d.line}:${d.column}:${i}`}>
                <button
                  className="text-left text-red-400 hover:underline"
                  onClick={() => goTo(d.line, d.column)}
                >
                  line {d.line}:{d.column}: {d.message}
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <div className="space-y-1">
            {result.refused ? (
              <p className="flex items-start gap-1 text-red-400">
                <AlertTriangle size={11} className="mt-0.5 shrink-0" /> {result.refused}
              </p>
            ) : (
              <p className="flex items-center gap-1 text-green-400">
                <CheckCircle2 size={11} />{" "}
                {newFunction
                  ? "Assembles. Add function to write it to the mod."
                  : "Assembles. Save as mod to write it."}
              </p>
            )}
            {result.unchecked && (
              <p className="whitespace-pre-wrap text-amber-400">{result.unchecked}</p>
            )}
            {result.warnings.map((d, i) => (
              <button
                key={`warning:${i}`}
                className="block text-left text-amber-400 hover:underline"
                onClick={() => goTo(d.line, d.column)}
              >
                line {d.line}:{d.column}: {d.message}
              </button>
            ))}
            {result.applied.map((change, i) => (
              <p key={`${change.name}:${i}`} className="text-muted-foreground">
                {change.name}: {change.before} → {change.after}
              </p>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
