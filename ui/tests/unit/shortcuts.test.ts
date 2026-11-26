import { describe, expect, it } from "vitest";
import { matchShortcut, shortcutKeys, shortcutText, type KeyLike } from "../../src/lib/util/shortcuts";

const key = (k: string, mods: Partial<KeyLike> = {}, code?: string): KeyLike => ({
  key: k,
  code,
  metaKey: false,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  ...mods,
});

describe("matchShortcut (PRODUCT §6)", () => {
  it("maps ⌘ shortcuts on macOS", () => {
    const m = (k: string, extra: Partial<KeyLike> = {}, code?: string) => matchShortcut(key(k, { metaKey: true, ...extra }, code), "macos");
    expect(m("Enter")).toBe("send");
    expect(m("u", {}, "KeyU")).toBe("importCurl");
    expect(m("e")).toBe("envPicker");
    expect(m("n")).toBe("newRequest");
    expect(m("s")).toBe("save");
    expect(m("l")).toBe("focusUrl");
    expect(m("C", { shiftKey: true }, "KeyC")).toBe("copyCurl");
    expect(m("F", { shiftKey: true }, "KeyF")).toBe("format");
    expect(m("E", { shiftKey: true }, "KeyE")).toBe("focusFilter");
    expect(m("1", {}, "Digit1")).toBe("tabActivity");
    expect(m("2", {}, "Digit2")).toBe("tabCollections");
    expect(m("3", {}, "Digit3")).toBe("tabEnv");
    expect(m(",", {}, "Comma")).toBe("settings");
    expect(m("b")).toBe("toggleSidebar");
  });

  it("uses Ctrl on Windows/Linux and ignores ⌘ there", () => {
    expect(matchShortcut(key("Enter", { ctrlKey: true }), "linux")).toBe("send");
    expect(matchShortcut(key("u", { ctrlKey: true }), "windows")).toBe("importCurl");
    expect(matchShortcut(key("u", { metaKey: true }), "windows")).toBeNull();
    expect(matchShortcut(key("u", { ctrlKey: true }), "macos")).toBeNull();
  });

  it("leaves ⌘F to the editor (not a global shortcut)", () => {
    expect(matchShortcut(key("f", { metaKey: true }), "macos")).toBeNull();
    expect(matchShortcut(key("f", { ctrlKey: true }), "linux")).toBeNull();
  });

  it("is layout independent via event.code", () => {
    // e.g. a Cyrillic layout reports key "г" for KeyU
    expect(matchShortcut(key("г", { ctrlKey: true }, "KeyU"), "linux")).toBe("importCurl");
  });

  it("handles F2 / F6 without modifiers and rejects Alt combos", () => {
    expect(matchShortcut(key("F2"), "macos")).toBe("rename");
    expect(matchShortcut(key("F6"), "linux")).toBe("nextPane");
    expect(matchShortcut(key("F6", { shiftKey: true }), "linux")).toBe("prevPane");
    expect(matchShortcut(key("u", { metaKey: true, altKey: true }), "macos")).toBeNull();
    expect(matchShortcut(key("a"), "macos")).toBeNull();
  });
});

describe("shortcut labels (AC-KEY-2)", () => {
  it("renders platform-correct key-caps", () => {
    expect(shortcutKeys("Mod+Enter", "macos")).toEqual(["⌘", "↵"]);
    expect(shortcutKeys("Mod+Enter", "windows")).toEqual(["Ctrl", "Enter"]);
    expect(shortcutText("Mod+Shift+C", "macos")).toBe("⌘⇧C");
    expect(shortcutText("Mod+Shift+C", "linux")).toBe("Ctrl+Shift+C");
  });
});
