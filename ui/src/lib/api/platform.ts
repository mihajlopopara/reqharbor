/** Clipboard, file pickers and file saving — Tauri plugins in the app, DOM fallbacks in a browser. */
import { isTauri, call } from "./index";

export async function copyText(text: string): Promise<void> {
  if (isTauri) {
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
    await writeText(text);
    return;
  }
  // Test hook, set first so a test can read it right after the click.
  (window as unknown as { __lastCopied?: string }).__lastCopied = text;
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
}

export interface PickedFile {
  /** Absolute path (Tauri) or a pseudo path `browser://<name>` (browser). */
  path: string;
  name: string;
  size: number | null;
  /** Text content, only read in browser mode when `readText` is set. */
  text?: string;
}

export async function pickFile(opts: {
  title?: string;
  filters?: { name: string; extensions: string[] }[];
  readText?: boolean;
}): Promise<PickedFile | null> {
  if (isTauri) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const res = await open({ multiple: false, directory: false, title: opts.title, filters: opts.filters });
    if (!res || Array.isArray(res)) return null;
    const path = String(res);
    return { path, name: path.split(/[\\/]/).pop() ?? path, size: null };
  }
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    if (opts.filters) input.accept = opts.filters.flatMap((f) => f.extensions.map((e) => "." + e)).join(",");
    input.onchange = async () => {
      const f = input.files?.[0];
      if (!f) return resolve(null);
      const picked: PickedFile = { path: `browser://${f.name}`, name: f.name, size: f.size };
      if (opts.readText) picked.text = await f.text();
      resolve(picked);
    };
    input.click();
  });
}

/** Save a cached response body to a user-chosen file. Returns false if cancelled. */
export async function saveResponseToFile(responseId: string, suggestedName: string): Promise<boolean> {
  if (isTauri) {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({ defaultPath: suggestedName, title: "Save response body" });
    if (!path) return false;
    await call("save_response_body", { responseId, path });
    return true;
  }
  const buf = await call("get_response_body", { responseId });
  const url = URL.createObjectURL(new Blob([buf]));
  const a = document.createElement("a");
  a.href = url;
  a.download = suggestedName;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
  return true;
}

export function fileNameOf(path: string): string {
  return path.replace(/^browser:\/\//, "").split(/[\\/]/).pop() ?? path;
}

/** Subscribe to OS file drops (Tauri only; absolute paths). */
export async function onFileDrop(handler: (paths: string[]) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  return getCurrentWebview().onDragDropEvent((e) => {
    if (e.payload.type === "drop") handler(e.payload.paths);
  });
}
