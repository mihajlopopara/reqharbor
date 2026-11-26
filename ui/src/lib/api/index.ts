/**
 * Backend adapter. In the Tauri webview, commands go to Rust via `invoke`; in a
 * plain browser (vite dev / Playwright) the same Commands map is served by an
 * in-memory mock (lazy-loaded, never part of the initial chunk).
 */
import { makeCall, type CommandError, type CommandName, type Commands, type Events } from "./types";

export type RawInvoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
export type Unlisten = () => void;

export interface Backend {
  invoke: RawInvoke;
  listen<E extends keyof Events>(event: E, handler: (payload: Events[E]) => void): Promise<Unlisten>;
}

export const isTauri: boolean = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

let backendPromise: Promise<Backend> | null = null;

async function tauriBackend(): Promise<Backend> {
  const [{ invoke }, ev] = await Promise.all([import("@tauri-apps/api/core"), import("@tauri-apps/api/event")]);
  return {
    invoke: (cmd, args) => invoke(cmd, args),
    listen: (event, handler) => ev.listen(event, (e) => handler(e.payload as never)),
  };
}

export function getBackend(): Promise<Backend> {
  if (!backendPromise) {
    backendPromise = isTauri ? tauriBackend() : import("./mock").then((m) => m.getMockBackend());
  }
  return backendPromise;
}

/** Test hook: replace the backend (e.g. a fresh mock). */
export function setBackend(b: Backend): void {
  backendPromise = Promise.resolve(b);
}

/** Normalise any rejection into a CommandError. */
export function toCommandError(e: unknown): CommandError {
  if (e && typeof e === "object" && "code" in e && "message" in e) return e as CommandError;
  if (typeof e === "string") {
    try {
      const parsed = JSON.parse(e);
      if (parsed && typeof parsed === "object" && "code" in parsed) return parsed as CommandError;
    } catch {
      /* plain string */
    }
    return { code: "internal", message: e };
  }
  return { code: "internal", message: e instanceof Error ? e.message : String(e) };
}

const typed = makeCall(async (cmd, args) => {
  const b = await getBackend();
  try {
    return await b.invoke(cmd, args);
  } catch (e) {
    throw toCommandError(e);
  }
});

/** Typed command call; rejects with CommandError. */
export const call: <K extends CommandName>(cmd: K, args: Commands[K]["args"]) => Promise<Commands[K]["result"]> =
  typed;

export async function listen<E extends keyof Events>(event: E, handler: (payload: Events[E]) => void): Promise<Unlisten> {
  const b = await getBackend();
  return b.listen(event, handler);
}

export function isCommandError(e: unknown): e is CommandError {
  return !!e && typeof e === "object" && "code" in e && "message" in e;
}

export function errorMessage(e: unknown): string {
  return toCommandError(e).message;
}

export * from "./types";
