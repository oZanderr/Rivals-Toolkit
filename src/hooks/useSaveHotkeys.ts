import { useEffect, useRef } from "react";

interface Options {
  dirty: boolean;
  saving?: boolean;
  onSave: () => void | Promise<void>;
  onDiscard: () => void;
}

export function useSaveHotkeys({ dirty, saving = false, onSave, onDiscard }: Options) {
  const dirtyRef = useRef(dirty);
  const savingRef = useRef(saving);
  const onSaveRef = useRef(onSave);
  const onDiscardRef = useRef(onDiscard);

  useEffect(() => {
    dirtyRef.current = dirty;
    savingRef.current = saving;
    onSaveRef.current = onSave;
    onDiscardRef.current = onDiscard;
  });

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const target = e.target as HTMLElement | null;
      const isEditable =
        !!target &&
        (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        if (!dirtyRef.current || savingRef.current) return;
        e.preventDefault();
        void onSaveRef.current();
      } else if (e.key === "Escape" && !isEditable) {
        if (!dirtyRef.current || savingRef.current) return;
        e.preventDefault();
        onDiscardRef.current();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}
