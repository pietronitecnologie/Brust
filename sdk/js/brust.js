// Brust SDK for web apps – a thin ES module around `window.brust`.
//
// Put every use of Brust behind this module so the same app also works in a
// normal browser (where `ready()` resolves to false and the other functions
// throw `Error("Not running inside Brust")`).
//
//   import * as brust from "./brust.js";
//   if (await brust.ready()) {
//     brust.watchClients(() => render());
//     const r = await brust.call("tester", "startTest", { code: "PZ-001" }, { timeoutMs: 120000 });
//   }

const NOT_IN_BRUST = "Not running inside Brust";

function bridge() {
  return typeof window !== "undefined" && window.brust ? window.brust : null;
}

function need() {
  const b = bridge();
  if (!b) throw new Error(NOT_IN_BRUST);
  return b;
}

/** True when `window.brust` is available right now. */
export function inBrust() {
  return bridge() !== null;
}

/**
 * Waits for the bridge. Resolves `true` inside Brust, `false` after `timeoutMs`
 * in a normal browser (or when there is no `window`, e.g. during SSR).
 */
export function ready(timeoutMs = 1500) {
  return new Promise((resolve) => {
    if (typeof window === "undefined") return resolve(false);
    if (inBrust()) return resolve(true);
    let done = false;
    const finish = (v) => {
      if (done) return;
      done = true;
      window.removeEventListener("brust:ready", onReady);
      clearTimeout(timer);
      resolve(v);
    };
    const onReady = () => finish(true);
    window.addEventListener("brust:ready", onReady);
    const timer = setTimeout(() => finish(inBrust()), timeoutMs);
  });
}

/** Static information about the current pane, or null outside Brust. */
export function context() {
  const b = bridge();
  return b ? { version: b.version, appId: b.appId, paneId: b.paneId } : null;
}

/** Runs a client command (e.g. "fs.read", "clipboard.read", "ui.open"). */
export function invoke(cmd, args = {}) {
  try {
    return need().invoke(cmd, args || {});
  } catch (e) {
    return Promise.reject(e);
  }
}

/**
 * Calls `method` on another app or on local software connected to the bridge.
 * `options.timeoutMs` rejects the Promise if no reply arrives in time.
 */
export function call(target, method, params = null, options = {}) {
  try {
    const b = need();
    const ms = options && (options.timeoutMs ?? options.timeout_ms);
    return b.call(target, method, params === undefined ? null : params, ms ? { timeout_ms: ms } : undefined);
  } catch (e) {
    return Promise.reject(e);
  }
}

/**
 * Emits an event. `target`: an app id, "*" (all apps and local software)
 * or null/undefined (local software only).
 */
export function emit(event, data = null, target = null) {
  need().emit(event, data === undefined ? null : data, target === undefined ? null : target);
}

/** Listens for an event ("*" = all). Returns an unsubscribe function. */
export function on(event, fn) {
  return need().on(event, fn);
}

/** Listens for a single occurrence of an event. Returns an unsubscribe function. */
export function once(event, fn) {
  const off = on(event, (data, meta) => {
    off();
    fn(data, meta);
  });
  return off;
}

/**
 * Registers a method callable by other apps / local software.
 * `fn(params, { from })` may return a value or a Promise. Returns an unregister function.
 */
export function handle(method, fn) {
  return need().handle(method, fn);
}

/** Runs a program allowed by `exec_allow` ("exec" permission). */
export function exec(program, args = [], options = {}) {
  return invoke("shell.exec", Object.assign({ program, args }, options || {}));
}

/** System information (os, hostname, username, version, …). */
export function info() {
  return invoke("system.info");
}

/* --------------------------------------------- local software availability */

const connected = new Map(); // name -> number of connections with that name

function addClient(name) {
  if (name) connected.set(name, (connected.get(name) || 0) + 1);
}
function removeClient(name) {
  const n = (connected.get(name) || 0) - 1;
  if (n > 0) connected.set(name, n);
  else connected.delete(name);
}

/**
 * Keeps track of the local programs connected to the bridge ("messaging" permission).
 * `onChange(names: string[])` is called once at start and on every change.
 * Resolves to a stop function (a no-op outside Brust).
 */
export async function watchClients(onChange = () => {}) {
  if (!(await ready())) return () => {};
  const b = need();
  const notify = () => {
    try {
      onChange([...connected.keys()]);
    } catch (e) {
      console.error("[brust]", e);
    }
  };
  // Subscribe first, so no change is lost while the list is loading.
  const offs = [
    b.on("bridge.client_connected", (d) => { addClient(d && d.name); notify(); }),
    b.on("bridge.client_disconnected", (d) => { removeClient(d && d.name); notify(); }),
  ];
  try {
    const list = await b.invoke("bridge.clients");
    connected.clear();
    (list || []).forEach((c) => addClient(c && c.name));
  } catch (e) {
    console.error("[brust] bridge.clients failed:", e && e.message);
  }
  notify();
  return () => offs.forEach((off) => off());
}

/** True if a local program with this name is connected (requires `watchClients`). */
export function isConnected(name) {
  return connected.has(String(name).replace(/^external:/, ""));
}

/* ------------------------------------------------------- client commands */

/** Notes (new in this release). */
export const notes = {
  /** Appends text to a notebook (the default one if omitted). */
  append(text, notebook) {
    const args = { text };
    if (notebook !== undefined && notebook !== null) args.notebook = notebook;
    return invoke("notes.append", args);
  },
};

/** User interface. `ui.badge` is new in this release. */
export const ui = {
  notify(message, title) {
    const args = { message };
    if (title !== undefined && title !== null) args.title = title;
    return invoke("ui.notify", args);
  },
  /** Sets the badge counter of this pane/tab (0 clears it). */
  badge(count) {
    return invoke("ui.badge", { count });
  },
  setTitle(title) {
    return invoke("ui.set_title", { title });
  },
  open(appOrOptions) {
    return invoke("ui.open", typeof appOrOptions === "string" ? { app: appOrOptions } : appOrOptions || {});
  },
  focus() {
    return invoke("ui.focus");
  },
};

/** Printing (new in this release). */
export const print = {
  /** Prints the current page. Options are forwarded as is to "print.page". */
  page(options = {}) {
    return invoke("print.page", options || {});
  },
  /** Lists the available printers ("print.list"). */
  printers() {
    return invoke("print.list");
  },
};

export default {
  ready, inBrust, context, invoke, call, emit, on, once, handle, exec, info,
  watchClients, isConnected, notes, ui, print,
};
