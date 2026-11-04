import type { ReactElement } from "react";

import { render as rtlRender, type RenderOptions } from "@testing-library/react";

import { TIP_DELAY, TooltipProvider } from "@/components/ui/tooltip";

export { act, cleanup, screen, waitFor, within } from "@testing-library/react";

/** Renders `ui` inside the tooltip provider the app wraps everything in. */
export function render(ui: ReactElement, options?: Omit<RenderOptions, "wrapper">) {
  return rtlRender(ui, {
    wrapper: ({ children }) => (
      <TooltipProvider delayDuration={TIP_DELAY} skipDelayDuration={0}>
        {children}
      </TooltipProvider>
    ),
    ...options,
  });
}
