/**
 * TS port of crates/core/src/urlx.rs — URL <-> Query rows sync (AC-REQ-2) for
 * zero-latency typing. Query values are raw (never decoded); building only
 * percent-encodes characters that would change the URL structure.
 */
import type { KeyValue } from "../api/types";

export function splitUrl(url: string): { base: string; query: string | null; fragment: string | null } {
  let rest = url;
  let fragment: string | null = null;
  const h = url.indexOf("#");
  if (h >= 0) {
    rest = url.slice(0, h);
    fragment = url.slice(h + 1);
  }
  const q = rest.indexOf("?");
  if (q >= 0) return { base: rest.slice(0, q), query: rest.slice(q + 1), fragment };
  return { base: rest, query: null, fragment };
}

export function parseQuery(query: string): KeyValue[] {
  return query
    .split("&")
    .filter((s) => s.length > 0)
    .map((seg) => {
      const i = seg.indexOf("=");
      return i >= 0
        ? { key: seg.slice(0, i), value: seg.slice(i + 1), enabled: true }
        : { key: seg, value: "", enabled: true };
    });
}

const enc = new TextEncoder();

function encodeWith(s: string, extra: string): string {
  let out = "";
  for (const ch of s) {
    const cp = ch.codePointAt(0)!;
    if (cp < 0x20 || cp === 0x7f || cp > 0x7e || ' &#"<>'.includes(ch) || extra.includes(ch)) {
      for (const b of enc.encode(ch)) out += "%" + b.toString(16).toUpperCase().padStart(2, "0");
    } else {
      out += ch;
    }
  }
  return out;
}

export const encodeQueryKey = (s: string): string => encodeWith(s, "=");
export const encodeQueryValue = (s: string): string => encodeWith(s, "");

export function buildQuery(params: KeyValue[]): string {
  const parts: string[] = [];
  for (const p of params) {
    if (!p.enabled) continue;
    if (p.key === "" && p.value === "") continue;
    parts.push(p.value === "" ? encodeQueryKey(p.key) : `${encodeQueryKey(p.key)}=${encodeQueryValue(p.value)}`);
  }
  return parts.join("&");
}

/** Query tab -> URL. Disabled rows are not included. */
export function paramsToUrl(url: string, params: KeyValue[]): string {
  const { base, fragment } = splitUrl(url);
  const q = buildQuery(params);
  let out = base;
  if (q) out += "?" + q;
  if (fragment !== null) out += "#" + fragment;
  return out;
}

/**
 * URL -> Query tab. Disabled rows keep their position; enabled rows are
 * replaced positionally by the URL's items (descriptions kept when the key is unchanged).
 */
export function urlToParams(url: string, existing: KeyValue[]): KeyValue[] {
  const { query } = splitUrl(url);
  const fresh = query === null ? [] : parseQuery(query);
  let i = 0;
  const out: KeyValue[] = [];
  for (const row of existing) {
    if (!row.enabled) {
      out.push({ ...row });
      continue;
    }
    if (i < fresh.length) {
      const next = fresh[i++];
      if (next.key === row.key && row.description !== undefined) next.description = row.description;
      out.push(next);
    }
  }
  while (i < fresh.length) out.push(fresh[i++]);
  return out;
}

function hostOf(rest: string): string {
  const end = rest.search(/[/?#]/);
  const authority = end >= 0 ? rest.slice(0, end) : rest;
  const at = authority.lastIndexOf("@");
  const hostPort = at >= 0 ? authority.slice(at + 1) : authority;
  if (hostPort.startsWith("[")) {
    const close = hostPort.indexOf("]");
    return close >= 0 ? hostPort.slice(0, close + 1) : hostPort;
  }
  const colon = hostPort.indexOf(":");
  return colon >= 0 ? hostPort.slice(0, colon) : hostPort;
}

/** AC-REQ-10: scheme-less URLs -> http:// for loopback, https:// otherwise. */
export function applyDefaultScheme(url: string): string {
  const t = url.trim();
  if (/^[A-Za-z][A-Za-z0-9+.-]*:\/\//.test(t) || t.startsWith("{{")) return t;
  const rest = t.replace(/^\/+/, "");
  const host = hostOf(rest).toLowerCase();
  const scheme = host === "localhost" || host === "127.0.0.1" || host === "[::1]" ? "http" : "https";
  return `${scheme}://${rest}`;
}
