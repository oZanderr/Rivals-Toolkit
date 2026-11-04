import { Folder, FolderInput, FolderPlus, FolderRoot } from "lucide-react";

import {
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
} from "@/components/ui/context-menu";
import { leafName } from "@/lib/modTree";

interface Props {
  /** Every folder below `~mods`, in the order the tree shows them. */
  folders: readonly string[];
  /** The folder every mod being moved is in, which is no move, or null when they differ. */
  current: string | null;
  disabled: boolean;
  onMove: (to: string) => void;
  onNewFolder: () => void;
}

/** A context menu's "Move to" submenu: `~mods` itself, each folder, or a new one. */
export function MoveToMenu({ folders, current, disabled, onMove, onNewFolder }: Props) {
  return (
    <ContextMenuSub>
      <ContextMenuSubTrigger disabled={disabled}>
        <FolderInput />
        Move to
      </ContextMenuSubTrigger>
      <ContextMenuSubContent className="max-h-72 max-w-72 overflow-y-auto">
        <ContextMenuItem disabled={current === ""} onSelect={() => onMove("")}>
          <FolderRoot />
          ~mods (top level)
        </ContextMenuItem>
        {folders.map((path) => (
          <ContextMenuItem
            key={path}
            disabled={current === path}
            onSelect={() => onMove(path)}
            style={{ paddingLeft: 8 + (path.split("/").length - 1) * 12 }}
          >
            <Folder />
            <span className="truncate">{leafName(path)}</span>
          </ContextMenuItem>
        ))}
        <ContextMenuSeparator />
        <ContextMenuItem onSelect={onNewFolder}>
          <FolderPlus />
          New folder…
        </ContextMenuItem>
      </ContextMenuSubContent>
    </ContextMenuSub>
  );
}
