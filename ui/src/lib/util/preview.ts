/**
 * HTML response preview: the body is rendered in a sandboxed srcdoc iframe,
 * whose document URL is about:srcdoc — so relative stylesheet/image/font URLs
 * (`/assets/app.css`) would resolve to nothing. Inject a <base> pointing at the
 * response URL so they load from the origin that served the page.
 */

const BASE_RE = /<base\b[^>]*?\bhref\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))[^>]*>/i;
const HEAD_RE = /<head\b[^>]*>/i;
const HTML_RE = /<html\b[^>]*>/i;
const DOCTYPE_RE = /^\s*<!doctype[^>]*>/i;

function escapeAttr(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function httpUrl(raw: string | null | undefined, base?: string): string | null {
  if (!raw) return null;
  try {
    const u = new URL(raw, base);
    return u.protocol === "http:" || u.protocol === "https:" ? u.href : null;
  } catch {
    return null;
  }
}

/** Returns `html` with a <base href> resolving relative URLs against `pageUrl`. */
export function withBaseHref(html: string, pageUrl: string | null | undefined): string {
  const page = httpUrl(pageUrl);
  if (!page) return html;

  // Only the first <base> counts. Honour the page's own (resolved against the
  // response URL, since a relative base would otherwise resolve to about:srcdoc).
  const own = BASE_RE.exec(html);
  const href = (own && httpUrl(own[1] ?? own[2] ?? own[3], page)) || page;
  const tag = `<base href="${escapeAttr(href)}">`;

  const head = HEAD_RE.exec(html);
  if (head) return html.slice(0, head.index + head[0].length) + tag + html.slice(head.index + head[0].length);
  const root = HTML_RE.exec(html);
  if (root) return html.slice(0, root.index + root[0].length) + `<head>${tag}</head>` + html.slice(root.index + root[0].length);
  const doctype = DOCTYPE_RE.exec(html);
  if (doctype) return doctype[0] + tag + html.slice(doctype[0].length);
  return tag + html;
}
