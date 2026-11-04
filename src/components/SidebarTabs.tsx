import { ArrowDown, ArrowUp, EyeOff, Settings2 } from "lucide-react";

import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { useSortableList } from "@/hooks/useSortableList";
import {
  ALWAYS_SHOWN,
  reorderTabs,
  setTabHidden,
  tabInfo,
  visibleTabs,
  type Tab,
  type TabLayout,
} from "@/lib/tabs";
import { cn } from "@/lib/utils";

interface Props {
  /** `null` until the stored layout has loaded, which lists no tabs. */
  layout: TabLayout | null;
  activeTab: Tab | null;
  onSelect: (tab: Tab) => void;
  onLayoutChange: (layout: TabLayout) => void;
  /** Opens the tab settings, where hidden tabs come back. */
  onCustomize: () => void;
}

/** The sidebar's tabs in the user's order: dragged to reorder, right-clicked to move or hide. */
export function SidebarTabs({ layout, activeTab, onSelect, onLayoutChange, onCustomize }: Props) {
  const shown = layout ? visibleTabs(layout) : [];
  const move = (from: number, to: number) => {
    if (layout) onLayoutChange(reorderTabs(layout, shown, from, to));
  };
  const { item, handle, draggingIndex } = useSortableList(shown.length, move);

  return (
    <ul className="flex flex-1 flex-col gap-0.5 px-2 pt-2">
      {shown.map((id, at) => {
        const { label, icon: Icon } = tabInfo(id);
        const row = item(at);
        return (
          <li key={id} ref={row.ref} style={row.style}>
            <ContextMenu>
              <ContextMenuTrigger asChild>
                <button
                  {...handle(at)}
                  onClick={() => onSelect(id)}
                  aria-current={activeTab === id ? "page" : undefined}
                  className={cn(
                    "flex w-full touch-none items-center gap-2.5 rounded-sm px-2.5 py-2 text-sm text-muted-foreground transition-colors hover:bg-secondary hover:text-foreground",
                    activeTab === id && "bg-secondary font-semibold text-foreground",
                    draggingIndex === at && "cursor-grabbing bg-secondary shadow-md"
                  )}
                >
                  <Icon size={15} />
                  {label}
                </button>
              </ContextMenuTrigger>
              <ContextMenuContent>
                <ContextMenuItem disabled={at === 0} onSelect={() => move(at, at - 1)}>
                  <ArrowUp />
                  Move up
                </ContextMenuItem>
                <ContextMenuItem
                  disabled={at === shown.length - 1}
                  onSelect={() => move(at, at + 1)}
                >
                  <ArrowDown />
                  Move down
                </ContextMenuItem>
                {id !== ALWAYS_SHOWN && layout && (
                  <ContextMenuItem onSelect={() => onLayoutChange(setTabHidden(layout, id, true))}>
                    <EyeOff />
                    Hide tab
                  </ContextMenuItem>
                )}
                <ContextMenuSeparator />
                <ContextMenuItem onSelect={onCustomize}>
                  <Settings2 />
                  Customize tabs…
                </ContextMenuItem>
              </ContextMenuContent>
            </ContextMenu>
          </li>
        );
      })}
    </ul>
  );
}
