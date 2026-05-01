const target = new EventTarget();
const EVENT_NAME = "tweak-profiles-changed";

export function emitTweakProfilesChanged(): void {
  target.dispatchEvent(new Event(EVENT_NAME));
}

export function onTweakProfilesChanged(handler: () => void): () => void {
  target.addEventListener(EVENT_NAME, handler);
  return () => target.removeEventListener(EVENT_NAME, handler);
}
