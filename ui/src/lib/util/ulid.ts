const ENC = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/** ULID-shaped id (26 chars, Crockford base32, time-prefixed). Used for scratch ids and executionIds. */
export function ulid(now: number = Date.now()): string {
  let t = now;
  let time = "";
  for (let i = 0; i < 10; i++) {
    time = ENC[t % 32] + time;
    t = Math.floor(t / 32);
  }
  const rnd = new Uint8Array(16);
  crypto.getRandomValues(rnd);
  let r = "";
  for (let i = 0; i < 16; i++) r += ENC[rnd[i] % 32];
  return time + r;
}
