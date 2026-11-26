/** Global keyboard shortcuts (PRODUCT §6, DESIGN §7.2). ⌘ on macOS, Ctrl elsewhere. */
export type ShortcutAction =
  | "send"
  | "importCurl"
  | "envPicker"
  | "newRequest"
  | "save"
  | "focusUrl"
  | "copyCurl"
  | "format"
  | "focusFilter"
  | "tabActivity"
  | "tabCollections"
  | "tabEnv"
  | "rename"
  | "toggleSidebar"
  | "settings"
  | "nextPane"
  | "prevPane";

export interface KeyLike {
  key: string;
  code?: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

export const isMac = (platform: string): boolean => platform === "macos";

/** Letter/digit of the event, layout-independent where possible (KeyU -> "u", Digit1 -> "1"). */
function baseKey(e: KeyLike): string {
  if (e.code) {
    if (/^Key[A-Z]$/.test(e.code)) return e.code.slice(3).toLowerCase();
    if (/^Digit[0-9]$/.test(e.code)) return e.code.slice(5);
    if (e.code === "Comma") return ",";
  }
  return e.key.length === 1 ? e.key.toLowerCase() : e.key;
}

export function matchShortcut(e: KeyLike, platform: string): ShortcutAction | null {
  const mac = isMac(platform);
  const mod = mac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
  if (e.altKey) return null;
  if (!mod) {
    if (e.metaKey || e.ctrlKey) return null;
    if (e.key === "F2" && !e.shiftKey) return "rename";
    if (e.key === "F6") return e.shiftKey ? "prevPane" : "nextPane";
    return null;
  }
  const k = baseKey(e);
  if (e.shiftKey) {
    switch (k) {
      case "c":
        return "copyCurl";
      case "f":
        return "format";
      case "e":
        return "focusFilter";
      default:
        return null;
    }
  }
  switch (k) {
    case "Enter":
      return "send";
    case "u":
      return "importCurl";
    case "e":
      return "envPicker";
    case "n":
      return "newRequest";
    case "s":
      return "save";
    case "l":
      return "focusUrl";
    case "1":
      return "tabActivity";
    case "2":
      return "tabCollections";
    case "3":
      return "tabEnv";
    case "b":
      return "toggleSidebar";
    case ",":
      return "settings";
    default:
      return null;
  }
}

/** Key-cap labels for a shortcut, platform-correct (AC-KEY-2, DESIGN §5.18). */
export function shortcutKeys(spec: string, platform: string): string[] {
  const mac = isMac(platform);
  return spec.split("+").map((p) => {
    switch (p) {
      case "Mod":
        return mac ? "⌘" : "Ctrl";
      case "Shift":
        return mac ? "⇧" : "Shift";
      case "Alt":
        return mac ? "⌥" : "Alt";
      case "Enter":
        return mac ? "↵" : "Enter";
      default:
        return p;
    }
  });
}

/** Compact hint text, e.g. "⌘⇧C" or "Ctrl+Shift+C". */
export function shortcutText(spec: string, platform: string): string {
  const keys = shortcutKeys(spec, platform);
  return isMac(platform) ? keys.join("") : keys.join("+");
}

export const SHORTCUT_LIST: { label: string; spec: string }[] = [
  { label: "Send request / cancel", spec: "Mod+Enter" },
  { label: "Import cURL", spec: "Mod+U" },
  { label: "Change environment", spec: "Mod+E" },
  { label: "New request", spec: "Mod+N" },
  { label: "Save request", spec: "Mod+S" },
  { label: "Focus URL bar", spec: "Mod+L" },
  { label: "Copy as cURL", spec: "Mod+Shift+C" },
  { label: "Format body", spec: "Mod+Shift+F" },
  { label: "Focus sidebar filter", spec: "Mod+Shift+E" },
  { label: "Activity / Collections / Env", spec: "Mod+1/2/3" },
  { label: "Find in editor / response", spec: "Mod+F" },
  { label: "Rename selected item", spec: "F2" },
  { label: "Toggle sidebar", spec: "Mod+B" },
  { label: "Settings", spec: "Mod+," },
  { label: "Next / previous pane", spec: "F6" },
  { label: "Close dialog / cancel", spec: "Esc" },
];
