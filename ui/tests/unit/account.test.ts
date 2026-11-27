import { beforeEach, describe, expect, it } from "vitest";
import { MockServer } from "../../src/lib/api/mock";
import { makeCall, type AuthStatus, type CommandError } from "../../src/lib/api/types";
import { deviceLine, elapsed, errorAction, initials, insecureAckFor, isInsecureUrl, methodButtonLabel, methodName, parseServerUrl } from "../../src/lib/util/account";
import { MSG_BROWSER_FAILED } from "../../src/lib/api/mock/account";
import { mergeKnownServer } from "../../src/lib/stores/app.svelte";

let server: MockServer;
let call: ReturnType<typeof makeCall>;
let events: AuthStatus[];

async function rejects(p: Promise<unknown>): Promise<CommandError> {
  try {
    await p;
  } catch (e) {
    return e as CommandError;
  }
  throw new Error("expected rejection");
}

function make(opts: ConstructorParameters<typeof MockServer>[0] = {}) {
  server = new MockServer({ latency: 0, ...opts, accounts: { authDelay: 0, ...(opts.accounts ?? {}) } });
  call = makeCall((cmd, args) => server.invoke(cmd, args));
  events = [];
  void server.backend().listen("auth://status", (s) => events.push(s));
}

beforeEach(() => make());

describe("server address normalisation (06 §9)", () => {
  it("matches the core's table", () => {
    const ok: [string, string][] = [
      ["rh.acme.dev", "https://rh.acme.dev"],
      ["  https://RH.Acme.DEV/  ", "https://rh.acme.dev"],
      ["HTTPS://rh.acme.dev:443", "https://rh.acme.dev"],
      ["http://rh.acme.dev:80/", "http://rh.acme.dev"],
      ["http://192.168.1.5:8080", "http://192.168.1.5:8080"],
      ["localhost:8080", "https://localhost:8080"],
    ];
    for (const [input, want] of ok) expect(parseServerUrl(input)).toEqual({ ok: true, origin: want });
    expect(parseServerUrl("https://rh.acme.dev/api")).toEqual({ ok: false, message: "Enter the server's address without a path" });
    expect(parseServerUrl("https://rh.acme.dev/?x=1").ok).toBe(false);
    expect(parseServerUrl("https://rh.acme.dev#top").ok).toBe(false);
    expect(parseServerUrl("ftp://rh.acme.dev").ok).toBe(false);
    expect(parseServerUrl("").ok).toBe(false);
  });
  it("warns only for plain http off loopback", () => {
    expect(isInsecureUrl("http://192.168.1.5:8080")).toBe(true);
    expect(isInsecureUrl("https://rh.acme.dev")).toBe(false);
    expect(isInsecureUrl("http://localhost:8080")).toBe(false);
    expect(isInsecureUrl("http://127.0.0.1:8080")).toBe(false);
  });
});

describe("wording helpers", () => {
  it("initials, method labels, timers, device lines", () => {
    expect(initials("Priya Shah")).toBe("PS");
    expect(initials("Sam")).toBe("S");
    expect(initials("", "olu@acme.dev")).toBe("O");
    expect(methodButtonLabel({ id: "github", label: "GitHub" })).toBe("Continue with GitHub");
    expect(methodButtonLabel({ id: "password", label: "Email and password" })).toBe("Sign in with email");
    expect(methodButtonLabel({ id: "oidc", label: "Acme SSO" })).toBe("Continue with Acme SSO");
    expect(methodButtonLabel({ id: "passkey", label: "Passkey" })).toBe("Continue with Passkey");
    expect(methodName("github")).toBe("GitHub");
    expect(elapsed(42_000)).toBe("0:42");
    expect(elapsed(725_000)).toBe("12:05");
    expect(errorAction("sign_in_timeout")).toBe("signIn");
    expect(errorAction("server_unreachable")).toBe("retry");
    expect(errorAction("loopback_unavailable")).toBe("advanced");
    expect(errorAction("rate_limited")).toBeNull();
    const now = Date.UTC(2026, 10, 27, 12);
    expect(deviceLine({ id: "a", deviceName: "x", createdAt: Date.UTC(2026, 10, 2), lastUsedDay: Date.UTC(2026, 10, 27), current: true }, now)).toBe(
      "Signed in Nov 2 · Active today",
    );
    expect(deviceLine({ id: "a", deviceName: "x", createdAt: null, lastUsedDay: Date.UTC(2026, 10, 20), current: false }, now)).toBe("Active Nov 20");
  });
  it("device dates: createdAt in local time, lastUsedDay as its UTC day", () => {
    const saved = process.env.TZ;
    try {
      // Honolulu is UTC-10: 05:00 UTC on Nov 2 is still Nov 1 there.
      process.env.TZ = "Pacific/Honolulu";
      const createdAt = Date.UTC(2026, 10, 2, 5);
      expect(new Date(createdAt).getDate()).toBe(1);
      const now = Date.UTC(2026, 10, 27, 12);
      expect(deviceLine({ id: "a", deviceName: "x", createdAt, lastUsedDay: Date.UTC(2026, 10, 20), current: false }, now)).toBe("Signed in Nov 1 · Active Nov 20");
      // Tokyo is UTC+9: 20:00 UTC on Nov 2 is Nov 3 there.
      process.env.TZ = "Asia/Tokyo";
      expect(deviceLine({ id: "a", deviceName: "x", createdAt: Date.UTC(2026, 10, 2, 20), lastUsedDay: Date.UTC(2026, 10, 26), current: false }, now)).toBe("Signed in Nov 3 · Active yesterday");
    } finally {
      if (saved === undefined) delete process.env.TZ;
      else process.env.TZ = saved;
    }
  });
  it("insecureAckFor reads the per-origin choice for what was typed", () => {
    const known = [
      { url: "http://10.0.0.2:8080", lastUsedAt: 1, insecureAck: true },
      { url: "http://10.0.0.3", lastUsedAt: 1, insecureAck: false },
    ];
    expect(insecureAckFor(known, "http://10.0.0.2:8080")).toBe(true);
    expect(insecureAckFor(known, " HTTP://10.0.0.2:8080/ ")).toBe(true);
    expect(insecureAckFor(known, "http://10.0.0.3")).toBe(false);
    expect(insecureAckFor(known, "http://10.0.0.4")).toBe(false);
    expect(insecureAckFor(known, "not a url/path")).toBe(false);
  });
  it("knownServers keeps the newest first, merged per origin", () => {
    let list = mergeKnownServer(undefined, "https://a.dev", { name: "A" }, 1);
    list = mergeKnownServer(list, "http://10.0.0.2", { insecureAck: true }, 2);
    list = mergeKnownServer(list, "https://a.dev", { lastUsedAt: 3, keepTyped: true }, 3);
    expect(list.map((k) => k.url)).toEqual(["https://a.dev", "http://10.0.0.2"]);
    expect(list[0]).toEqual({ url: "https://a.dev", name: "A", lastUsedAt: 3, keepTyped: true });
    for (let i = 0; i < 20; i++) list = mergeKnownServer(list, `https://s${i}.dev`, { lastUsedAt: 10 + i }, 10 + i);
    expect(list).toHaveLength(8);
  });
});

describe("mock discovery", () => {
  it("maps every server state", async () => {
    const d = await call("server_discover", { serverUrl: "reqharbor.acme.dev" });
    expect(d.status).toBe("ok");
    expect(d.canSignIn).toBe(true);
    expect(d.serverUrl).toBe("https://reqharbor.acme.dev");
    expect(d.info?.auth.methods.map((m) => m.id)).toEqual(["password", "github"]);
    const cases: [string, string, boolean][] = [
      ["https://unreachable.example", "unreachable", false],
      ["https://tokenonly.example", "token_sync_only", false],
      ["https://legacy-only.example", "token_sync_only", false],
      ["https://not-reqharbor.example", "not_reqharbor", false],
      ["https://too-old.example", "client_too_old", false],
      ["https://futureapi.example", "ok", true],
      ["https://setup.example", "ok", true],
    ];
    for (const [url, status, can] of cases) {
      const r = await call("server_discover", { serverUrl: url });
      expect([url, r.status, r.canSignIn]).toEqual([url, status, can]);
    }
    expect((await call("server_discover", { serverUrl: "https://setup.example" })).message).toMatch(/setup code/);
    expect((await call("server_discover", { serverUrl: "https://other-public.example" })).canonicalUrl).toBe("https://rh.acme.dev");
    expect((await call("server_discover", { serverUrl: "http://192.168.1.5:8080" })).insecure).toBe(true);
    expect((await rejects(call("server_discover", { serverUrl: "https://x.dev/api" }))).code).toBe("invalid");
  });
  it("no methods disables sign-in", async () => {
    make({ accounts: { methods: [] } });
    const d = await call("server_discover", { serverUrl: "https://reqharbor.acme.dev" });
    expect(d.canSignIn).toBe(false);
    expect(d.message).toMatch(/doesn't have sign-in set up/);
  });
});

describe("mock sign-in", () => {
  it("waits for the browser, then signs in", async () => {
    const started = await call("auth_sign_in_start", { serverUrl: "https://reqharbor.acme.dev", method: "github" });
    expect(started.signIn.state).toBe("waiting");
    expect(started.signIn.startedAt).not.toBeNull();
    expect(server.accounts.opened[0]).toContain("/auth/authorize?client_id=reqharbor-desktop");
    expect(server.accounts.opened[0]).toContain("method=github");
    await call("auth_sign_in_reopen", {});
    expect(server.accounts.opened).toHaveLength(2);
    await server.accounts.finishBrowser();
    const st = await call("auth_status", {});
    expect(st.signIn.state).toBe("success");
    expect(st.account?.name).toBe("Priya Shah");
    expect(st.account?.method).toBe("github");
    expect(events.map((e) => e.signIn.state)).toEqual(["checking", "waiting", "exchanging", "success"]);
    const devices = await call("auth_sessions", {});
    expect(devices.find((d) => d.current)).toBeTruthy();
    const other = devices.find((d) => !d.current)!;
    await call("auth_session_revoke", { sessionId: other.id });
    expect(await call("auth_sessions", {})).toHaveLength(1);
    expect((await rejects(call("auth_session_revoke", { sessionId: st.account!.sessionId }))).code).toBe("invalid");
    const out = await call("auth_sign_out", {});
    expect(out.account).toBeNull();
    expect((await rejects(call("auth_sessions", {}))).code).toBe("not_signed_in");
  });
  it("cancel drops the pending sign-in", async () => {
    await call("auth_sign_in_start", { serverUrl: "https://reqharbor.acme.dev" });
    const st = await call("auth_sign_in_cancel", {});
    expect(st.signIn.state).toBe("idle");
    await server.accounts.finishBrowser();
    expect((await call("auth_status", {})).account).toBeNull();
  });
  it("reports each failure with its code", async () => {
    for (const [auth, code] of [
      ["timeout", "sign_in_timeout"],
      ["denied", "sign_in_denied"],
      ["offline", "server_unreachable"],
      ["keychain", "keychain_unavailable"],
    ] as const) {
      make({ accounts: { auth } });
      await call("auth_sign_in_start", { serverUrl: "https://reqharbor.acme.dev" });
      await server.accounts.finishBrowser();
      const st = await call("auth_status", {});
      expect([auth, st.signIn.state, st.signIn.error]).toEqual([auth, "error", code]);
      expect(st.account).toBeNull();
    }
    make({ accounts: { auth: "no_port" } });
    expect((await rejects(call("auth_sign_in_start", { serverUrl: "https://reqharbor.acme.dev" }))).code).toBe("loopback_unavailable");
    make({ accounts: { auth: "email_in_use" } });
    await call("auth_sign_in_start", { serverUrl: "https://reqharbor.acme.dev" });
    await server.accounts.finishBrowser();
    expect((await call("auth_status", {})).signIn.state).toBe("waiting");
  });
  it("a browser that can't be opened keeps waiting with a copyable link", async () => {
    make({ accounts: { auth: "no_browser" } });
    const started = await call("auth_sign_in_start", { serverUrl: "https://reqharbor.acme.dev", method: "github" });
    expect(started.signIn.state).toBe("waiting");
    expect(started.signIn.message).toBe(MSG_BROWSER_FAILED);
    expect(started.signIn.authorizeUrl).toContain("https://reqharbor.acme.dev/auth/authorize?client_id=reqharbor-desktop");
    expect(server.accounts.opened).toHaveLength(0);
    // The user opened the link by hand: the sign-in still finishes.
    await server.accounts.finishBrowser();
    const st = await call("auth_status", {});
    expect(st.signIn.state).toBe("success");
    expect(st.signIn.message).toBeNull();
    expect(st.signIn.authorizeUrl).toBeNull();
  });
  it("Cancel and a new start do nothing while the code is being exchanged", async () => {
    await call("auth_sign_in_start", { serverUrl: "https://reqharbor.acme.dev", method: "github" });
    // Freeze the flow in "exchanging" (the mock's exchange is otherwise instant at latency 0).
    server.accounts.status.signIn.state = "exchanging";
    expect((await call("auth_sign_in_cancel", {})).signIn.state).toBe("exchanging");
    expect((await rejects(call("auth_sign_in_start", { serverUrl: "https://reqharbor.acme.dev" }))).code).toBe("invalid");
    expect((await call("auth_status", {})).signIn.state).toBe("exchanging");
    // Waiting can still be cancelled.
    server.accounts.status.signIn.state = "waiting";
    expect((await call("auth_sign_in_cancel", {})).signIn.state).toBe("idle");
  });
  it("refuses servers it can't sign in to", async () => {
    expect((await rejects(call("auth_sign_in_start", { serverUrl: "https://tokenonly.example" }))).code).toBe("token_sync_only");
    expect((await rejects(call("auth_sign_in_start", { serverUrl: "https://unreachable.example" }))).code).toBe("server_unreachable");
    expect((await rejects(call("auth_sign_in_start", { serverUrl: "https://too-old.example" }))).code).toBe("client_too_old");
    expect((await call("auth_status", {})).signIn.error).toBe("client_too_old");
  });
  it("plain http needs Continue anyway (argument or knownServers)", async () => {
    expect((await rejects(call("auth_sign_in_start", { serverUrl: "http://192.168.1.5:8080" }))).code).toBe("insecure_unconfirmed");
    expect((await call("auth_sign_in_start", { serverUrl: "http://192.168.1.5:8080", insecureConfirmed: true })).signIn.state).toBe("waiting");
    await call("auth_sign_in_cancel", {});
    const s = await call("get_settings", {});
    await call("save_settings", { settings: { ...s, knownServers: [{ url: "http://192.168.1.5:8080", lastUsedAt: 1, insecureAck: true }] } });
    expect((await call("auth_sign_in_start", { serverUrl: "http://192.168.1.5:8080" })).signIn.state).toBe("waiting");
  });
  it("launch states and the session-ended banner", async () => {
    make({ accounts: { account: "signInRequired" } });
    let st = await call("auth_status", {});
    expect(st.account).not.toBeNull();
    expect(st.signInRequired).toBe(true);
    expect((await rejects(call("auth_sessions", {}))).code).toBe("sign_in_required");
    make({ accounts: { account: "signedIn" } });
    server.accounts.expireSession();
    expect(events.at(-1)?.signInRequired).toBe(true);
    make({ accounts: { account: "keychainUnreadable" } });
    st = await call("auth_status", {});
    expect(st.keychainUnreadable).toBe(true);
    expect(st.account).toBeNull();
    make({ accounts: { account: "file" } });
    st = await call("auth_status", {});
    expect(st.keychain).toBe("file");
    expect(st.keychainNote).toMatch(/stored in a file/);
  });
  it("the accounts flag reaches bootstrap", async () => {
    expect((await call("bootstrap", {})).accounts).toBe(true);
    make({ accountsEnabled: false });
    expect((await call("bootstrap", {})).accounts).toBe(false);
  });
});
