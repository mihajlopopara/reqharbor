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

/** Session length: "850 ms", "4.2 s", "38 s", "3 min 05 s", "1 h 02 min". */
export function formatDuration(ms: number): string {
  if (ms < 1000) return formatMs(ms);
  const s = ms / 1000;
  if (s < 10) return `${s.toFixed(1)} s`;
  if (s < 60) return `${Math.round(s)} s`;
  const total = Math.round(s);
  if (total < 3600) return `${Math.floor(total / 60)} min ${String(total % 60).padStart(2, "0")} s`;
  return `${Math.floor(total / 3600)} h ${String(Math.floor((total % 3600) / 60)).padStart(2, "0")} min`;
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

/** Mirrors core `sync::unsupported_notice` (06-go-server §9): items the server doesn't accept yet. */
export function unsupportedNotice(n: number): string | null {
  if (n <= 0) return null;
  return n === 1
    ? "1 item isn't synced because this server is older than your app. Ask its admin to update reqharbor-server."
    : `${n} items aren't synced because this server is older than your app. Ask its admin to update reqharbor-server.`;
}

/** Shown once when a sync found the server restored from a backup (06-go-server §9). */
export const RESTORED_NOTICE = "The sync server was restored from a backup. Your data was sent to it again.";

/**
 * Tree badge for an item that isn't synced (`SkippedEntity.reason`): a short label and a tooltip.
 * The reason is "too_large" or the one the server rejected the change with.
 */
export function skippedReasonText(reason: string): { label: string; title: string } {
  switch (reason) {
    case "too_large":
    case "entity_too_large":
      return { label: "too large", title: "Not synced: too large (over 512 KB). Make it smaller to sync again." };
    case "invalid_json_value":
      return { label: "not synced", title: "Not synced: the server can't store one of its values. Change it to sync again." };
    case "forbidden_role":
      return { label: "not synced", title: "Not synced: your role can't change it on the server." };
    default:
      return { label: "not synced", title: `Not synced: the server rejected it (${reason}). Change it to sync again.` };
  }
}

/** Mirrors `sync_policy::skipped_warning` (src-tauri): one sentence per reason, joined with ". ". */
export function skippedWarning(skipped: { name: string; reason: string }[]): string | null {
  if (!skipped.length) return null;
  const rank = (r: string) => (r === "too_large" ? 0 : r === "invalid_json_value" ? 1 : r === "forbidden_role" ? 2 : 3);
  const reasons = [...new Set(skipped.map((s) => s.reason))].sort((a, b) => rank(a) - rank(b) || (a < b ? -1 : a > b ? 1 : 0));
  return reasons
    .map((reason) => {
      const items = skipped.filter((s) => s.reason === reason);
      const n = items.length;
      const one = n === 1;
      const head =
        reason === "too_large"
          ? `${n} ${one ? "item is" : "items are"} too large to sync (over 512 KB)`
          : reason === "invalid_json_value"
            ? `${n} ${one ? "item isn't" : "items aren't"} synced because the server can't store one of ${one ? "its" : "their"} values`
            : reason === "forbidden_role"
              ? `${n} ${one ? "item isn't" : "items aren't"} synced because your role can't change ${one ? "it" : "them"} on the server`
              : `${n} ${one ? "item was" : "items were"} rejected by the server (${reason})`;
      return `${head}: ${items.map((s) => s.name).join(", ")}`;
    })
    .join(". ");
}
