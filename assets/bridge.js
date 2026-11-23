// Brust bridge – injected into every app page.
// API: window.brust (see README.md).
(function () {
  if (window.brust) return;
  var CFG = __BRUST_CFG__;
  var ipc = window.ipc;
  if (!ipc) return;
  var seq = 0;
  var pending = new Map();
  var listeners = new Map();
  var handlers = new Map();

  function post(msg) { ipc.postMessage(JSON.stringify(msg)); }

  function invoke(cmd, args) {
    return new Promise(function (resolve, reject) {
      var id = ++seq;
      pending.set(id, { resolve: resolve, reject: reject });
      post({ t: "invoke", id: id, cmd: cmd, args: args || {} });
    });
  }

  function on(event, fn) {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event).add(fn);
    return function () { off(event, fn); };
  }
  function off(event, fn) { var s = listeners.get(event); if (s) s.delete(fn); }

  function dispatch(event, data, meta) {
    [event, "*"].forEach(function (name) {
      var s = listeners.get(name);
      if (!s) return;
      s.forEach(function (fn) {
        try { fn(data, Object.assign({ event: event }, meta)); } catch (e) { console.error("[brust]", e); }
      });
    });
    try {
      window.dispatchEvent(new CustomEvent("brust:" + event, { detail: { data: data, meta: meta } }));
    } catch (e) {}
  }

  window.__brust_recv = function (m) {
    switch (m.t) {
      case "reply": {
        var p = pending.get(m.id);
        if (!p) return;
        pending.delete(m.id);
        if (m.ok) p.resolve(m.result); else p.reject(new Error(m.error));
        break;
      }
      case "event":
        dispatch(m.event, m.data, { source: m.source });
        break;
      case "call": {
        var h = handlers.get(m.method);
        var done = function (ok, val) {
          post({ t: "call_result", id: m.id, ok: ok, result: ok ? val : undefined, error: ok ? undefined : String(val && val.message || val) });
        };
        if (!h) { done(false, "unhandled method: " + m.method); return; }
        Promise.resolve().then(function () { return h(m.params, { from: m.from }); })
          .then(function (r) { done(true, r === undefined ? null : r); }, function (e) { done(false, e); });
        break;
      }
    }
  };

  // No back/forward navigation: Alt+arrow keys, browser keys and mouse side buttons.
  window.addEventListener("keydown", function (e) {
    var k = e.key;
    if ((e.altKey && (k === "ArrowLeft" || k === "ArrowRight")) || k === "BrowserBack" || k === "BrowserForward") {
      e.preventDefault(); e.stopImmediatePropagation(); return;
    }
    if (k === "F5" || (e.ctrlKey && (k === "r" || k === "R"))) { e.preventDefault(); post({ t: "shortcut", name: "reload_tab" }); return; }
    if (e.ctrlKey && k === "Tab") { e.preventDefault(); post({ t: "shortcut", name: e.shiftKey ? "prev_tab" : "next_tab" }); return; }
    if (e.ctrlKey && !e.shiftKey && (k === "t" || k === "T")) { e.preventDefault(); post({ t: "shortcut", name: "new_tab" }); return; }
    if (e.altKey && k >= "1" && k <= "4") { e.preventDefault(); post({ t: "shortcut", name: "focus_pane", index: +k - 1 }); }
    if (e.ctrlKey && !e.shiftKey && !e.altKey && (k === "k" || k === "K")) { e.preventDefault(); post({ t: "shortcut", name: "palette" }); return; }
    if (e.ctrlKey && e.altKey && e.shiftKey && (k === "K" || k === "k")) { e.preventDefault(); post({ t: "shortcut", name: "kiosk_exit" }); return; }
    if (e.ctrlKey && e.shiftKey && (k === "L" || k === "l")) { e.preventDefault(); post({ t: "shortcut", name: "lock" }); return; }
    if (e.ctrlKey && e.shiftKey && (k === "S" || k === "s")) {
      // Quick capture: selected text -> notes.
      e.preventDefault();
      var sel = String(window.getSelection ? window.getSelection() : "");
      post({ t: "capture", text: sel, title: document.title, url: location.href });
    }
  }, true);
  ["mousedown", "mouseup", "auxclick"].forEach(function (type) {
    window.addEventListener(type, function (e) {
      if (e.button === 3 || e.button === 4) { e.preventDefault(); e.stopImmediatePropagation(); }
    }, true);
  });
  window.addEventListener("pointerdown", function () { post({ t: "focus" }); }, true);
  // Activity pings (at most every 20 s) for the idle auto-lock.
  var lastPing = 0;
  ["keydown", "pointerdown", "wheel"].forEach(function (type) {
    window.addEventListener(type, function () {
      var now = Date.now();
      if (now - lastPing > 20000) { lastPing = now; post({ t: "activity" }); }
    }, { capture: true, passive: true });
  });
  window.addEventListener("focus", function () { post({ t: "focus" }); });

  window.brust = Object.freeze({
    version: CFG.version,
    appId: CFG.appId,
    paneId: CFG.paneId,
    /** Runs a client command (local capabilities, UI, messaging). */
    invoke: invoke,
    /** Listens for events from other apps or external software ("*" = all). */
    on: on,
    off: off,
    /** Sends an event: target = app id, "*" (all) or null (external software only). */
    emit: function (event, data, target) { post({ t: "emit", event: event, data: data === undefined ? null : data, target: target === undefined ? null : target }); },
    /** Registers a method callable by other apps or external software. */
    handle: function (method, fn) { handlers.set(method, fn); return function () { handlers.delete(method); }; },
    /** Calls a method of another open app or of local software connected to the bridge.
        opts.timeout_ms: rejects the Promise if no reply arrives in time. */
    call: function (target, method, params, opts) {
      var p = invoke("bridge.call", { target: target, method: method, params: params === undefined ? null : params });
      var ms = opts && opts.timeout_ms;
      if (!ms) return p;
      return Promise.race([p, new Promise(function (_, rej) { setTimeout(function () { rej(new Error("timeout: no reply from " + target + " in " + ms + " ms")); }, ms); })]);
    },
    exec: function (program, args, opts) { return invoke("shell.exec", Object.assign({ program: program, args: args || [] }, opts || {})); },
    open: function (target) { return invoke("shell.open", { target: target }); },
    info: function () { return invoke("system.info"); }
  });
  var ready = function () { window.dispatchEvent(new CustomEvent("brust:ready", { detail: window.brust })); };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", ready); else setTimeout(ready, 0);
})();
