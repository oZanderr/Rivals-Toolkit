import { useEffect, useState } from "react";

import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { AlertTriangle, Download, ImageUp, Loader2, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tip } from "@/components/ui/tooltip";
import {
  draftKey,
  textureTarget,
  useEditSession,
  type TextureQuality,
} from "@/hooks/useAssetEdits";

interface TextureMip {
  size_x: number;
  size_y: number;
  size_z: number;
  placement: string;
  size: number;
}

interface TexturePlatform {
  format: string;
  size_x: number;
  size_y: number;
  slices: number;
  cubemap: boolean;
  first_mip: number;
  mips: TextureMip[];
}

export interface TextureInfo {
  export: number;
  name: string;
  layout: { class: string; platforms: TexturePlatform[] };
  absent_mips?: number[];
  undecodable?: string;
  hdr?: boolean;
}

interface SavedImage {
  path: string;
  mip: number;
  mips: number;
  width: number;
  height: number;
}

/** The image files a texture goes out as, and comes in from, by the range of values it holds. */
const OUT_KINDS = { hdr: ["exr", "dds"], low: ["png", "tga", "bmp", "dds"] };
const IN_KINDS = { hdr: ["exr", "dds"], low: ["png", "tga", "bmp", "jpg", "jpeg", "dds"] };
const KIND_NAMES: Record<string, string> = {
  png: "PNG",
  tga: "TGA",
  bmp: "BMP",
  dds: "DDS",
  exr: "OpenEXR",
  jpg: "JPEG",
  jpeg: "JPEG",
};
/** The formats whose encoders take long enough for how hard they work to matter. */
const SLOW_FORMATS = new Set(["PF_BC7", "PF_BC6H"]);

const CHECKERBOARD = {
  backgroundImage: "repeating-conic-gradient(#8884 0% 25%, transparent 0% 50%)",
  backgroundSize: "16px 16px",
};

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function mipsText(saved: SavedImage): string {
  return saved.mips > 1 ? `mips ${saved.mip} to ${saved.mip + saved.mips - 1}` : `mip ${saved.mip}`;
}

/** A texture's format, mips and preview, with its pixels saved to an image file or replaced from
 *  one, the replacement queued as a draft until the save. Keyed by its export and the read it
 *  shows, so each starts afresh. */
export function TexturePane({
  gamePath,
  container,
  entry,
  exportIndex,
  onReport,
}: {
  gamePath: string;
  container: string;
  entry: string;
  exportIndex: number;
  onReport: (message: string, type: "ok" | "err") => void;
}) {
  const session = useEditSession();
  const [info, setInfo] = useState<TextureInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [mip, setMip] = useState("largest");
  const [quality, setQuality] = useState<TextureQuality>("default");

  useEffect(() => {
    let cancelled = false;
    let url: string | null = null;
    const args = { gameRoot: gamePath, container, entry, export: exportIndex };
    invoke<TextureInfo>("texture_info", args).then(
      (read) => !cancelled && setInfo(read),
      (e: unknown) => !cancelled && setError(String(e))
    );
    invoke<ArrayBuffer>("texture_preview", args).then(
      (png) => {
        if (cancelled || typeof URL.createObjectURL !== "function") return;
        url = URL.createObjectURL(new Blob([png], { type: "image/png" }));
        setPreview(url);
      },
      (e: unknown) => !cancelled && setPreviewError(String(e))
    );
    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [gamePath, container, entry, exportIndex]);

  if (error) {
    return (
      <div className="flex flex-1 items-center justify-center gap-2 p-6 text-sm text-muted-foreground">
        <AlertTriangle size={15} className="text-amber-400" /> {error}
      </div>
    );
  }
  if (!info) {
    return (
      <div className="flex flex-1 items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 size={15} className="animate-spin" /> Reading texture
      </div>
    );
  }

  const platform = info.layout.platforms[0];
  const absent = new Set(info.absent_mips ?? []);
  const outKinds = info.hdr ? OUT_KINDS.hdr : OUT_KINDS.low;
  const inKinds = info.hdr ? IN_KINDS.hdr : IN_KINDS.low;
  const key = draftKey(textureTarget(exportIndex));
  const drafted = session.drafts[key]?.draft;
  const replacing = drafted?.op === "texture_image" ? drafted.file : null;
  const replaceLock = info.undecodable ?? session.locked;

  const exportImage = async () => {
    const path = await saveDialog({
      defaultPath: `${info.name}.${outKinds[0]}`,
      filters: outKinds.map((kind) => ({ name: KIND_NAMES[kind], extensions: [kind] })),
    });
    if (!path) return;
    try {
      const saved = await invoke<SavedImage>("export_texture", {
        gameRoot: gamePath,
        container,
        entry,
        export: exportIndex,
        mip: mip === "largest" ? null : Number(mip),
        path,
      });
      onReport(`Saved ${mipsText(saved)}, ${saved.width}x${saved.height}, to ${saved.path}`, "ok");
    } catch (e: unknown) {
      onReport(String(e), "err");
    }
  };

  const replace = async () => {
    const picked = await openDialog({
      multiple: false,
      directory: false,
      filters: [{ name: "Images", extensions: inKinds }],
    });
    const file = Array.isArray(picked) ? picked[0] : picked;
    if (!file) return;
    session.setDraft(textureTarget(exportIndex), { op: "texture_image", file, quality }, []);
  };

  const chooseQuality = (next: string) => {
    const chosen = next as TextureQuality;
    setQuality(chosen);
    if (replacing)
      session.setDraft(
        textureTarget(exportIndex),
        { op: "texture_image", file: replacing, quality: chosen },
        []
      );
  };

  return (
    <div className="min-h-0 min-w-0 flex-1 overflow-auto p-4">
      <div className="flex flex-wrap items-start gap-6">
        <div className="flex flex-col gap-2">
          {preview ? (
            <img
              src={preview}
              alt={`${info.name} preview`}
              className="max-h-[512px] max-w-[512px] rounded border border-border object-contain"
              style={CHECKERBOARD}
            />
          ) : (
            <div className="flex h-48 w-48 items-center justify-center rounded border border-border p-3 text-center text-[11px] text-muted-foreground">
              {previewError ?? <Loader2 size={15} className="animate-spin" />}
            </div>
          )}
        </div>

        <div className="flex min-w-[320px] flex-1 flex-col gap-3 text-[11px]">
          {platform && (
            <div className="flex flex-col gap-1">
              <span className="font-mono text-xs text-foreground">
                {platform.format}, {platform.size_x}x{platform.size_y}
                {platform.cubemap ? ", a cube's 6 faces" : ""}
                {!platform.cubemap && platform.slices > 1
                  ? `, ${platform.slices} slices`
                  : ""}, {platform.mips.length} mip(s)
              </span>
              {platform.first_mip > 0 && (
                <span className="text-muted-foreground">
                  The cook left out its {platform.first_mip} largest mip(s), so its mips start
                  smaller than its size.
                </span>
              )}
              {info.hdr && (
                <span className="text-muted-foreground">
                  Holds high dynamic range values: it goes out and comes in as OpenEXR or DDS.
                </span>
              )}
              {info.undecodable && <span className="text-amber-400">{info.undecodable}</span>}
            </div>
          )}

          {platform && (
            <table className="w-fit border-collapse font-mono">
              <tbody>
                {platform.mips.map((level, index) => (
                  <tr key={index} className={absent.has(index) ? "text-muted-foreground/60" : ""}>
                    <td className="pr-3 text-muted-foreground">mip {index}</td>
                    <td className="pr-3 text-right">
                      {level.size_x}x{level.size_y}
                      {level.size_z > 1 ? `x${level.size_z}` : ""}
                    </td>
                    <td className="pr-3">{level.placement}</td>
                    <td className="pr-3 text-right">{formatSize(level.size)}</td>
                    <td>{absent.has(index) ? "not installed" : ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          <div className="flex flex-wrap items-center gap-2">
            <Select value={mip} onValueChange={setMip}>
              <Tip content="The mip Export saves, and for a DDS every one below it" side="bottom">
                <SelectTrigger size="sm" className="h-7 w-36 text-[11px]" aria-label="Mip">
                  <SelectValue />
                </SelectTrigger>
              </Tip>
              <SelectContent>
                <SelectItem value="largest" className="text-[11px]">
                  Largest it has
                </SelectItem>
                {platform?.mips.map((level, index) =>
                  absent.has(index) ? null : (
                    <SelectItem key={index} value={String(index)} className="text-[11px]">
                      Mip {index}, {level.size_x}x{level.size_y}
                    </SelectItem>
                  )
                )}
              </SelectContent>
            </Select>
            <Tip
              content={
                info.undecodable ??
                `Save the mip as ${outKinds.map((kind) => KIND_NAMES[kind]).join(", ")}`
              }
              side="bottom"
            >
              <span>
                <Button
                  size="sm"
                  variant="outline"
                  className="h-7"
                  disabled={Boolean(info.undecodable)}
                  onClick={() => void exportImage()}
                >
                  <Download size={13} /> Export…
                </Button>
              </span>
            </Tip>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            {platform && SLOW_FORMATS.has(platform.format) && (
              <Select value={quality} onValueChange={chooseQuality}>
                <Tip content={`How hard ${platform.format} works, slower for closer colors`}>
                  <SelectTrigger size="sm" className="h-7 w-28 text-[11px]" aria-label="Quality">
                    <SelectValue />
                  </SelectTrigger>
                </Tip>
                <SelectContent>
                  <SelectItem value="fast" className="text-[11px]">
                    Fast
                  </SelectItem>
                  <SelectItem value="default" className="text-[11px]">
                    Default
                  </SelectItem>
                  <SelectItem value="best" className="text-[11px]">
                    Best
                  </SelectItem>
                </SelectContent>
              </Select>
            )}
            <Tip
              content={
                replaceLock ??
                `Give it an image's pixels, encoded in ${platform?.format ?? "its format"} with its mips rebuilt, on save. A DDS in that format keeps its own mips.`
              }
              side="bottom"
            >
              <span>
                <Button
                  size="sm"
                  variant="outline"
                  className="h-7"
                  disabled={Boolean(replaceLock)}
                  onClick={() => void replace()}
                >
                  <ImageUp size={13} /> Replace…
                </Button>
              </span>
            </Tip>
          </div>
          {replacing && (
            <div className="flex items-center gap-2 rounded border border-sky-500/30 bg-sky-500/10 px-2 py-1">
              <span className="min-w-0 flex-1 truncate">
                Takes the pixels of <span className="font-mono">{replacing}</span> on save
              </span>
              <Tip content="Keep the texture's own pixels">
                <button
                  type="button"
                  aria-label="Discard the replacement"
                  className="text-muted-foreground hover:text-foreground"
                  onClick={() => session.dropDraft(key)}
                >
                  <X size={12} />
                </button>
              </Tip>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
