export interface PakScanError {
  pak_name: string;
  pak_path: string;
  error: string;
}

// A pak that fails to open used to be skipped silently, which looked identical to a mod that was
// never there. This turns it into something the user can act on.
export function unreadableScanMessage(errors: PakScanError[]): string {
  const [first] = errors;
  if (!first) return "";
  return errors.length === 1
    ? `Could not read ${first.pak_name}: ${first.error}`
    : `Could not read ${errors.length} paks, including ${first.pak_name}: ${first.error}`;
}
