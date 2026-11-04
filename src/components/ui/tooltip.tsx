import * as React from "react";

import { Tooltip as TooltipPrimitive } from "radix-ui";

import { cn } from "@/lib/utils";

const TooltipProvider = TooltipPrimitive.Provider;
/** How long a pointer rests on something before its hint shows. */
const TIP_DELAY = 600;
const Tooltip = TooltipPrimitive.Root;
const TooltipTrigger = TooltipPrimitive.Trigger;

const TooltipContent = React.forwardRef<
  React.ComponentRef<typeof TooltipPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof TooltipPrimitive.Content>
>(({ className, sideOffset = 4, ...props }, ref) => (
  <TooltipPrimitive.Portal>
    <TooltipPrimitive.Content
      ref={ref}
      sideOffset={sideOffset}
      className={cn(
        "pointer-events-none z-50 max-w-xs rounded-md border border-border bg-popover px-2 py-1 text-[11px] text-popover-foreground shadow-md break-all",
        "data-[state=delayed-open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=delayed-open]:fade-in-0",
        className
      )}
      {...props}
    />
  </TooltipPrimitive.Portal>
));
TooltipContent.displayName = TooltipPrimitive.Content.displayName;

interface TipProps {
  content: React.ReactNode;
  children: React.ReactNode;
  side?: React.ComponentProps<typeof TooltipPrimitive.Content>["side"];
  align?: React.ComponentProps<typeof TooltipPrimitive.Content>["align"];
  delayDuration?: number;
  disabled?: boolean;
  asChild?: boolean;
}

function Tip({
  content,
  children,
  side,
  align,
  delayDuration,
  disabled,
  asChild = true,
}: TipProps) {
  if (disabled || content === null || content === undefined || content === false) {
    return <>{children}</>;
  }
  // The app's one provider holds the delay, so a long list does not mount a provider per row.
  return (
    <Tooltip disableHoverableContent delayDuration={delayDuration}>
      <TooltipTrigger asChild={asChild}>{children}</TooltipTrigger>
      <TooltipContent side={side} align={align}>
        {content}
      </TooltipContent>
    </Tooltip>
  );
}

export { Tooltip, TooltipTrigger, TooltipContent, TooltipProvider, Tip, TIP_DELAY };
