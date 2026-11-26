// Identifiers and token primitives.

const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const BASE62 = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

function randomBytes(n: number): Uint8Array {
  const b = new Uint8Array(n);
  crypto.getRandomValues(b);
  return b;
}

/** ULID: 48-bit ms timestamp + 80 bits of randomness, Crockford base32 (26 chars). */
export function ulid(now = Date.now()): string {
  let ts = "";
  let t = now;
  for (let i = 0; i < 10; i++) {
    ts = CROCKFORD[t % 32] + ts;
    t = Math.floor(t / 32);
  }
  const rnd = randomBytes(16);
  let r = "";
  for (let i = 0; i < 16; i++) r += CROCKFORD[rnd[i]! % 32];
  return ts + r;
}

function base64url(bytes: Uint8Array): string {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** Share id: 128 random bits as fixed-width (22 chars) base62. */
export const SHARE_ID_RE = /^[0-9A-Za-z]{22}$/;
export function shareId(): string {
  let n = 0n;
  for (const b of randomBytes(16)) n = (n << 8n) | BigInt(b);
  let out = "";
  for (let i = 0; i < 22; i++) {
    out = BASE62[Number(n % 62n)] + out;
    n /= 62n;
  }
  return out;
}

/** Workspace token: `rh_` + base64url(32 random bytes) (43 chars). */
export const TOKEN_RE = /^rh_[A-Za-z0-9_-]{43}$/;
export function newToken(): string {
  return "rh_" + base64url(randomBytes(32));
}

export async function sha256Hex(input: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(input));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Constant-time string comparison (length is not secret here: both are 64-char hex digests). */
export function timingSafeEqual(a: string, b: string): boolean {
  const ea = new TextEncoder().encode(a);
  const eb = new TextEncoder().encode(b);
  let diff = ea.length ^ eb.length;
  const len = Math.max(ea.length, eb.length);
  for (let i = 0; i < len; i++) diff |= (ea[i] ?? 0) ^ (eb[i] ?? 0);
  return diff === 0;
}
