export type PakEventSource = "PakIniEditor" | "PakTweaks";

export interface PakChangedEvent {
  pakPath: string;
  source: PakEventSource;
}

const target = new EventTarget();
const EVENT_NAME = "pak-changed";

export function emitPakChanged(detail: PakChangedEvent): void {
  target.dispatchEvent(new CustomEvent<PakChangedEvent>(EVENT_NAME, { detail }));
}

export function onPakChanged(handler: (event: PakChangedEvent) => void): () => void {
  const listener = (ev: Event) => handler((ev as CustomEvent<PakChangedEvent>).detail);
  target.addEventListener(EVENT_NAME, listener);
  return () => target.removeEventListener(EVENT_NAME, listener);
}
