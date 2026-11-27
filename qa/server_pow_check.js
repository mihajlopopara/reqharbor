// Unit check of the login proof of work, server/internal/web/static/pow.js (06-go-server.md §5.9).
// Runs pow.js in a sandbox WITHOUT WebCrypto (as on an http PUBLIC_URL that is not loopback, where
// crypto.subtle is undefined) against a fake form, and checks the nonce it submits with node's
// own SHA-256. Also checks that a broken challenge leaves the button disabled with a visible
// message. Run in the dev container from the repo root:
//
//   docker compose run --rm dev node qa/server_pow_check.js
//
// qa/run_live_server_tests.sh runs it too.
"use strict";
const fs = require("fs");
const path = require("path");
const vm = require("vm");
const nodeCrypto = require("crypto");

const src = fs.readFileSync(path.join(__dirname, "..", "server/internal/web/static/pow.js"), "utf8");

function leadingZeroBits(buf) {
  let n = 0;
  for (const b of buf) {
    if (b === 0) { n += 8; continue; }
    let x = b;
    while (!(x & 128)) { n++; x <<= 1; }
    break;
  }
  return n;
}

function run(challenge, difficulty) {
  const input = { value: "" };
  const button = { disabled: false };
  const status = { textContent: "Your browser is checking this sign-in." };
  const form = {
    getAttribute: (n) => ({ "data-challenge": challenge, "data-difficulty": String(difficulty) })[n],
    querySelector: (q) => (q === "button" ? button : q === "input[name=pow]" ? input : null),
  };
  const sandbox = {
    document: {
      querySelector: (q) => (q === "form[data-challenge]" ? form : null),
      getElementById: (id) => (id === "pow-status" ? status : null),
    },
    window: {}, // no window.crypto, no crypto.subtle
    atob: (s) => {
      if (/[^A-Za-z0-9+/=]/.test(s)) throw new Error("InvalidCharacterError"); // as browsers do
      return Buffer.from(s, "base64").toString("latin1");
    },
    setTimeout,
    Date,
  };
  vm.createContext(sandbox);
  vm.runInContext(src, sandbox);
  return { input, button, status };
}

function fail(msg) {
  console.error("pow check FAILED: " + msg);
  process.exit(1);
}

async function main() {
  if (/crypto\.subtle|crypto\.getRandomValues|TextEncoder/.test(src)) fail("pow.js depends on WebCrypto or TextEncoder");

  for (const difficulty of [1, 8, 16, 18]) {
    const raw = nodeCrypto.randomBytes(16);
    const challenge = raw.toString("base64url");
    const t0 = Date.now();
    const { input, button, status } = run(challenge, difficulty);
    if (!button.disabled) fail("the button is not disabled while the check runs");
    while (!input.value) {
      if (Date.now() - t0 > 60000) fail(`no proof after 60 s at difficulty ${difficulty}`);
      await new Promise((r) => setTimeout(r, 20));
    }
    const [c, nonce] = input.value.split(".");
    if (c !== challenge || !/^[0-9]+$/.test(nonce)) fail(`malformed proof ${input.value}`);
    const sum = nodeCrypto.createHash("sha256").update(Buffer.concat([raw, Buffer.from(nonce, "ascii")])).digest();
    if (leadingZeroBits(sum) < difficulty) fail(`proof ${input.value} has ${leadingZeroBits(sum)} bits, want ${difficulty}`);
    if (button.disabled || !/Ready/.test(status.textContent)) fail("the form was not re-enabled");
    console.log(`pow ok: difficulty ${difficulty}, nonce ${nonce}, ${Date.now() - t0} ms`);
  }

  // A challenge that cannot be decoded: the button stays disabled and the page says so.
  const bad = run("!!!not base64!!!", 18);
  await new Promise((r) => setTimeout(r, 50));
  if (!bad.button.disabled || bad.input.value || !/could not finish the check/.test(bad.status.textContent)) {
    fail("a broken challenge did not leave a disabled form with a visible message");
  }
  console.log("pow ok: broken challenge shows the message");
}

main().catch((e) => fail(e && e.stack ? e.stack : String(e)));
