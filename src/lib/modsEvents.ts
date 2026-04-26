export type ModsEventSource = "Sounds";

export interface ModsChangedEvent {
  modsFolder: string;
  source: ModsEventSource;
}

const target = new EventTarget();
const EVENT_NAME = "mods-changed";

export function emitModsChanged(detail: ModsChangedEvent): void {
  target.dispatchEvent(new CustomEvent<ModsChangedEvent>(EVENT_NAME, { detail }));
}

export function onModsChanged(handler: (event: ModsChangedEvent) => void): () => void {
  const listener = (ev: Event) => handler((ev as CustomEvent<ModsChangedEvent>).detail);
  target.addEventListener(EVENT_NAME, listener);
  return () => target.removeEventListener(EVENT_NAME, listener);
}

export function normalizeFolderPath(p: string): string {
  return p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
}
