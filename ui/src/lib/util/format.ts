export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${trim(bytes / 1024)} KB`;
  return `${trim(bytes / (1024 * 1024))} MB`;
}

function trim(n: number): string {
  return n >= 100 ? n.toFixed(0) : n >= 10 ? n.toFixed(1).replace(/\.0$/, "") : n.toFixed(2).replace(/\.?0+$/, "");
}

export function formatMs(ms: number): string {
  return `${Math.round(ms).toLocaleString("en-US")} ms`;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function startOfDay(ts: number): number {
  const d = new Date(ts);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** "now", "2m", "3h", "Yesterday", "Nov 12" (DESIGN §5.2). */
export function relativeTime(ts: number, now: number): string {
  const diff = Math.max(0, now - ts);
  if (diff < 60_000) return "now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m`;
  const today = startOfDay(now);
  if (ts >= today) return `${Math.floor(diff / 3_600_000)}h`;
  if (ts >= today - 86_400_000) return "Yesterday";
  const d = new Date(ts);
  const sameYear = d.getFullYear() === new Date(now).getFullYear();
  return `${MONTHS[d.getMonth()]} ${d.getDate()}${sameYear ? "" : `, ${d.getFullYear()}`}`;
}

/** "Sent 2m ago" style phrase. */
export function agoPhrase(ts: number, now: number): string {
  const r = relativeTime(ts, now);
  if (r === "now") return "just now";
  if (/^\d+[mh]$/.test(r)) return `${r} ago`;
  return r === "Yesterday" ? "yesterday" : `on ${r}`;
}

export function historyGroup(ts: number, now: number): "Today" | "Yesterday" | "Earlier" {
  const today = startOfDay(now);
  if (ts >= today) return "Today";
  if (ts >= today - 86_400_000) return "Yesterday";
  return "Earlier";
}

export function dateTime(ts: number): string {
  const d = new Date(ts);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${MONTHS[d.getMonth()]} ${d.getDate()}, ${hh}:${mm}`;
}

export function statusClass(status: number): string {
  if (status < 200) return "s1";
  if (status < 300) return "s2";
  if (status < 400) return "s3";
  if (status < 500) return "s4";
  return "s5";
}

const KNOWN = new Set(["GET", "POST", "PUT", "PATCH", "DELETE", "QUERY", "HEAD", "OPTIONS"]);

export function methodClass(method: string): string {
  return KNOWN.has(method) ? `m-${method.toLowerCase()}` : "m-custom";
}

/** Custom methods truncate at 7 chars (+ tooltip). */
export function methodLabel(method: string): string {
  return method.length > 7 ? method.slice(0, 6) + "…" : method;
}

export const ERROR_LABEL: Record<string, string> = {
  dns: "DNS",
  connect: "Connect",
  tls: "TLS",
  timeout: "Timeout",
  cancelled: "Cancelled",
  other: "Error",
};

export function stripScheme(url: string): string {
  return url.replace(/^[a-z][a-z0-9+.-]*:\/\//i, "");
}
