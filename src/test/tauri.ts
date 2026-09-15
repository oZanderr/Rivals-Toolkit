import { vi } from "vitest";

/// A promise the test resolves by hand.
///
/// Detection on a large config mod takes over a second, and the bugs that live in this component
/// are about what happens while a call is still out. Holding one open is how a test reproduces
/// that without depending on timing.
export function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

type Args = Record<string, unknown>;
type Handler = (args: Args) => unknown;

export interface TauriMock {
  invoke: (cmd: string, args?: Args) => Promise<unknown>;
  on(cmd: string, handler: Handler): TauriMock;
  calls: { cmd: string; args: Args }[];
  callsTo(cmd: string): Args[];
}

/// Stands in for the Rust side. Commands with no handler throw by name rather than returning
/// undefined, so a test that forgets one says which.
export function createTauri(): TauriMock {
  const handlers = new Map<string, Handler>();
  const calls: { cmd: string; args: Args }[] = [];

  const mock: TauriMock = {
    invoke: async (cmd, args = {}) => {
      calls.push({ cmd, args });
      const handler = handlers.get(cmd);
      if (!handler) throw new Error(`no test handler for invoke("${cmd}")`);
      return handler(args);
    },
    on(cmd, handler) {
      handlers.set(cmd, handler);
      return mock;
    },
    calls,
    callsTo: (cmd) => calls.filter((c) => c.cmd === cmd).map((c) => c.args),
  };
  return mock;
}

let current: TauriMock | null = null;

export function installTauri(mock: TauriMock): void {
  current = mock;
}

/// What the module mock forwards to, so `vi.mock` can be hoisted while the mock it talks to is
/// built per test.
export function invokeProxy(cmd: string, args?: Args): Promise<unknown> {
  if (!current) throw new Error(`invoke("${cmd}") before installTauri()`);
  return current.invoke(cmd, args);
}

export const windowStub = {
  getCurrentWindow: () => ({
    onDragDropEvent: vi.fn(async () => () => {}),
  }),
};

export const dialogStub = {
  open: vi.fn(async () => null),
};
