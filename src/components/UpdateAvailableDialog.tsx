import { useEffect, useRef, useState } from "react";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ArrowUpCircle, Download, ExternalLink, RefreshCw, XCircle } from "lucide-react";
import { Dialog as DialogPrimitive } from "radix-ui";

import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import type { UpdateInfo } from "@/hooks/useUpdateCheck";

interface Props {
  updateInfo: UpdateInfo;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

type Phase = "idle" | "downloading" | "verifying" | "ready" | "error";

interface DownloadProgress {
  phase: string;
  downloaded: number;
  total: number;
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
  const trimmed = section.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function mb(bytes: number): string {
  return (bytes / (1024 * 1024)).toFixed(1);
}

export function UpdateAvailableDialog({ updateInfo, open, onOpenChange }: Props) {
  const changelog = extractChangelog(updateInfo.release_notes);
  const primaryRef = useRef<HTMLButtonElement>(null);
  const cancelledRef = useRef(false);

  const [phase, setPhase] = useState<Phase>("idle");
  const [progress, setProgress] = useState<DownloadProgress>({
    phase: "",
    downloaded: 0,
    total: 0,
  });
  const [error, setError] = useState<string | null>(null);

  // Reset transient download UI each time the dialog opens.
  useEffect(() => {
    if (!open) return;
    /* eslint-disable react-hooks/set-state-in-effect -- reset transient state on open */
    setPhase("idle");
    setError(null);
    setProgress({ phase: "", downloaded: 0, total: 0 });
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [open]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    listen<DownloadProgress>("update-download-progress", (event) => {
      setProgress(event.payload);
      const p = event.payload.phase;
      if (p === "downloading") setPhase("downloading");
      else if (p === "verifying" || p === "staging") setPhase("verifying");
      else if (p === "ready") setPhase("ready");
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const busy = phase === "downloading" || phase === "verifying";

  async function startDownload() {
    cancelledRef.current = false;
    setError(null);
    setProgress({ phase: "downloading", downloaded: 0, total: 0 });
    setPhase("downloading");
    try {
      await invoke("download_update", { version: updateInfo.latest_version });
      setPhase("ready");
    } catch (e: unknown) {
      const msg = String(e);
      if (cancelledRef.current || /cancel/i.test(msg)) {
        setPhase("idle");
      } else {
        setError(msg);
        setPhase("error");
      }
    }
  }

  function cancelDownload() {
    cancelledRef.current = true;
    invoke("cancel_update_download").catch(() => {});
  }

  async function restartNow() {
    try {
      // On success the backend swaps files and exits the process, so this never resolves.
      await invoke("apply_update_and_restart");
    } catch (e: unknown) {
      setError(String(e));
      setPhase("error");
    }
  }

  function openReleasePage() {
    openUrl(updateInfo.release_url).catch(console.error);
  }

  const pct =
    progress.total > 0
      ? Math.min(100, Math.round((progress.downloaded / progress.total) * 100))
      : 0;
  const progressLabel =
    phase === "verifying"
      ? "Verifying…"
      : progress.total > 0
        ? `Downloading… ${pct}%`
        : `Downloading… ${mb(progress.downloaded)} MB`;

  return (
    <DialogPrimitive.Root
      open={open}
      onOpenChange={(next) => {
        // Don't let the dialog close out from under an in-flight download.
        if (!next && busy) return;
        onOpenChange(next);
      }}
    >
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/80 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0" />
        <DialogPrimitive.Content
          className="fixed left-[50%] top-[50%] z-50 grid w-full max-w-xl translate-x-[-50%] translate-y-[-50%] gap-4 rounded-lg border bg-background p-6 shadow-lg duration-200 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95"
          onOpenAutoFocus={(e) => {
            e.preventDefault();
            primaryRef.current?.focus();
          }}
          onEscapeKeyDown={(e) => {
            if (busy) e.preventDefault();
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

          {busy && (
            <div className="flex flex-col gap-1.5">
              <Progress value={phase === "verifying" ? 100 : pct} />
              <span className="text-[12px] text-muted-foreground">{progressLabel}</span>
            </div>
          )}

          {phase === "error" && error && (
            <div className="flex items-start gap-2 rounded-md border border-destructive/30 bg-destructive/10 p-3 text-[12px] text-destructive">
              <XCircle size={14} className="mt-0.5 shrink-0" />
              <span className="break-words">{error}</span>
            </div>
          )}

          <div className="flex flex-col-reverse sm:flex-row sm:justify-end sm:gap-2">
            {phase === "idle" && (
              <>
                <DialogPrimitive.Close asChild>
                  <Button variant="outline">Dismiss</Button>
                </DialogPrimitive.Close>
                <Button variant="outline" onClick={openReleasePage}>
                  <ExternalLink size={14} />
                  Open in browser
                </Button>
                <Button
                  ref={primaryRef}
                  variant="blue"
                  onClick={startDownload}
                  className="focus-visible:ring-0 focus-visible:border-blue-accent-border"
                >
                  <Download size={14} />
                  Download &amp; install
                </Button>
              </>
            )}

            {busy && (
              <Button variant="outline" onClick={cancelDownload} disabled={phase === "verifying"}>
                Cancel
              </Button>
            )}

            {phase === "ready" && (
              <>
                <DialogPrimitive.Close asChild>
                  <Button variant="outline">Later</Button>
                </DialogPrimitive.Close>
                <Button
                  ref={primaryRef}
                  variant="blue"
                  onClick={restartNow}
                  className="focus-visible:ring-0 focus-visible:border-blue-accent-border"
                >
                  <RefreshCw size={14} />
                  Restart now
                </Button>
              </>
            )}

            {phase === "error" && (
              <>
                <DialogPrimitive.Close asChild>
                  <Button variant="outline">Dismiss</Button>
                </DialogPrimitive.Close>
                <Button variant="blue" onClick={openReleasePage}>
                  <ExternalLink size={14} />
                  Open release page
                </Button>
              </>
            )}
          </div>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
