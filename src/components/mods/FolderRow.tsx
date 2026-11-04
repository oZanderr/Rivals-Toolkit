import {
  ChevronDown,
  Folder,
  FolderOpen,
  FolderPlus,
  Pencil,
  Power,
  PowerOff,
  Trash2,
} from "lucide-react";

import { RenameInput } from "./RenameInput";

import { Checkbox } from "@/components/ui/checkbox";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { Switch } from "@/components/ui/switch";
import { Tip } from "@/components/ui/tooltip";
import { indent, type FolderNode, type TreeMod } from "@/lib/modTree";
import { cn } from "@/lib/utils";

export interface FolderActions {
  toggleExpanded: () => void;
  select: (on: boolean) => void;
  switchAll: (enabled: boolean) => void;
  newSubfolder: () => void;
  startRename: () => void;
  rename: (name: string) => void;
  cancelRename: () => void;
  reveal: () => void;
  remove: () => void;
}

interface Props<M extends TreeMod> {
  folder: FolderNode<M>;
  depth: number;
  expanded: boolean;
  /** Off while a filter shows every folder open. */
  collapsible: boolean;
  /** The mods its count, checkbox and switch act on. */
  mods: M[];
  selected: boolean | "indeterminate";
  busy: boolean;
  gameRunning: boolean;
  renaming: boolean;
  /** Where a drop being dragged over the list would land, as far as this folder goes. */
  drop: "target" | "inside" | null;
  actions: FolderActions;
}

/** A folder's row in the mod list, laid out like a mod's: its switch and checkbox cover every mod
 *  inside it. */
export function FolderRow<M extends TreeMod>({
  folder,
  depth,
  expanded,
  collapsible,
  mods,
  selected,
  busy,
  gameRunning,
  renaming,
  drop,
  actions,
}: Props<M>) {
  const enabled = mods.filter((mod) => mod.enabled).length;
  const locked = busy || gameRunning;
  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <li
          data-drop-folder={folder.path}
          onClick={() => collapsible && actions.toggleExpanded()}
          className={cn(
            "relative flex h-10 cursor-default items-center gap-3 border-b border-border/50 bg-secondary/15 px-3 select-none hover:bg-secondary/40",
            drop === "inside" && "bg-ok/5",
            drop === "target" && "bg-ok/15 ring-1 ring-ok ring-inset hover:bg-ok/15"
          )}
        >
          <Checkbox
            checked={selected}
            disabled={mods.length === 0}
            onClick={(e) => e.stopPropagation()}
            onCheckedChange={(on) => actions.select(on === true)}
            aria-label={`Select every mod in ${folder.path}`}
          />
          <span
            className="flex min-w-0 flex-1 items-center gap-2 text-[13px]"
            style={depth > 0 ? { paddingLeft: indent(depth) } : undefined}
          >
            <button
              type="button"
              disabled={!collapsible}
              aria-expanded={expanded}
              aria-label={`${expanded ? "Collapse" : "Expand"} ${folder.path}`}
              onClick={(e) => {
                e.stopPropagation();
                actions.toggleExpanded();
              }}
              className="shrink-0 text-muted-foreground hover:text-foreground disabled:opacity-40"
            >
              <ChevronDown
                size={14}
                className={cn("transition-transform", !expanded && "-rotate-90")}
              />
            </button>
            {expanded ? (
              <FolderOpen size={15} className="shrink-0 text-muted-foreground" />
            ) : (
              <Folder size={15} className="shrink-0 text-muted-foreground" />
            )}
            {renaming ? (
              <RenameInput
                displayName={folder.name}
                onCommit={actions.rename}
                onCancel={actions.cancelRename}
              />
            ) : (
              <Tip content={folder.path} side="top" align="start">
                <span className="min-w-0 truncate font-medium">{folder.name}</span>
              </Tip>
            )}
            <span className="ml-auto shrink-0 text-[11px] tabular-nums text-muted-foreground">
              {mods.length === 0 ? "empty" : `${enabled}/${mods.length} on`}
            </span>
          </span>
          <Switch
            checked={enabled > 0}
            disabled={locked || mods.length === 0}
            onClick={(e) => e.stopPropagation()}
            onCheckedChange={actions.switchAll}
            aria-label={`Switch every mod in ${folder.path} ${enabled > 0 ? "off" : "on"}`}
            className="shrink-0"
          />
          {/* The width of a mod row's delete button, so the switches line up. */}
          <span className="w-5.25 shrink-0" />
        </li>
      </ContextMenuTrigger>
      <ContextMenuContent>
        <ContextMenuItem
          disabled={locked || enabled === mods.length}
          onSelect={() => actions.switchAll(true)}
        >
          <Power />
          Enable all
        </ContextMenuItem>
        <ContextMenuItem
          disabled={locked || enabled === 0}
          onSelect={() => actions.switchAll(false)}
        >
          <PowerOff />
          Disable all
        </ContextMenuItem>
        <ContextMenuSeparator />
        <ContextMenuItem onSelect={actions.newSubfolder}>
          <FolderPlus />
          New subfolder…
        </ContextMenuItem>
        <ContextMenuItem disabled={locked} onSelect={actions.startRename}>
          <Pencil />
          Rename
        </ContextMenuItem>
        <ContextMenuItem onSelect={actions.reveal}>
          <FolderOpen />
          Show in folder
        </ContextMenuItem>
        <ContextMenuSeparator />
        <ContextMenuItem destructive disabled={locked} onSelect={actions.remove}>
          <Trash2 />
          Delete folder
        </ContextMenuItem>
      </ContextMenuContent>
    </ContextMenu>
  );
}
