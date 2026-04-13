import { useRef } from "react";

import { openUrl } from "@tauri-apps/plugin-opener";
import { ArrowUpCircle, Download } from "lucide-react";
import { Dialog as DialogPrimitive } from "radix-ui";

import { Button } from "@/components/ui/button";
import type { UpdateInfo } from "@/hooks/useUpdateCheck";

interface Props {
  updateInfo: UpdateInfo;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

function extractChangelog(raw: string | null): string | null {
  if (!raw) return null;
  const start = raw.match(/^##\s+Changelog\s*$/im);
  let section: string;
  if (start && start.index !== undefined) {
    const after = raw.slice(start.index + start[0].length);
    const next = after.match(/^##\s+/m);
    section = next && next.index !== undefined ? after.slice(0, next.index) : after;
  } else {
    section = raw;
  }
  const stripped = section.replace(/\s*\(\b[0-9a-f]{7,40}\b\)\s*$/gm, "").trim();
  return stripped.length > 0 ? stripped : null;
}

export function UpdateAvailableDialog({ updateInfo, open, onOpenChange }: Props) {
  const changelog = extractChangelog(updateInfo.release_notes);
  const downloadRef = useRef<HTMLButtonElement>(null);

  const handleDownload = () => {
    openUrl(updateInfo.release_url).catch(console.error);
    onOpenChange(false);
  };

  return (
    <DialogPrimitive.Root open={open} onOpenChange={onOpenChange}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/80 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0" />
        <DialogPrimitive.Content
          className="fixed left-[50%] top-[50%] z-50 grid w-full max-w-xl translate-x-[-50%] translate-y-[-50%] gap-4 rounded-lg border bg-background p-6 shadow-lg duration-200 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95"
          onOpenAutoFocus={(e) => {
            e.preventDefault();
            downloadRef.current?.focus();
          }}
        >
          <div className="flex flex-col gap-2">
            <DialogPrimitive.Title className="flex items-center gap-2 text-lg font-semibold">
              <ArrowUpCircle size={20} className="text-blue-accent-foreground" />
              Update Available
            </DialogPrimitive.Title>
            <DialogPrimitive.Description asChild>
              <div className="text-sm text-muted-foreground">
                A new version of Rivals Toolkit is available:{" "}
                <span className="font-mono font-semibold text-foreground">
                  v{updateInfo.current_version}
                </span>{" "}
                <span className="text-muted-foreground">→</span>{" "}
                <span className="font-mono font-semibold text-blue-accent-foreground">
                  v{updateInfo.latest_version}
                </span>
              </div>
            </DialogPrimitive.Description>
          </div>

          {changelog && (
            <div className="flex flex-col gap-1.5">
              <span className="text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                Changelog
              </span>
              <div className="max-h-64 overflow-y-auto rounded-md border border-border bg-background p-3">
                <pre className="whitespace-pre-wrap font-sans text-[12px] leading-relaxed text-foreground">
                  {changelog}
                </pre>
              </div>
            </div>
          )}

          <div className="flex flex-col-reverse sm:flex-row sm:justify-end sm:gap-2">
            <DialogPrimitive.Close asChild>
              <Button variant="outline">Dismiss</Button>
            </DialogPrimitive.Close>
            <Button
              ref={downloadRef}
              variant="blue"
              onClick={handleDownload}
              className="focus-visible:ring-0 focus-visible:border-blue-accent-border"
            >
              <Download size={14} />
              Download update
            </Button>
          </div>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
