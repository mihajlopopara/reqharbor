/**
 * Mock of the accounts commands (server_discover, auth_*), so Settings › Account
 * works in a plain browser and in Playwright. Mirrors the Rust behaviour in
 * src-tauri/src/auth_rt.rs and crates/core/src/{server_info,auth}.rs.
 *
 * Query-string switches:
 * - `?mockServer=ok|tokenOnly|legacyOnly|futureApi|otherPublicUrl|unreachable|notReqharbor|insecure|setup|tooOld`
 *   (the default for every address; a host containing one of these words, case-insensitive,
 *   overrides it, e.g. `https://unreachable.example`).
 * - `?mockMethods=password,github,oidc` (default `password,github`).
 * - `?mockAuth=ok|timeout|offline|denied|email_in_use|no_port|no_browser|keychain` — how the browser part ends
 *   (`no_browser`: the system browser can't be opened; the waiting view offers "Copy link").
 * - `?mockAuthDelay=<ms>`: how long the fake browser takes (default 1200; 0 = only when
 *   `window.__mock.accounts.finishBrowser()` is called).
 * - `?mockAccount=signedIn|signInRequired|keychainUnreadable|file` — the state at launch.
 */
import type {
  Account,
  AuthMethod,
  AuthStatus,
  CommandErrorCode,
  Events,
  ServerDiscovery,
  ServerInfo,
  SessionInfo,
  Settings,
  SignInStatus,
} from "../types";
import { fail } from "./errors";
import { isInsecureUrl, parseServerUrl, serverHost } from "../../util/account";

export type MockServerMode =
  | "ok"
  | "tokenOnly"
  | "legacyOnly"
  | "futureApi"
  | "otherPublicUrl"
  | "unreachable"
  | "notReqharbor"
  | "insecure"
  | "setup"
  | "tooOld";

export type MockAuthMode = "ok" | "timeout" | "offline" | "denied" | "email_in_use" | "no_port" | "no_browser" | "keychain";

export interface MockAccountOptions {
  server?: MockServerMode;
  methods?: string[];
  auth?: MockAuthMode;
  /** ms the fake browser takes; 0 = wait for finishBrowser(). */
  authDelay?: number;
  account?: "signedIn" | "signInRequired" | "keychainUnreadable" | "file";
}

const MODES: MockServerMode[] = ["tokenOnly", "legacyOnly", "futureApi", "otherPublicUrl", "unreachable", "notReqharbor", "insecure", "setup", "tooOld", "ok"];
const METHOD_LABELS: Record<string, string> = { password: "Email and password", github: "GitHub", google: "Google", oidc: "Acme SSO" };

export const MSG_TOKEN_ONLY = "This server only supports token sync. Use Sync › Advanced.";
export const MSG_NO_SYNC = "This server has no sync enabled yet. Ask its admin.";
export const MSG_NOT_REQHARBOR = "This doesn't look like a ReqHarbor server.";
export const MSG_SETUP = "This server hasn't been set up yet. Its admin will find the setup code in the server log.";
export const MSG_NO_METHODS = "This server doesn't have sign-in set up. Ask its admin, or sync with a token under Sync › Advanced.";
export const MSG_INSECURE = "This server doesn't use HTTPS. Your password, tokens and synced data can be read on the network.";

/** Same rules as `normalize_server_url` in crates/core/src/server_info.rs; rejects with "invalid". */
export function normalizeServerUrl(input: string): string {
  const p = parseServerUrl(input);
  return p.ok ? p.origin : fail("invalid", p.message);
}

export const displayHost = serverHost;
export const isInsecure = isInsecureUrl;

const idle = (): SignInStatus => ({ state: "idle", serverUrl: null, method: null, startedAt: null, error: null, message: null, authorizeUrl: null });

/** Same copy as `BROWSER_FAILED` in src-tauri/src/auth_rt.rs. */
export const MSG_BROWSER_FAILED = "Couldn't open your browser. Copy the sign-in link and open it in your browser.";

const errorMessages: Partial<Record<CommandErrorCode, string>> = {
  sign_in_timeout: "Sign-in timed out. Try again.",
  sign_in_denied: "Sign-in was cancelled in the browser.",
  server_unreachable: "Can't reach the server. Check your connection and try again.",
  loopback_unavailable: "ReqHarbor couldn't start sign-in on this computer. You can still sync with a token under Sync › Advanced.",
  keychain_unavailable: "ReqHarbor couldn't save your sign-in to the system keychain.",
  sign_in_failed: "Sign-in didn't finish. Start again from ReqHarbor.",
};

interface Hooks {
  emit: <E extends keyof Events>(event: E, payload: Events[E]) => void;
  delay: (ms: number) => Promise<void>;
  now: () => number;
  settings: () => Settings;
  /** A browser sign-in finished (the store's migration runs here). */
  onSignedIn?: (account: Account) => void;
  onSignedOut?: () => void;
}

export class MockAccounts {
  opts: Required<Omit<MockAccountOptions, "account">> & { account?: MockAccountOptions["account"] };
  status: AuthStatus;
  sessions: SessionInfo[] = [];
  /** Every authorize URL "opened in the browser" (for tests). */
  opened: string[] = [];
  private hooks: Hooks;
  private generation = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(opts: MockAccountOptions, hooks: Hooks) {
    this.hooks = hooks;
    this.opts = {
      server: opts.server ?? "ok",
      methods: opts.methods ?? ["password", "github"],
      auth: opts.auth ?? "ok",
      authDelay: opts.authDelay ?? 1200,
      account: opts.account,
    };
    this.status = {
      enabled: true,
      account: null,
      signInRequired: false,
      keychain: opts.account === "file" ? "file" : "system",
      keychainNote: opts.account === "file" ? "Your sign-in is stored in a file because no system keychain was found." : null,
      keychainUnreadable: opts.account === "keychainUnreadable",
      server: null,
      offline: false,
      signIn: idle(),
    };
    if (opts.account && opts.account !== "keychainUnreadable") {
      const origin = "https://reqharbor.acme.dev";
      const d = this.classify(origin);
      this.signedIn(origin, "github", d);
      this.status.signInRequired = opts.account === "signInRequired";
    }
  }

  private emit(): AuthStatus {
    const s = structuredClone(this.status);
    this.hooks.emit("auth://status", s);
    return s;
  }

  modeFor(origin: string): MockServerMode {
    const host = displayHost(origin).toLowerCase().replace(/[-_]/g, "");
    return MODES.find((m) => m !== "ok" && host.includes(m.toLowerCase().replace(/url$/, ""))) ?? this.opts.server;
  }

  info(origin: string, mode: MockServerMode): ServerInfo {
    const methods: AuthMethod[] = this.opts.methods.filter(Boolean).map((id) => ({ id, label: METHOD_LABELS[id] ?? id }));
    const host = displayHost(origin);
    const info: ServerInfo = {
      software: "reqharbor-server",
      version: "0.2.0",
      apiVersion: mode === "futureApi" ? 2 : 1,
      instanceId: "01JDMOCKINSTANCE0000000000",
      syncEpoch: "01JFMOCKEPOCH000000000000",
      name: host.startsWith("reqharbor.acme") ? "Acme ReqHarbor" : host,
      publicUrl: mode === "otherPublicUrl" ? "https://rh.acme.dev" : origin,
      minClientVersion: mode === "tooOld" ? "9.0.0" : null,
      auth: { methods: mode === "legacyOnly" ? [] : methods, signup: "invite", passwordReset: "admin", setupRequired: mode === "setup" },
      features: { accounts: mode !== "legacyOnly", teams: false, legacyWorkspaces: mode === "legacyOnly" ? "open" : "off" },
      sync: { types: ["project", "collection", "folder", "request", "environment"], requestKinds: ["http"] },
    };
    if (mode === "futureApi") (info as ServerInfo & { somethingNew?: unknown }).somethingNew = { x: 1 };
    return info;
  }

  /** The 06 §9 table, as `classify` in server_info.rs. */
  classify(origin: string): ServerDiscovery {
    const mode = this.modeFor(origin);
    const base: ServerDiscovery = {
      serverUrl: origin,
      status: "ok",
      message: null,
      info: null,
      canonicalUrl: null,
      insecure: isInsecure(origin) || mode === "insecure",
      canSignIn: false,
      fetchedAt: this.hooks.now(),
      cached: false,
    };
    if (mode === "unreachable") return { ...base, status: "unreachable", message: `Can't reach ${displayHost(origin)}. Check the address and your connection.` };
    if (mode === "tokenOnly") return { ...base, status: "token_sync_only", message: MSG_TOKEN_ONLY };
    if (mode === "notReqharbor") return { ...base, status: "not_reqharbor", message: MSG_NOT_REQHARBOR };
    const info = this.info(origin, mode);
    const canonical = info.publicUrl && info.publicUrl !== origin ? info.publicUrl : null;
    const d: ServerDiscovery = { ...base, info, canonicalUrl: canonical };
    if (info.minClientVersion) return { ...d, status: "client_too_old", message: `This server needs ReqHarbor ${info.minClientVersion} or newer.` };
    if (!info.features.accounts) return { ...d, status: "token_sync_only", message: info.features.legacyWorkspaces === "off" ? MSG_NO_SYNC : MSG_TOKEN_ONLY };
    if (info.auth.methods.length === 0) return { ...d, message: MSG_NO_METHODS };
    return { ...d, canSignIn: true, message: info.auth.setupRequired ? MSG_SETUP : null };
  }

  async discover(serverUrl: string): Promise<ServerDiscovery> {
    const origin = normalizeServerUrl(serverUrl);
    await this.hooks.delay(150);
    const fresh = this.classify(origin);
    const cached = this.status.server;
    if (fresh.status === "unreachable" && this.status.account?.serverUrl === origin && cached?.info && fresh.fetchedAt - cached.fetchedAt < 24 * 3600_000) {
      return { ...structuredClone(cached), cached: true };
    }
    return fresh;
  }

  private insecureAcked(origin: string): boolean {
    const known = (this.hooks.settings() as Settings & { knownServers?: { url: string; insecureAck?: boolean }[] }).knownServers;
    return !!known?.some((k) => k.url === origin && k.insecureAck);
  }

  private failSignIn(gen: number, code: CommandErrorCode, message: string): void {
    if (gen !== this.generation) return;
    this.status.signIn = { ...this.status.signIn, state: "error", error: code, message, authorizeUrl: null };
    this.emit();
  }

  async signInStart(args: { serverUrl: string; method?: string | null; insecureConfirmed?: boolean | null }): Promise<AuthStatus> {
    const method = args.method || null;
    if (this.status.signIn.state === "exchanging") return fail("invalid", "Sign-in is finishing. Try again in a moment.");
    this.clearTimer();
    const gen = ++this.generation;
    this.status.signIn = { ...idle(), state: "checking", serverUrl: args.serverUrl.trim(), method };
    this.emit();
    let origin: string;
    try {
      origin = normalizeServerUrl(args.serverUrl);
    } catch (e) {
      const err = e as { code: CommandErrorCode; message: string };
      this.failSignIn(gen, err.code, err.message);
      throw e;
    }
    await this.hooks.delay(150);
    const d = this.classify(origin);
    const reject = (code: CommandErrorCode, message: string): never => {
      this.failSignIn(gen, code, message);
      return fail(code, message);
    };
    const codes: Record<string, CommandErrorCode> = { unreachable: "server_unreachable", token_sync_only: "token_sync_only", not_reqharbor: "not_reqharbor", client_too_old: "client_too_old" };
    if (d.status !== "ok") reject(codes[d.status], d.message ?? "");
    if (!d.canSignIn) reject("no_sign_in_methods", d.message ?? MSG_NO_METHODS);
    if (d.insecure && !args.insecureConfirmed && !this.insecureAcked(origin)) reject("insecure_unconfirmed", MSG_INSECURE);
    if (this.opts.auth === "no_port") reject("loopback_unavailable", errorMessages.loopback_unavailable!);
    if (gen !== this.generation) return fail("sign_in_cancelled", "Sign-in was cancelled.");
    const url = `${origin}/auth/authorize?client_id=reqharbor-desktop&redirect_uri=${encodeURIComponent("http://127.0.0.1:53682/cb")}&code_challenge_method=S256${method ? `&method=${encodeURIComponent(method)}` : ""}`;
    const opened = this.opts.auth !== "no_browser";
    if (opened) this.opened.push(url);
    this.status.signIn = { ...idle(), state: "waiting", serverUrl: origin, method, startedAt: this.hooks.now(), ...(opened ? {} : { message: MSG_BROWSER_FAILED, authorizeUrl: url }) };
    this.pending = { gen, origin, method, discovery: d };
    const s = this.emit();
    if (this.opts.authDelay > 0) this.timer = setTimeout(() => void this.finishBrowser(), this.opts.authDelay);
    return s;
  }

  private pending: { gen: number; origin: string; method: string | null; discovery: ServerDiscovery } | null = null;

  private clearTimer() {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
  }

  /** The fake browser finishes (or fails, per `?mockAuth`). */
  async finishBrowser(): Promise<void> {
    this.clearTimer();
    const p = this.pending;
    if (!p || p.gen !== this.generation || this.status.signIn.state !== "waiting") return;
    const mode = this.opts.auth;
    if (mode === "email_in_use") return; // The hosted page shows why; the app keeps waiting.
    if (mode === "timeout") return this.failSignIn(p.gen, "sign_in_timeout", errorMessages.sign_in_timeout!);
    if (mode === "denied") return this.failSignIn(p.gen, "sign_in_denied", errorMessages.sign_in_denied!);
    this.status.signIn = { ...this.status.signIn, state: "exchanging", message: null, authorizeUrl: null };
    this.emit();
    await this.hooks.delay(300);
    if (p.gen !== this.generation) return;
    if (mode === "offline") return this.failSignIn(p.gen, "server_unreachable", errorMessages.server_unreachable!);
    if (mode === "keychain") return this.failSignIn(p.gen, "keychain_unavailable", errorMessages.keychain_unavailable!);
    this.pending = null;
    this.signedIn(p.origin, p.method ?? "github", p.discovery);
    this.status.signIn = { ...this.status.signIn, state: "success", error: null, message: null, authorizeUrl: null };
    this.emit();
    if (this.status.account) this.hooks.onSignedIn?.(this.status.account);
  }

  /** Who the next browser sign-in signs in as (tests: `__mock.accounts.identity = {…}`); default Priya. */
  identity: { userId: string; email: string; name: string } | null = null;

  private signedIn(origin: string, method: string, d: ServerDiscovery) {
    const who = this.identity ?? { userId: "01JUSERPRIYA0000000000000", email: "priya@acme.dev", name: "Priya Shah" };
    const account: Account = {
      serverUrl: origin,
      serverInstanceId: d.info?.instanceId ?? "",
      serverName: d.info?.name || displayHost(origin),
      userId: who.userId,
      email: who.email,
      name: who.name,
      method,
      sessionId: "01JSESSIONTHISDEVICE00000",
    };
    const now = this.hooks.now();
    const day = (ms: number) => Math.floor(ms / 86_400_000) * 86_400_000;
    this.status.account = account;
    this.status.server = d;
    this.status.signInRequired = false;
    this.status.keychainUnreadable = false;
    this.status.offline = false;
    this.sessions = [
      { id: account.sessionId, deviceName: "MacBook Pro (macOS)", createdAt: now - 25 * 86_400_000, lastUsedDay: day(now), current: true },
      { id: "01JSESSIONUBUNTUDESK000000", deviceName: "ubuntu-desk (Linux)", createdAt: now - 44 * 86_400_000, lastUsedDay: day(now - 7 * 86_400_000), current: false },
    ];
  }

  reopen(): null {
    if (!this.pending || this.status.signIn.state !== "waiting") return fail("not_found", "No sign-in is in progress");
    // Same as the Rust side: a browser that still can't be opened keeps the hint.
    if (this.opts.auth !== "no_browser") this.opened.push(this.opened[this.opened.length - 1] ?? "");
    return null;
  }

  cancel(): AuthStatus {
    // Once the code is being exchanged the sign-in finishes as a unit.
    if (this.status.signIn.state === "exchanging") return this.emit();
    this.clearTimer();
    this.generation++;
    this.pending = null;
    this.status.signIn = idle();
    return this.emit();
  }

  async signOut(): Promise<AuthStatus> {
    this.clearTimer();
    this.generation++;
    this.pending = null;
    await this.hooks.delay(100);
    this.status = { ...this.status, account: null, server: null, signInRequired: false, keychainUnreadable: false, offline: false, signIn: idle() };
    this.sessions = [];
    this.hooks.onSignedOut?.();
    return this.emit();
  }

  private requireSession(): void {
    if (!this.status.account || this.status.keychainUnreadable) fail("not_signed_in", "You're not signed in.");
    if (this.status.signInRequired) fail("sign_in_required", "Your session ended. Sign in again to keep syncing.");
  }

  async me(): Promise<AuthStatus> {
    this.requireSession();
    await this.hooks.delay(80);
    return structuredClone(this.status);
  }

  async listSessions(): Promise<SessionInfo[]> {
    this.requireSession();
    await this.hooks.delay(120);
    return structuredClone(this.sessions);
  }

  async revoke(sessionId: string): Promise<null> {
    this.requireSession();
    await this.hooks.delay(80);
    const row = this.sessions.find((s) => s.id === sessionId);
    if (!row) return fail("auth_rejected", "Session not found");
    if (row.current) return fail("invalid", "Use Sign out to sign this device out.");
    this.sessions = this.sessions.filter((s) => s.id !== sessionId);
    return null;
  }

  /** Test hook: the server revoked this device (AC-AUTH-9). */
  expireSession(): void {
    if (!this.status.account) return;
    this.status.signInRequired = true;
    this.emit();
  }
}
