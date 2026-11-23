// Ponte bridge – iniettato in ogni pagina degli applicativi.
// API: window.ponte (vedi README.md).
(function () {
  if (window.ponte) return;
  var CFG = __PONTE_CFG__;
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
        try { fn(data, Object.assign({ event: event }, meta)); } catch (e) { console.error("[ponte]", e); }
      });
    });
    try {
      window.dispatchEvent(new CustomEvent("ponte:" + event, { detail: { data: data, meta: meta } }));
    } catch (e) {}
  }

  window.__ponte_recv = function (m) {
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
        if (!h) { done(false, "metodo non gestito: " + m.method); return; }
        Promise.resolve().then(function () { return h(m.params, { from: m.from }); })
          .then(function (r) { done(true, r === undefined ? null : r); }, function (e) { done(false, e); });
        break;
      }
    }
  };

  // Nessuna navigazione avanti/indietro: tasti Alt+frecce, tasti browser e pulsanti laterali del mouse.
  window.addEventListener("keydown", function (e) {
    var k = e.key;
    if ((e.altKey && (k === "ArrowLeft" || k === "ArrowRight")) || k === "BrowserBack" || k === "BrowserForward") {
      e.preventDefault(); e.stopImmediatePropagation(); return;
    }
    if (k === "F5" || (e.ctrlKey && (k === "r" || k === "R"))) { e.preventDefault(); post({ t: "shortcut", name: "reload_tab" }); return; }
    if (e.ctrlKey && k === "Tab") { e.preventDefault(); post({ t: "shortcut", name: e.shiftKey ? "prev_tab" : "next_tab" }); return; }
    if (e.ctrlKey && !e.shiftKey && (k === "t" || k === "T")) { e.preventDefault(); post({ t: "shortcut", name: "new_tab" }); return; }
    if (e.altKey && k >= "1" && k <= "4") { e.preventDefault(); post({ t: "shortcut", name: "focus_pane", index: +k - 1 }); }
  }, true);
  ["mousedown", "mouseup", "auxclick"].forEach(function (type) {
    window.addEventListener(type, function (e) {
      if (e.button === 3 || e.button === 4) { e.preventDefault(); e.stopImmediatePropagation(); }
    }, true);
  });
  window.addEventListener("pointerdown", function () { post({ t: "focus" }); }, true);
  window.addEventListener("focus", function () { post({ t: "focus" }); });

  window.ponte = Object.freeze({
    version: CFG.version,
    appId: CFG.appId,
    paneId: CFG.paneId,
    /** Esegue un comando del client (capacità locali, UI, messaggistica). */
    invoke: invoke,
    /** Ascolta eventi da altre app o da software esterni ("*" = tutti). */
    on: on,
    off: off,
    /** Invia un evento: target = id app, "*" (tutti) o null (solo software esterni). */
    emit: function (event, data, target) { post({ t: "emit", event: event, data: data === undefined ? null : data, target: target === undefined ? null : target }); },
    /** Registra un metodo invocabile da altre app o software esterni. */
    handle: function (method, fn) { handlers.set(method, fn); return function () { handlers.delete(method); }; },
    /** Invoca un metodo registrato da un'altra app aperta. */
    call: function (target, method, params) { return invoke("bridge.call", { target: target, method: method, params: params === undefined ? null : params }); },
    exec: function (program, args, opts) { return invoke("shell.exec", Object.assign({ program: program, args: args || [] }, opts || {})); },
    open: function (target) { return invoke("shell.open", { target: target }); },
    info: function () { return invoke("system.info"); }
  });
  var ready = function () { window.dispatchEvent(new CustomEvent("ponte:ready", { detail: window.ponte })); };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", ready); else setTimeout(ready, 0);
})();
