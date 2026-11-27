/** Wording helpers for accounts and sign-in (UX §3, §4; 06-go-server §9). */
import type { AuthMethod, CommandErrorCode, KnownServer, SessionInfo } from "../api/types";

/** "PS" for "Priya Shah"; the email's first letter when there is no name. */
export function initials(name: string, email = ""): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  if (words.length >= 2) return (first(words[0]) + first(words[words.length - 1])).toUpperCase();
  if (words.length === 1) return first(words[0]).toUpperCase();
  return first(email.trim()).toUpperCase() || "?";
}

function first(s: string): string {
  return Array.from(s)[0] ?? "";
}

/** Button label per method; unknown ids get "Continue with {label}" (06 §9). */
export function methodButtonLabel(m: AuthMethod): string {
  switch (m.id) {
    case "password":
      return "Sign in with email";
    case "github":
      return "Continue with GitHub";
    case "google":
      return "Continue with Google";
    default:
      return `Continue with ${m.label || m.id}`;
  }
}

/** "Signed in with GitHub" wording for Account.method. */
export function methodName(id: string): string {
  switch (id) {
    case "password":
      return "Email and password";
    case "github":
      return "GitHub";
    case "google":
      return "Google";
    case "oidc":
      return "Single sign-on";
    case "":
      return "";
    default:
      return id;
  }
}

/** Host (and port) of an origin, as the UI names a server. */
export function serverHost(origin: string): string {
  return origin.replace(/^[a-z]+:\/\//i, "").replace(/\/$/, "");
}

/** "0:42", "12:05". */
export function elapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** What the error row's action button says, if any (UX §3.2 error table). */
export function errorAction(code: CommandErrorCode | null): "retry" | "signIn" | "advanced" | null {
  switch (code) {
    case "server_unreachable":
    case "keychain_unavailable":
    case "server_error":
      return "retry";
    case "sign_in_timeout":
    case "sign_in_denied":
    case "sign_in_failed":
    case "sign_in_required":
      return "signIn";
    case "loopback_unavailable":
    case "token_sync_only":
    case "no_sign_in_methods":
      return "advanced";
    default:
      return null;
  }
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** "Nov 2": local time by default; `utc` for values that are UTC days. */
function shortDate(ms: number, utc = false): string {
  const d = new Date(ms);
  return utc ? `${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()}` : `${MONTHS[d.getMonth()]} ${d.getDate()}`;
}

/**
 * "Signed in Nov 2 · Active today" (UX §4.2). `createdAt` is an instant, shown in local time.
 * `lastUsedDay` is a UTC day on the server (midnight UTC, 06 `sessions.last_used_day`), so
 * it is compared with today's UTC day and printed as that UTC date: near midnight UTC "today"
 * can be off by one from the local calendar, which a per-day value can't avoid.
 */
export function deviceLine(s: SessionInfo, now: number): string {
  const parts: string[] = [];
  if (s.createdAt) parts.push(`Signed in ${shortDate(s.createdAt)}`);
  if (s.lastUsedDay !== null) {
    const today = Math.floor(now / 86_400_000);
    const day = Math.floor(s.lastUsedDay / 86_400_000);
    parts.push(day >= today ? "Active today" : day === today - 1 ? "Active yesterday" : `Active ${shortDate(s.lastUsedDay, true)}`);
  }
  return parts.join(" · ");
}

/**
 * Same rules as `normalize_server_url` (crates/core/src/server_info.rs): trim, add https://,
 * lowercase scheme and host, drop the default port and a trailing "/", refuse a path.
 */
export function parseServerUrl(input: string): { ok: true; origin: string } | { ok: false; message: string } {
  const t = input.trim();
  if (!t) return { ok: false, message: "Enter the server's address" };
  const withScheme = t.includes("://") ? t : `https://${t}`;
  let u: URL;
  try {
    u = new URL(withScheme);
  } catch {
    return { ok: false, message: "Enter a valid server address, like https://reqharbor.example.com" };
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return { ok: false, message: "Server URL must start with https:// or http://" };
  if (u.username || u.password || !u.hostname) return { ok: false, message: "Enter a valid server address, like https://reqharbor.example.com" };
  const rest = withScheme.replace(/^[a-z]+:\/\/[^/?#]*/i, "");
  if (rest !== "" && rest !== "/") return { ok: false, message: "Enter the server's address without a path" };
  return { ok: true, origin: `${u.protocol}//${u.host.toLowerCase()}` };
}

/** `localhost`, 127.0.0.0/8 or ::1. */
export function isLoopbackHost(host: string): boolean {
  const h = host.replace(/^\[|\]$/g, "").toLowerCase();
  return h === "localhost" || h === "::1" || /^127\.\d+\.\d+\.\d+$/.test(h);
}

/** Plain http to a non-loopback host (06 §9 warning). */
export function isInsecureUrl(input: string): boolean {
  const p = parseServerUrl(input);
  if (!p.ok) return false;
  const u = new URL(p.origin);
  return u.protocol === "http:" && !isLoopbackHost(u.hostname);
}

/** Whether `knownServers` holds the plain-http "Continue anyway" for what was typed (06 §9). */
export function insecureAckFor(known: readonly KnownServer[], input: string): boolean {
  const p = parseServerUrl(input);
  return p.ok && known.some((k) => k.url === p.origin && k.insecureAck === true);
}
