import { useRef, useState } from "react";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Input } from "@/components/ui/input";
import { joinFolder } from "@/lib/modTree";

/** Why `name` cannot name a folder, as far as can be told here: the backend has the last word. */
function folderNameProblem(name: string): string | null {
  for (const part of name.trim().split("/")) {
    if (!part) return "A folder needs a name between each /";
    if (part === "." || part === "..") return `${part} names no folder`;
    if (/[<>:"\\|?*]/.test(part)) return "A folder's name cannot hold < > : \" \\ | ? or *";
    if (part !== part.trim() || part.endsWith(".")) {
      return "A folder's name cannot start or end with a space, or end with a dot";
    }
  }
  return null;
}

interface Props {
  /** Open while set: the folder the new one goes in, "" for `~mods`. */
  parent: string | null;
  /** Says what happens once it is made, such as moving mods into it. */
  then?: string;
  onConfirm: (path: string) => void;
  onClose: () => void;
}

/** Asks for the name of a new folder below `~mods`. */
export function FolderNameDialog({ parent, then, onConfirm, onClose }: Props) {
  const [name, setName] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const problem = name.trim() ? folderNameProblem(name) : null;
  const ready = name.trim() !== "" && problem === null;
  const submit = () => {
    if (!ready || parent === null) return;
    onConfirm(joinFolder(parent, name.trim()));
    onClose();
  };
  return (
    <AlertDialog open={parent !== null} onOpenChange={(open) => !open && onClose()}>
      <AlertDialogContent
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          input.current?.focus();
        }}
      >
        <AlertDialogHeader>
          <AlertDialogTitle>New folder</AlertDialogTitle>
          <AlertDialogDescription>
            In <span className="font-mono">{parent ? `~mods/${parent}` : "~mods"}</span>
            {then ? `, ${then}` : ""}. A / makes folders inside folders.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <Input
          ref={input}
          value={name}
          placeholder="Folder name"
          aria-label="Folder name"
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              submit();
            }
          }}
          className="h-8 font-mono text-xs"
        />
        {problem && <p className="text-[11px] text-err">{problem}</p>}
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction disabled={!ready} onClick={submit}>
            Make folder
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
