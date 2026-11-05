import { useEffect, useState } from "react";

import { Check, Copy, Terminal } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Tip } from "@/components/ui/tooltip";

/** The Steam launch option Proton needs before it loads the proxy DLLs beside the game. */
export function WineLaunchOption({ option }: { option: string }) {
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 2000);
    return () => clearTimeout(timer);
  }, [copied]);

  return (
    <div className="flex items-center gap-2.5 rounded-md border border-border bg-card px-3 py-2">
      <Terminal size={15} className="shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1 text-[12px] text-muted-foreground">
        Proton loads its own copy of these DLLs unless told otherwise. Set this as Marvel
        Rivals&apos; launch options in Steam:{" "}
        <code className="break-all font-mono text-foreground">{option}</code>
      </span>
      <Tip content={copied ? "Copied" : "Copy the launch option"}>
        <Button
          variant="ghost"
          size="icon-sm"
          onClick={() =>
            navigator.clipboard.writeText(option).then(
              () => setCopied(true),
              (e: unknown) => console.error(e)
            )
          }
        >
          {copied ? <Check size={14} /> : <Copy size={14} />}
        </Button>
      </Tip>
    </div>
  );
}
