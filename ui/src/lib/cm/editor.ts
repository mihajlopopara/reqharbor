/**
 * CodeMirror 6 setup (lazy chunk). Language packages are loaded on demand,
 * the theme maps to design tokens so it follows theme switches without reload,
 * and `{{var}}` decorations mirror the URL bar. Typing `{{` offers the
 * environment's variables and the `{{$dynamic}}` catalogue.
 */
import { Compartment, EditorSelection, EditorState, RangeSetBuilder, type Extension } from "@codemirror/state";
import {
  Decoration,
  EditorView,
  ViewPlugin,
  drawSelection,
  highlightActiveLine,
  hoverTooltip,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
  placeholder as placeholderExt,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { HighlightStyle, bracketMatching, foldGutter, foldKeymap, indentOnInput, syntaxHighlighting } from "@codemirror/language";
import { highlightSelectionMatches, openSearchPanel, search, searchKeymap } from "@codemirror/search";
import { autocompletion, type CompletionContext, type CompletionResult, type CompletionSource } from "@codemirror/autocomplete";
import { tags as t } from "@lezer/highlight";
import type { VarLookup } from "../util/vars";
import { VAR_RE, completeVar, dynamicInfo, dynamicTip, isResolvable } from "../util/vars";

export type EditorLanguage = "json" | "xml" | "html" | "graphql" | "text";

const langCache = new Map<EditorLanguage, Promise<Extension>>();

export function loadLanguage(lang: EditorLanguage): Promise<Extension> {
  let p = langCache.get(lang);
  if (!p) {
    p =
      lang === "json"
        ? import("@codemirror/lang-json").then((m) => m.json())
        : lang === "xml"
          ? import("@codemirror/lang-xml").then((m) => m.xml())
          : lang === "html"
            ? import("@codemirror/lang-html").then((m) => m.html())
            : lang === "graphql"
              ? import("cm6-graphql").then((m) => m.graphqlLanguageSupport())
              : Promise.resolve([]);
    langCache.set(lang, p);
  }
  return p;
}

const theme = EditorView.theme({
  "&": {
    color: "var(--text-1)",
    backgroundColor: "var(--bg-pane)",
    fontSize: "var(--fs-code)",
    height: "100%",
  },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "var(--font-mono)", lineHeight: "var(--lh-code)", overflow: "auto" },
  ".cm-content": { caretColor: "var(--text-1)", padding: "4px 0 16px" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--text-1)", borderLeftWidth: "1.5px" },
  ".cm-gutters": { backgroundColor: "var(--bg-pane)", color: "var(--text-3)", border: "none" },
  ".cm-lineNumbers .cm-gutterElement": { minWidth: "32px", padding: "0 8px 0 4px" }, // --text-3 at full opacity: ≥ 4.5:1
  ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--text-2)" },
  ".cm-activeLine": { backgroundColor: "var(--bg-code-active-line)" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection": {
    backgroundColor: "var(--accent-soft) !important",
  },
  ".cm-selectionMatch": { backgroundColor: "var(--bg-mark)" },
  ".cm-matchingBracket": { backgroundColor: "var(--bg-active)", outline: "none" },
  "&:not(.cm-focused) .cm-activeLine": { backgroundColor: "transparent" },
  "&:not(.cm-focused) .cm-activeLineGutter": { color: "var(--text-3)" },
  "&:not(.cm-focused) .cm-matchingBracket": { backgroundColor: "transparent", outline: "none" },
  ".cm-foldGutter .cm-gutterElement": { color: "var(--text-3)", opacity: "0", transition: "opacity var(--dur-fast)" },
  ".cm-gutters:hover .cm-foldGutter .cm-gutterElement": { opacity: "1" },
  ".cm-foldPlaceholder": { backgroundColor: "var(--bg-active)", border: "none", color: "var(--text-2)" },
  ".cm-placeholder": { color: "var(--text-3)" },
  ".cm-panels": { backgroundColor: "var(--bg-raised)", color: "var(--text-1)", borderColor: "var(--border-subtle)" },
  ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--border-subtle)" },
  ".cm-search": { fontSize: "var(--fs-sm)", padding: "6px 8px" },
  ".cm-search input, .cm-search button": { fontSize: "var(--fs-sm)" },
  ".cm-textfield": {
    backgroundColor: "var(--bg-input)",
    border: "1px solid var(--border-subtle)",
    borderRadius: "var(--r-sm)",
    color: "var(--text-1)",
    padding: "2px 6px",
  },
  ".cm-button": {
    backgroundImage: "none",
    backgroundColor: "var(--bg-input)",
    border: "1px solid var(--border-subtle)",
    borderRadius: "var(--r-sm)",
    color: "var(--text-1)",
  },
  ".cm-searchMatch": { backgroundColor: "var(--bg-mark)" },
  ".cm-searchMatch-selected": { outline: "1px solid var(--warning)" },
  ".cm-var": { color: "var(--var-fg)", backgroundColor: "var(--var-bg)", borderRadius: "var(--r-xs)" },
  ".cm-var-bad": {
    color: "var(--var-unresolved-fg)",
    backgroundColor: "var(--var-unresolved-bg)",
    borderRadius: "var(--r-xs)",
    textDecoration: "underline wavy",
    textUnderlineOffset: "3px",
  },
  ".cm-var-dyn": { color: "var(--var-dynamic-fg)", backgroundColor: "var(--var-dynamic-bg)", borderRadius: "var(--r-xs)" },
  ".cm-var-tip": { padding: "4px 8px", fontSize: "var(--fs-sm)", fontFamily: "var(--font-ui)", color: "var(--text-1)" },
  ".cm-var-val": { fontFamily: "var(--font-mono)", border: "0", background: "none", padding: "0", color: "inherit" },
  "button.cm-var-val": { color: "var(--accent-fg)", cursor: "pointer" },
  ".cm-var-src": { color: "var(--text-3)" },
  ".cm-tooltip": { backgroundColor: "var(--bg-raised)", border: "none", boxShadow: "var(--shadow-popover)", borderRadius: "var(--r-sm)" },
  ".cm-tooltip.cm-tooltip-autocomplete > ul": { fontFamily: "var(--font-mono)", fontSize: "var(--fs-sm)", maxHeight: "14em", padding: "4px 0" },
  ".cm-tooltip.cm-tooltip-autocomplete > ul > li": { padding: "2px 8px", color: "var(--text-1)" },
  ".cm-tooltip.cm-tooltip-autocomplete > ul > li[aria-selected]": { backgroundColor: "var(--accent-soft)", color: "var(--text-1)" },
  ".cm-completionLabel": { color: "var(--var-fg)" },
  ".cm-completion-dynamic .cm-completionLabel": { color: "var(--var-dynamic-fg)" },
  ".cm-completion-gql .cm-completionLabel": { color: "var(--text-1)" },
  ".cm-tooltip.cm-completionInfo": { maxWidth: "320px", padding: "6px 8px", fontFamily: "var(--font-ui)", fontSize: "var(--fs-sm)", color: "var(--text-2)" },
  // AC-GQL-6: hover docs and lint (danger / warning wavy underlines, like unresolved variables).
  ".cm-gql-tip": { padding: "6px 8px", maxWidth: "360px", fontSize: "var(--fs-sm)", fontFamily: "var(--font-ui)", color: "var(--text-1)" },
  ".cm-gql-sig": { fontFamily: "var(--font-mono)", color: "var(--syn-key)", whiteSpace: "pre-wrap" },
  ".cm-gql-doc": { color: "var(--text-2)", marginTop: "4px", whiteSpace: "pre-wrap" },
  ".cm-lintRange-error": { backgroundImage: "none", textDecoration: "underline wavy var(--danger)", textUnderlineOffset: "3px" },
  ".cm-lintRange-warning, .cm-lintRange-info": { backgroundImage: "none", textDecoration: "underline wavy var(--warning)", textUnderlineOffset: "3px" },
  ".cm-tooltip-lint": { padding: "0" },
  ".cm-diagnostic": { padding: "4px 8px", fontSize: "var(--fs-sm)", fontFamily: "var(--font-ui)", color: "var(--text-1)", borderLeftWidth: "3px", whiteSpace: "pre-wrap", maxWidth: "420px" },
  ".cm-diagnostic-error": { borderLeftColor: "var(--danger)" },
  ".cm-diagnostic-warning, .cm-diagnostic-info": { borderLeftColor: "var(--warning)" },
  ".cm-completionDetail": { color: "var(--text-3)", fontStyle: "normal", fontFamily: "var(--font-ui)", marginLeft: "12px" },
});

const highlight = HighlightStyle.define([
  { tag: t.propertyName, color: "var(--syn-key)" },
  { tag: [t.string, t.special(t.string), t.attributeValue], color: "var(--syn-string)" },
  { tag: t.number, color: "var(--syn-number)" },
  { tag: t.bool, color: "var(--syn-bool)" },
  { tag: t.null, color: "var(--syn-null)" },
  { tag: [t.punctuation, t.bracket, t.separator, t.angleBracket], color: "var(--syn-punct)" },
  { tag: [t.tagName, t.typeName], color: "var(--syn-tag)" },
  { tag: t.attributeName, color: "var(--syn-attr)" },
  { tag: [t.comment, t.meta, t.processingInstruction, t.documentMeta], color: "var(--syn-comment)" },
  { tag: t.keyword, color: "var(--syn-bool)" },
  // GraphQL (cm6-graphql): type names, $variables, @directives, arguments, enum values.
  { tag: t.atom, color: "var(--syn-tag)" },
  { tag: t.variableName, color: "var(--syn-number)" },
  { tag: t.modifier, color: "var(--syn-attr)" },
  { tag: t.special(t.name), color: "var(--syn-number)" },
  { tag: t.invalid, color: "var(--danger)" },
]);

function varPlugin(getLookup: () => VarLookup | null): Extension {
  const varMark = Decoration.mark({ class: "cm-var" });
  const dynMark = Decoration.mark({ class: "cm-var-dyn" });
  const badMark = Decoration.mark({ class: "cm-var-bad" });
  const build = (view: EditorView): DecorationSet => {
    const lookup = getLookup();
    const b = new RangeSetBuilder<Decoration>();
    if (!lookup) return b.finish();
    for (const { from, to } of view.visibleRanges) {
      const text = view.state.doc.sliceString(from, to);
      for (const m of text.matchAll(VAR_RE)) {
        const s = from + m.index!;
        b.add(s, s + m[0].length, !isResolvable(m[1], lookup) ? badMark : m[1].startsWith("$") ? dynMark : varMark);
      }
    }
    return b.finish();
  };
  // AC-ENV-4: hover shows "value · from Env"; secrets masked until clicked.
  const hover = hoverTooltip((view, pos) => {
    const lookup = getLookup();
    if (!lookup) return null;
    const line = view.state.doc.lineAt(pos);
    for (const m of line.text.matchAll(VAR_RE)) {
      const from = line.from + m.index!;
      const to = from + m[0].length;
      if (pos < from || pos > to) continue;
      const dyn = m[1].startsWith("$") ? dynamicInfo(m[1]) : undefined;
      const info = m[1].startsWith("$") ? undefined : lookup.get(m[1]);
      return {
        pos: from,
        end: to,
        above: false,
        create() {
          const dom = document.createElement("div");
          dom.className = "cm-var-tip";
          if (dyn) {
            dom.textContent = dynamicTip(dyn);
          } else if (m[1].startsWith("$")) {
            dom.textContent = `Unknown dynamic variable ${m[0]} — sent as typed`;
          } else if (!info) {
            dom.textContent = `Unresolved variable ${m[0]} — not defined in the active environment or Global`;
          } else {
            const val = document.createElement(info.secret ? "button" : "span");
            val.className = "cm-var-val";
            val.textContent = info.secret ? "•••• · click to reveal" : info.value || "(empty)";
            if (info.secret) val.addEventListener("click", () => (val.textContent = info.value || "(empty)"));
            const src = document.createElement("span");
            src.className = "cm-var-src";
            src.textContent = ` · from ${info.environmentName}`;
            dom.append(val, src);
          }
          return { dom };
        },
      };
    }
    return null;
  });
  return [hover, ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) {
        this.decorations = build(view);
      }
      update(u: ViewUpdate) {
        if (u.docChanged || u.viewportChanged || u.transactions.some((tr) => tr.reconfigured)) this.decorations = build(u.view);
      }
    },
    { decorations: (v) => v.decorations },
  )];
}

/** `{{` autocomplete: environment variables, then the `$` catalogue. */
function varCompletionSource(getLookup: () => VarLookup | null): CompletionSource {
  return (ctx: CompletionContext): CompletionResult | null => {
    const lookup = getLookup();
    if (!lookup) return null;
    const line = ctx.state.doc.lineAt(ctx.pos);
    const col = ctx.pos - line.from;
    const res = completeVar(line.text.slice(0, col), line.text.slice(col), lookup);
    if (!res) return null;
    return {
      from: line.from + res.from,
      to: line.from + res.to,
      filter: false,
      options: res.options.map((o) => ({
        label: o.label,
        detail: o.detail,
        apply: o.insert,
        type: o.kind === "dynamic" ? "dynamic" : "variable",
      })),
    };
  };
}

/**
 * One autocompletion for both `{{variables}}` and an editor's own source (GraphQL fields,
 * variable names, set with `setAddons`): `override` would otherwise hide language sources.
 */
function completion(getLookup: () => VarLookup | null, getExtra: () => CompletionSource | null): Extension {
  const vars = varCompletionSource(getLookup);
  const extra: CompletionSource = (ctx) => getExtra()?.(ctx) ?? null;
  return autocompletion({
    override: [vars, extra],
    icons: false,
    optionClass: (c) => (c.type === "dynamic" ? "cm-completion-dynamic" : c.type === "gql" ? "cm-completion-gql" : ""),
  });
}

export interface EditorOptions {
  value: string;
  language: EditorLanguage;
  readOnly?: boolean;
  wrap?: boolean;
  /** Disable syntax highlighting (very large bodies). */
  plain?: boolean;
  lookup?: VarLookup | null;
  placeholder?: string;
  ariaLabel?: string;
  onChange?: (value: string) => void;
}

export interface EditorController {
  view: EditorView;
  setValue(v: string): void;
  getValue(): string;
  setLanguage(l: EditorLanguage, plain?: boolean): Promise<void>;
  setWrap(w: boolean): void;
  setLookup(l: VarLookup | null): void;
  /** Replace the selection with `text`, put the caret after it (or `caret` chars into it) and focus. */
  insert(text: string, caret?: number): void;
  /**
   * Editor-specific extras (GraphQL lint / hover, variables lint) and a completion source used
   * next to `{{variables}}` completion. Replaces the previous ones.
   */
  setAddons(extension: Extension, completion: CompletionSource | null): void;
  focus(): void;
  goTo(line: number, column: number): void;
  openSearch(): void;
  destroy(): void;
}

export async function createEditor(parent: HTMLElement, opts: EditorOptions): Promise<EditorController> {
  const langC = new Compartment();
  const wrapC = new Compartment();
  const varC = new Compartment();
  const addonC = new Compartment();
  let lookup = opts.lookup ?? null;
  let extraSource: CompletionSource | null = null;
  let suppress = false;
  const varExtensions = (): Extension => varPlugin(() => lookup);
  const lang = opts.plain ? [] : await loadLanguage(opts.language);
  const extensions: Extension[] = [
    lineNumbers(),
    foldGutter({ markerDOM: (open) => {
      const s = document.createElement("span");
      s.textContent = open ? "▾" : "▸";
      return s;
    } }),
    // DR-010: read-only viewers get no active-line band or bracket boxes
    opts.readOnly ? [] : [highlightActiveLineGutter(), highlightActiveLine(), bracketMatching()],
    history(),
    drawSelection(),
    indentOnInput(),
    highlightSelectionMatches(),
    search({ top: true }),
    syntaxHighlighting(highlight),
    theme,
    keymap.of([...searchKeymap, ...foldKeymap, ...historyKeymap, ...defaultKeymap, ...(opts.readOnly ? [] : [indentWithTab])]),
    langC.of(lang),
    wrapC.of(opts.wrap ? EditorView.lineWrapping : []),
    varC.of(lookup ? varExtensions() : []),
    opts.readOnly ? [] : completion(() => lookup, () => extraSource),
    addonC.of([]),
    EditorState.readOnly.of(!!opts.readOnly),
    EditorView.contentAttributes.of({ "aria-label": opts.ariaLabel ?? "Editor" }),
    EditorView.updateListener.of((u) => {
      if (u.docChanged && !suppress) opts.onChange?.(u.state.doc.toString());
    }),
  ];
  if (opts.placeholder) extensions.push(placeholderExt(opts.placeholder));
  const view = new EditorView({ parent, state: EditorState.create({ doc: opts.value, extensions }) });
  if (opts.readOnly) {
    view.scrollDOM.tabIndex = 0;
    view.scrollDOM.setAttribute("role", "region");
    view.scrollDOM.setAttribute("aria-label", opts.ariaLabel ?? "Editor");
  }

  return {
    view,
    setValue(v) {
      const cur = view.state.doc;
      if (cur.length === v.length && cur.toString() === v) return;
      suppress = true;
      view.dispatch({ changes: { from: 0, to: cur.length, insert: v }, selection: EditorSelection.cursor(Math.min(view.state.selection.main.head, v.length)) });
      suppress = false;
    },
    getValue: () => view.state.doc.toString(),
    async setLanguage(l, plain = false) {
      const ext = plain ? [] : await loadLanguage(l);
      view.dispatch({ effects: langC.reconfigure(ext) });
    },
    setWrap(w) {
      view.dispatch({ effects: wrapC.reconfigure(w ? EditorView.lineWrapping : []) });
    },
    setLookup(l) {
      lookup = l;
      view.dispatch({ effects: varC.reconfigure(l ? varExtensions() : []) });
    },
    insert(text, caret) {
      const from = view.state.selection.main.from;
      view.dispatch(view.state.replaceSelection(text));
      if (caret !== undefined) view.dispatch({ selection: { anchor: from + Math.max(0, Math.min(caret, text.length)) } });
      view.focus();
    },
    setAddons(extension, source) {
      extraSource = source;
      view.dispatch({ effects: addonC.reconfigure(extension) });
    },
    focus: () => view.focus(),
    goTo(line, column) {
      const ln = view.state.doc.line(Math.max(1, Math.min(line, view.state.doc.lines)));
      const pos = Math.min(ln.from + Math.max(0, column - 1), ln.to);
      view.dispatch({ selection: { anchor: pos }, scrollIntoView: true });
      view.focus();
    },
    openSearch: () => {
      openSearchPanel(view);
    },
    destroy: () => view.destroy(),
  };
}
