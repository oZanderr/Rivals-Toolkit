const target = new EventTarget();
const EVENT_NAME = "settings-changed";

export function emitSettingsChanged(): void {
  target.dispatchEvent(new Event(EVENT_NAME));
}

export function onSettingsChanged(handler: () => void): () => void {
  target.addEventListener(EVENT_NAME, handler);
  return () => target.removeEventListener(EVENT_NAME, handler);
}
