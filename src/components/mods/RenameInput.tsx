import { useEffect, useRef } from "react";

/** An inline editor for a mod's or folder's name. A mod's `_N_P` suffix stays fixed, so only the
 *  name before it is edited. */
export function RenameInput({
  displayName,
  onCommit,
  onCancel,
}: {
  displayName: string;
  onCommit: (value: string) => void;
  onCancel: () => void;
}) {
  const match = displayName.match(/^(.+?)(_\d+_P)?\.pak$/);
  const base = match ? match[1] : displayName;
  const suffix = match?.[2] ?? "";
  const ref = useRef<HTMLInputElement>(null);
  const committed = useRef(false);

  useEffect(() => {
    const el = ref.current;
    if (el) {
      el.focus();
      el.select();
    }
  }, []);

  const commit = (value: string) => {
    if (committed.current) return;
    committed.current = true;
    onCommit(value + suffix);
  };

  return (
    <span className="flex items-center gap-0 rounded border border-primary bg-background focus-within:ring-1 focus-within:ring-primary">
      <input
        ref={ref}
        defaultValue={base}
        className="min-w-0 flex-1 bg-transparent px-1.5 py-0.5 text-[13px] font-semibold outline-none"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter") {
            commit((e.target as HTMLInputElement).value);
          } else if (e.key === "Escape") {
            committed.current = true;
            onCancel();
          }
        }}
        onBlur={(e) => commit(e.target.value)}
      />
      {suffix && (
        <span className="shrink-0 pr-1.5 font-mono text-[13px] text-muted-foreground/40">
          {suffix}
        </span>
      )}
    </span>
  );
}
