// Brust bridge client for local software – Node.js >= 22, no dependencies
// (uses the global WebSocket).
//
//   import { BrustClient } from "./brust-client.mjs";
//   const client = new BrustClient({ name: "tester" });   // token from $BRUST_TOKEN
//   client.method("startTest", async (params, ctx) => {
//     await ctx.emitToCaller("test.progress", { percent: 50 });
//     return { outcome: "OK" };
//   });
//   client.on("order.saved", (data, meta) => console.log(meta.source, data));
//   client.start();
//
// Protocol: see README.md / GUIDE.md of Brust ("Bridge for local software").

export class BrustError extends Error {
  constructor(message) {
    super(message);
    this.name = "BrustError";
  }
}

export class TimeoutError extends Error {
  constructor(message) {
    super(message);
    this.name = "TimeoutError";
  }
}

/** Context passed to method handlers. */
export class CallContext {
  constructor(client, id, method, caller) {
    this.client = client;
    this.id = id;
    this.method = method;
    /** The `from` field: an app id ("crm") or "external:<name>". */
    this.caller = caller;
  }
  /** Sends an event to the app (or program) that made the call. */
  emitToCaller(event, data = null) {
    return this.client.emit(event, data, this.caller || "*");
  }
}

const noopLogger = { info() {}, warn() {}, error() {}, debug() {} };

export class BrustClient {
  /**
   * @param {object} opts
   * @param {string} [opts.token]   bridge token (default: process.env.BRUST_TOKEN)
   * @param {string} [opts.name]    client name, used by apps in brust.call(name, ...)
   * @param {number} [opts.port]    default 47800
   * @param {string} [opts.host]    default "127.0.0.1"
   * @param {boolean} [opts.reconnect] default true
   * @param {number} [opts.minBackoffMs] default 500
   * @param {number} [opts.maxBackoffMs] default 15000
   * @param {Function} [opts.WebSocket] WebSocket implementation (default: globalThis.WebSocket)
   * @param {object} [opts.logger]  console-like logger (default: silent)
   */
  constructor(opts = {}) {
    const token = opts.token || (typeof process !== "undefined" ? process.env.BRUST_TOKEN : undefined);
    if (!token) throw new Error("Brust token missing: pass {token} or set BRUST_TOKEN");
    this.token = token;
    this.name = opts.name || "client";
    this.port = opts.port || 47800;
    this.host = opts.host || "127.0.0.1";
    this.reconnect = opts.reconnect !== false;
    this.minBackoffMs = opts.minBackoffMs ?? 500;
    this.maxBackoffMs = opts.maxBackoffMs ?? 15000;
    this.WebSocket = opts.WebSocket || globalThis.WebSocket;
    if (!this.WebSocket) throw new Error("No global WebSocket: Node.js >= 22 is required");
    this.logger = opts.logger || noopLogger;

    this.welcome = null;
    this._methods = new Map();
    this._listeners = new Map();
    this._connectHooks = [];
    this._pending = new Map();
    this._nextId = 1;
    this._ws = null;
    this._open = false;
    this._stopping = false;
    this._backoff = this.minBackoffMs;
    this._timer = null;
    this._waiters = new Set();
  }

  get url() {
    return `ws://${this.host}:${this.port}/?token=${encodeURIComponent(this.token)}&name=${encodeURIComponent(this.name)}`;
  }

  get connected() {
    return this._open;
  }

  /* ------------------------------------------------------- registration */

  /** Registers `fn(params, ctx)` (sync or async) as method `name`. */
  method(name, fn) {
    this._methods.set(name, fn);
    return this;
  }

  /** Listens for `event` ("*" = all). `fn(data, {event, source})`. Returns an unsubscribe function. */
  on(event, fn) {
    if (!this._listeners.has(event)) this._listeners.set(event, new Set());
    this._listeners.get(event).add(fn);
    return () => this._listeners.get(event)?.delete(fn);
  }

  /** `fn(welcome)` runs after every (re)connection. */
  onConnect(fn) {
    this._connectHooks.push(fn);
    return this;
  }

  /* ----------------------------------------------------------- requests */

  /** Resolves once connected; rejects after `timeoutMs`. */
  waitConnected(timeoutMs = 10000) {
    if (this._open) return Promise.resolve();
    return new Promise((resolve, reject) => {
      const w = {
        resolve: () => { clearTimeout(w.timer); this._waiters.delete(w); resolve(); },
        timer: timeoutMs == null ? null : setTimeout(() => {
          this._waiters.delete(w);
          reject(new Error("not connected to Brust"));
        }, timeoutMs),
      };
      this._waiters.add(w);
    });
  }

  /**
   * Sends an event to an app (`target` = app id) or to all ("*").
   * With `{wait: true}` waits for Brust's acknowledgement.
   */
  async emit(event, data = null, target = "*", { wait = false } = {}) {
    const msg = { type: "emit", target: target ?? "*", event, data: data === undefined ? null : data };
    if (wait) {
      await this._request(msg, 10000);
      return;
    }
    if (!this._open) throw new Error("not connected to Brust");
    this._send({ ...msg, id: this._nextId++ }); // the acknowledgement is ignored
  }

  /** Calls a method registered with `brust.handle` by the app `target`. */
  call(target, method, params = null, { timeoutMs = 30000 } = {}) {
    return this._request({ type: "call", target, method, params: params === undefined ? null : params }, timeoutMs);
  }

  /** Tabs, panes and configured apps. */
  list({ timeoutMs = 10000 } = {}) {
    return this._request({ type: "list" }, timeoutMs);
  }

  /** Opens an app (optionally at a url) or a web address: `open({app})`, `open({url})`, or `open("erp")`. */
  open(arg, { timeoutMs = 10000 } = {}) {
    const o = typeof arg === "string" ? { app: arg } : arg || {};
    if (!o.app && !o.url) return Promise.reject(new Error("specify app or url"));
    const msg = { type: "open" };
    if (o.app) msg.app = o.app;
    if (o.url) msg.url = o.url;
    return this._request(msg, timeoutMs);
  }

  /** Brings an open app to the front. */
  focus(app, { timeoutMs = 10000 } = {}) {
    return this._request({ type: "focus", app }, timeoutMs);
  }

  /** Reloads the active tab. */
  reload({ timeoutMs = 10000 } = {}) {
    return this._request({ type: "reload" }, timeoutMs);
  }

  async _request(msg, timeoutMs) {
    const start = Date.now();
    await this.waitConnected(timeoutMs);
    const id = this._nextId++;
    const remaining = timeoutMs == null ? null : Math.max(0, timeoutMs - (Date.now() - start));
    return new Promise((resolve, reject) => {
      const entry = { resolve, reject, timer: null };
      if (remaining != null) {
        entry.timer = setTimeout(() => {
          this._pending.delete(id);
          reject(new TimeoutError(`timeout: no reply to '${msg.method || msg.type}' in ${timeoutMs} ms`));
        }, remaining);
      }
      this._pending.set(id, entry);
      try {
        this._send({ ...msg, id });
      } catch (e) {
        clearTimeout(entry.timer);
        this._pending.delete(id);
        reject(e);
      }
    });
  }

  _send(msg) {
    // WebSocket.send queues frames in order, so no extra lock is needed in Node.
    if (!this._ws || !this._open) throw new Error("not connected to Brust");
    this._ws.send(JSON.stringify(msg));
  }

  /* ---------------------------------------------------------- lifecycle */

  /** Connects and keeps the connection alive (reconnecting with backoff) until `stop()`. */
  start() {
    this._stopping = false;
    if (!this._ws) this._connect();
    return this;
  }

  /** Closes the connection and stops reconnecting. */
  stop() {
    this._stopping = true;
    clearTimeout(this._timer);
    this._timer = null;
    const ws = this._ws;
    if (ws) {
      try { ws.close(1000); } catch {}
    }
    return new Promise((resolve) => {
      if (!ws || ws.readyState === 3) return resolve();
      ws.addEventListener("close", () => resolve(), { once: true });
      setTimeout(resolve, 2000);
    });
  }

  _connect() {
    let ws;
    try {
      ws = new this.WebSocket(this.url);
    } catch (e) {
      this.logger.warn(`[brust] cannot connect: ${e.message}`);
      this._scheduleReconnect();
      return;
    }
    this._ws = ws;
    // Give up on a connection attempt that hangs.
    const connectTimer = setTimeout(() => down({ code: 0, reason: "connect timeout" }), 10000);
    const down = (ev) => {
      clearTimeout(connectTimer);
      if (this._ws !== ws) return;
      this._ws = null;
      this._open = false;
      try { ws.close(); } catch {}
      for (const [, p] of this._pending) {
        clearTimeout(p.timer);
        p.reject(new Error("connection to Brust lost"));
      }
      this._pending.clear();
      if (!this._stopping) this.logger.warn(`[brust] disconnected (${(ev && ev.code) || ""} ${(ev && ev.reason) || ""})`.trim());
      this._scheduleReconnect();
    };
    ws.addEventListener("open", () => {
      clearTimeout(connectTimer);
      if (this._ws !== ws) return;
      this._open = true;
      this._backoff = this.minBackoffMs;
      this.logger.info(`[brust] connected as '${this.name}'`);
      for (const w of [...this._waiters]) w.resolve();
    });
    ws.addEventListener("message", (ev) => {
      if (this._ws !== ws) return;
      this._onText(typeof ev.data === "string" ? ev.data : Buffer.from(ev.data).toString("utf8"));
    });
    // Node's WebSocket may fire only "error" (no "close") when the connection is refused.
    ws.addEventListener("error", () => down({ code: 0, reason: "error" }));
    ws.addEventListener("close", (ev) => down(ev));
  }

  _scheduleReconnect() {
    if (this._stopping || !this.reconnect) return;
    const delay = Math.round(this._backoff * (0.8 + 0.4 * Math.random()));
    this._backoff = Math.min(this._backoff * 2, this.maxBackoffMs);
    clearTimeout(this._timer);
    this._timer = setTimeout(() => {
      this._timer = null;
      if (!this._stopping) this._connect();
    }, delay);
  }

  /* ----------------------------------------------------------- dispatch */

  _onText(text) {
    let msg;
    try {
      msg = JSON.parse(text);
    } catch {
      this.logger.warn("[brust] invalid JSON received");
      return;
    }
    if (msg && typeof msg === "object") this._dispatch(msg);
  }

  _dispatch(msg) {
    switch (msg.type) {
      case "call":
        this._handleCall(msg);
        break;
      case "result":
      case "pong": {
        const p = this._pending.get(msg.id);
        if (!p) return;
        this._pending.delete(msg.id);
        clearTimeout(p.timer);
        if (msg.type === "pong" || msg.ok !== false) p.resolve(msg.result === undefined ? null : msg.result);
        else p.reject(new BrustError(msg.error || "error"));
        break;
      }
      case "event": {
        const meta = { event: msg.event, source: msg.source };
        for (const name of [msg.event, "*"]) {
          for (const fn of [...(this._listeners.get(name) || [])]) this._safe(fn, msg.data, meta);
        }
        break;
      }
      case "welcome":
        this.welcome = msg;
        for (const fn of this._connectHooks) this._safe(fn, msg);
        break;
      case "error":
        this.logger.warn(`[brust] error: ${msg.error}`);
        break;
    }
  }

  _safe(fn, ...args) {
    try {
      const r = fn(...args);
      if (r && typeof r.then === "function") r.catch((e) => this.logger.error("[brust] listener error:", e));
    } catch (e) {
      this.logger.error("[brust] listener error:", e);
    }
  }

  async _handleCall(msg) {
    const ctx = new CallContext(this, msg.id, msg.method, msg.from || "*");
    let reply;
    try {
      const fn = this._methods.get(msg.method);
      if (!fn) throw new BrustError(`unknown method: ${msg.method}`);
      const result = await fn(msg.params === undefined ? null : msg.params, ctx);
      reply = { type: "reply", id: msg.id, ok: true, result: result === undefined ? null : result };
      JSON.stringify(reply); // fail here (ok:false) if the result is not serializable
    } catch (e) {
      reply = { type: "reply", id: msg.id, ok: false, error: String((e && e.message) || e || "error") };
    }
    try {
      this._send(reply);
    } catch (e) {
      this.logger.warn(`[brust] could not reply to call ${msg.id}: ${e.message}`);
    }
  }
}

export default BrustClient;
