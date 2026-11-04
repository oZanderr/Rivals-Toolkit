import { GripVertical, RotateCcw } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Tip } from "@/components/ui/tooltip";
import { useSortableList } from "@/hooks/useSortableList";
import {
  ALWAYS_SHOWN,
  DEFAULT_TAB_LAYOUT,
  isDefaultTabLayout,
  reorderTabs,
  setTabHidden,
  tabInfo,
  type TabLayout,
} from "@/lib/tabs";
import { cn } from "@/lib/utils";

interface Props {
  /** `null` until the stored layout has loaded. */
  layout: TabLayout | null;
  /** Applies and saves a change at once, outside the settings' Save. */
  onChange: (layout: TabLayout) => void;
}

/** Which sidebar tabs show, and in what order. */
export function TabsSettings({ layout, onChange }: Props) {
  const order = layout?.order ?? [];
  const { item, handle, draggingIndex } = useSortableList(order.length, (from, to) => {
    if (layout) onChange(reorderTabs(layout, order, from, to));
  });

  return (
    <div className="flex flex-col overflow-hidden rounded-md border border-border">
      <div className="flex items-center gap-2 border-b border-border bg-card px-3 py-2">
        <h3 className="min-w-0 text-sm font-semibold">Tabs</h3>
        <Button
          variant="outline"
          size="sm"
          className="ml-auto shrink-0"
          disabled={!layout || isDefaultTabLayout(layout)}
          onClick={() => onChange(DEFAULT_TAB_LAYOUT)}
        >
          <RotateCcw size={13} />
          Reset to default
        </Button>
      </div>
      <p className="px-3 pt-2 text-[11px] text-muted-foreground">
        Drag to reorder, or right-click a tab in the sidebar. The first shown tab opens at startup.
      </p>
      <ul className="flex flex-col py-1">
        {order.map((id, at) => {
          const { label, icon: Icon } = tabInfo(id);
          const hidden = layout?.hidden.includes(id) ?? false;
          const row = item(at);
          return (
            <li
              key={id}
              ref={row.ref}
              style={row.style}
              className={cn(
                "flex items-center gap-3 bg-background px-3 py-2 hover:bg-secondary/50",
                draggingIndex === at && "bg-secondary shadow-md"
              )}
            >
              <Tip content="Drag to reorder (Alt+Up/Down)">
                <button
                  {...handle(at)}
                  aria-label={`Reorder ${label}`}
                  className={cn(
                    "touch-none text-muted-foreground hover:text-foreground",
                    draggingIndex === at ? "cursor-grabbing" : "cursor-grab"
                  )}
                >
                  <GripVertical size={14} />
                </button>
              </Tip>
              <label className="flex min-w-0 flex-1 items-center gap-3">
                <Icon size={15} className={cn(hidden && "text-muted-foreground")} />
                <span className={cn("text-[13px] font-medium", hidden && "text-muted-foreground")}>
                  {label}
                </span>
                {id === ALWAYS_SHOWN && (
                  <span className="text-[11px] text-muted-foreground">Always shown</span>
                )}
                <Switch
                  className="ml-auto"
                  checked={!hidden}
                  disabled={!layout || id === ALWAYS_SHOWN}
                  aria-label={`Show ${label}`}
                  onCheckedChange={(on) => {
                    if (layout) onChange(setTabHidden(layout, id, !on));
                  }}
                />
              </label>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
