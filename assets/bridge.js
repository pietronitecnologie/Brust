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

  /* ---------- Right-click menu ---------- */
  var menuCustom = [];
  var menuTheme = CFG.theme || "dark";
  var menuHost = null, menuSR = null, menuBox = null, menuOpen = false, menuSel = -1, menuEntries = [];

  window.__brust_theme = function (t) { menuTheme = t === "light" ? "light" : "dark"; if (menuHost) menuHost.setAttribute("data-theme", menuTheme); };

  function txt(el) { return ((el && (el.innerText || el.textContent)) || "").replace(/\s+/g, " ").trim(); }
  function escHtml(s) { return String(s).replace(/[&<>"]/g, function (c) { return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]; }); }
  var CELL = "td,th,[role=cell],[role=gridcell],[role=columnheader],[role=rowheader]";
  var TABLE = "table,[role=table],[role=grid],[role=treegrid]";
  function rowsOf(t) {
    if (t.tagName === "TABLE") return Array.prototype.filter.call(t.rows, function (r) { return r.closest("table") === t; });
    return Array.prototype.filter.call(t.querySelectorAll("[role=row]"), function (r) { return r.closest("[role=table],[role=grid],[role=treegrid]") === t; });
  }
  function cellsOf(r) {
    if (r.tagName === "TR") return Array.prototype.slice.call(r.cells);
    return Array.prototype.filter.call(r.querySelectorAll("[role=cell],[role=gridcell],[role=columnheader],[role=rowheader]"), function (c) { return c.closest("[role=row]") === r; });
  }
  // Grid with colspan/rowspan expanded: each slot holds { el, text, head, origin }.
  function gridOf(t) {
    var grid = [], pos = new Map();
    rowsOf(t).forEach(function (r, ri) {
      grid[ri] = grid[ri] || [];
      var ci = 0;
      cellsOf(r).forEach(function (c) {
        while (grid[ri][ci]) ci++;
        var cs = Math.max(1, Math.min(1000, +c.colSpan || +c.getAttribute("aria-colspan") || 1));
        var rs = Math.max(1, Math.min(1000, +c.rowSpan || +c.getAttribute("aria-rowspan") || 1));
        var head = c.tagName === "TH" || /header/.test(c.getAttribute("role") || "");
        var value = txt(c);
        pos.set(c, { r: ri, c: ci });
        for (var y = 0; y < rs; y++) {
          grid[ri + y] = grid[ri + y] || [];
          for (var x = 0; x < cs; x++) grid[ri + y][ci + x] = { el: c, text: (y === 0 && x === 0) ? value : "", fill: value, head: head, origin: y === 0 && x === 0, cs: cs, rs: rs };
        }
        ci += cs;
      });
    });
    var width = grid.reduce(function (w, r) { return Math.max(w, r.length); }, 0);
    grid = grid.map(function (r) { var o = []; for (var i = 0; i < width; i++) o.push(r[i] || { text: "", fill: "", head: false, origin: true, cs: 1, rs: 1 }); return o; });
    return { grid: grid, pos: pos };
  }
  // `filled`: merged cells repeated in every slot they cover (rows, columns, CSV, Markdown);
  // otherwise only the first slot has the text (whole-table copy, like spreadsheets do).
  function val(c, filled) { return (filled ? c.fill : c.text) || ""; }
  function matrixText(m, filled) { return m.map(function (r) { return r.map(function (c) { return val(c, filled).replace(/\t/g, " "); }).join("\t"); }).join("\r\n"); }
  function matrixHtml(m, spans) {
    return "<table>" + m.map(function (r) {
      return "<tr>" + r.map(function (c) {
        if (spans && !c.origin) return "";
        var tg = c.head ? "th" : "td";
        var at = spans ? (c.cs > 1 ? " colspan='" + c.cs + "'" : "") + (c.rs > 1 ? " rowspan='" + c.rs + "'" : "") : "";
        return "<" + tg + at + ">" + escHtml(spans ? c.text : c.fill || "") + "</" + tg + ">";
      }).join("") + "</tr>";
    }).join("") + "</table>";
  }
  function matrixCsv(m) {
    return m.map(function (r) { return r.map(function (c) { var v = val(c, true); return /[",;\r\n]/.test(v) ? '"' + v.replace(/"/g, '""') + '"' : v; }).join(","); }).join("\r\n");
  }
  function matrixMd(m) {
    if (!m.length) return "";
    var cell = function (c) { return val(c, true).replace(/\|/g, "\\|"); };
    var out = ["| " + m[0].map(cell).join(" | ") + " |", "|" + m[0].map(function () { return " --- "; }).join("|") + "|"];
    m.slice(1).forEach(function (r) { out.push("| " + r.map(cell).join(" | ") + " |"); });
    return out.join("\n");
  }
  function copyData(text, html) {
    var done = false;
    var h = function (e) { e.clipboardData.setData("text/plain", text); if (html) e.clipboardData.setData("text/html", html); e.preventDefault(); done = true; };
    document.addEventListener("copy", h, true);
    try { document.execCommand("copy"); } catch (x) {}
    document.removeEventListener("copy", h, true);
    if (!done) post({ t: "menu", action: "copy", text: text, html: html || "" });
  }
  function flash(msg) {
    ensureMenu();
    var f = menuSR.querySelector(".flash");
    f.textContent = msg; f.classList.add("show");
    clearTimeout(flash.t); flash.t = setTimeout(function () { f.classList.remove("show"); }, 1400);
  }

  function ensureMenu() {
    if (menuHost) return;
    menuHost = document.createElement("brust-menu");
    menuHost.setAttribute("data-theme", menuTheme);
    menuSR = menuHost.attachShadow({ mode: "closed" });
    menuSR.innerHTML = "<style>" +
      ":host{all:initial;position:fixed;inset:0;z-index:2147483647;pointer-events:none;font:13px 'Segoe UI Variable Text','Segoe UI',system-ui,sans-serif;" +
      "--bg:#20242b;--fg:#d9dde4;--mut:#8b93a1;--line:#2e333c;--hi:#263453;--hifg:#e4ecff;--sh:0 14px 40px rgba(0,0,0,.45)}" +
      ":host([data-theme=light]){--bg:#fff;--fg:#1c2230;--mut:#677083;--line:#dde1e8;--hi:#dfe8ff;--hifg:#13306f;--sh:0 14px 40px rgba(20,30,50,.18)}" +
      ".m{position:fixed;min-width:220px;max-width:340px;background:var(--bg);color:var(--fg);border:1px solid var(--line);border-radius:10px;box-shadow:var(--sh);padding:5px;display:none;pointer-events:auto;user-select:none}" +
      ".m.show{display:block}.i{display:flex;align-items:center;gap:9px;padding:6px 9px;border-radius:7px;cursor:default;white-space:nowrap}" +
      ".i .ic{width:18px;text-align:center;opacity:.85}.i .l{flex:1;overflow:hidden;text-overflow:ellipsis}.i .k{color:var(--mut);font-size:11px}" +
      ".i.sel{background:var(--hi);color:var(--hifg)}.i.off{opacity:.45}.s{height:1px;background:var(--line);margin:4px 6px}" +
      ".h{color:var(--mut);font-size:10.5px;letter-spacing:.06em;text-transform:uppercase;padding:6px 9px 3px}" +
      ".flash{position:fixed;left:50%;bottom:22px;transform:translateX(-50%) translateY(8px);background:var(--bg);color:var(--fg);border:1px solid var(--line);border-radius:9px;padding:7px 14px;box-shadow:var(--sh);opacity:0;transition:opacity .15s,transform .15s}" +
      ".flash.show{opacity:1;transform:translateX(-50%)}</style><div class='m' role='menu'></div><div class='flash'></div>";
    menuBox = menuSR.querySelector(".m");
    menuBox.addEventListener("mousedown", function (e) { e.preventDefault(); }); // keep the page selection and focus
    (document.documentElement || document.body).appendChild(menuHost);
  }

  function context(e) {
    var t = e.target && e.target.nodeType === 1 ? e.target : (e.target && e.target.parentElement);
    var sel = String(window.getSelection ? window.getSelection() : "");
    var ed = t && t.closest && t.closest("input,textarea,[contenteditable=''],[contenteditable=true]");
    if (ed && ed.tagName === "INPUT" && !/^(text|search|url|tel|email|password|number|)$/i.test(ed.type || "")) ed = null;
    if (ed && (ed.tagName === "INPUT" || ed.tagName === "TEXTAREA")) {
      try { sel = ed.value.substring(ed.selectionStart, ed.selectionEnd); } catch (x) {}
    }
    var a = t && t.closest && t.closest("a[href]");
    var img = t && t.closest && t.closest("img");
    var cell = !ed && t && t.closest && t.closest(CELL);
    var table = cell && cell.closest(TABLE);
    var ctx = { target: t, selection: sel, editable: ed || null, link: a ? a.href : null, image: img ? (img.currentSrc || img.src) : null, cell: null, table: null };
    if (cell && table) {
      var g = gridOf(table), p = g.pos.get(cell);
      if (p) {
        ctx.cell = { text: txt(cell), row: p.r, column: p.c };
        ctx.table = { rows: g.grid.map(function (r) { return r.map(function (c) { return c.fill || ""; }); }) };
        ctx._grid = g.grid; ctx._pos = p;
      }
    }
    return ctx;
  }

  function buildItems(ctx) {
    var it = [];
    var add = function (icon, label, run, opts) { it.push({ icon: icon, label: label, run: run, key: (opts && opts.key) || "", off: !!(opts && opts.off) }); };
    var sep = function () { if (it.length && it[it.length - 1] !== "-") it.push("-"); };
    var head = function (h) { sep(); it.push({ head: h }); };
    var short = function (s) { s = s.replace(/\s+/g, " ").trim(); return s.length > 28 ? s.slice(0, 27) + "…" : s; };
    if (ctx.editable) {
      add("✂", "Cut", function () { document.execCommand("cut"); }, { key: "Ctrl+X", off: !ctx.selection });
      add("⧉", "Copy", function () { document.execCommand("copy"); }, { key: "Ctrl+C", off: !ctx.selection });
      add("📋", "Paste", function () {
        invoke("clipboard.read").then(function (text) { ctx.editable.focus(); document.execCommand("insertText", false, text || ""); }, function () { flash("Use Ctrl+V to paste"); });
      }, { key: "Ctrl+V", off: !CFG.canPaste });
      add("▤", "Select all", function () { ctx.editable.focus(); if (ctx.editable.select) ctx.editable.select(); else document.execCommand("selectAll"); }, { key: "Ctrl+A" });
    } else if (ctx.selection.trim()) {
      add("⧉", "Copy", function () { document.execCommand("copy"); }, { key: "Ctrl+C" });
      add("🔎", "Search “" + short(ctx.selection) + "”", function () { post({ t: "menu", action: "search", q: ctx.selection }); });
      if (CFG.notes) add("📝", "Save to notes", function () { post({ t: "capture", text: ctx.selection, title: document.title, url: location.href }); flash("Saved to notes"); }, { key: "Ctrl+Shift+S" });
    }
    if (ctx.link) {
      sep();
      add("↗", "Open link in browser", function () { post({ t: "menu", action: "open_external", url: ctx.link }); });
      add("🔗", "Copy link address", function () { copyData(ctx.link); flash("Link copied"); });
    }
    if (ctx.image) {
      sep();
      add("🖼", "Copy image address", function () { copyData(ctx.image); flash("Address copied"); });
      if (/^https?:/.test(ctx.image)) add("↗", "Open image in browser", function () { post({ t: "menu", action: "open_external", url: ctx.image }); });
    }
    if (ctx.cell) {
      var g = ctx._grid, p = ctx._pos;
      var row = [g[p.r]], col = g.map(function (r) { return [r[p.c]]; });
      head("Table");
      add("▦", "Copy table", function () { copyData(matrixText(g, false), matrixHtml(g, true)); flash("Table copied (" + g.length + " rows)"); });
      add("▭", "Copy row", function () { copyData(matrixText(row, true), matrixHtml(row, false)); flash("Row copied"); });
      add("▯", "Copy column", function () { copyData(matrixText(col, true), matrixHtml(col, false)); flash("Column copied (" + col.length + " cells)"); });
      add("·", "Copy cell", function () { copyData(ctx.cell.text); flash("Cell copied"); });
      add("⇣", "Copy table as CSV", function () { copyData(matrixCsv(g)); flash("Table copied as CSV"); });
      add("M", "Copy table as Markdown", function () { copyData(matrixMd(g)); flash("Table copied as Markdown"); });
      if (CFG.notes) add("📝", "Save table to notes", function () { post({ t: "capture", text: matrixMd(g), title: document.title, url: location.href }); flash("Table saved to notes"); });
    }
    var custom = menuCustom.filter(function (c) {
      var w = c.when || "always";
      if (typeof w === "function") { try { return !!w(publicCtx(ctx)); } catch (x) { return false; } }
      return w === "always" || (w === "selection" && ctx.selection.trim()) || (w === "link" && ctx.link) || (w === "table" && ctx.cell) || (w === "editable" && ctx.editable) || (w === "image" && ctx.image);
    });
    if (custom.length) {
      sep();
      custom.forEach(function (c) { add(c.icon || "•", c.label, function () { try { c.run(publicCtx(ctx)); } catch (x) { console.error("[brust menu]", x); } }); });
    }
    sep();
    add("⟳", "Reload", function () { location.reload(); }, { key: "F5" });
    add("🖨", "Print…", function () { window.print(); });
    add("⧉", "Open in a separate window", function () { post({ t: "menu", action: "detach" }); });
    if (CFG.devtools) add("</>", "Inspect", function () { post({ t: "menu", action: "devtools" }); });
    if (it[it.length - 1] === "-") it.pop();
    return it;
  }
  function publicCtx(ctx) { return { selection: ctx.selection, link: ctx.link, image: ctx.image, cell: ctx.cell, table: ctx.table, target: ctx.target }; }

  function renderMenu() {
    menuBox.innerHTML = "";
    menuEntries.forEach(function (x, i) {
      var d = document.createElement("div");
      if (x === "-") { d.className = "s"; }
      else if (x.head) { d.className = "h"; d.textContent = x.head; }
      else {
        d.className = "i" + (x.off ? " off" : "") + (i === menuSel ? " sel" : "");
        d.setAttribute("role", "menuitem");
        d.innerHTML = "<span class='ic'></span><span class='l'></span><span class='k'></span>";
        d.children[0].textContent = x.icon; d.children[1].textContent = x.label; d.children[2].textContent = x.key;
        d.addEventListener("mousemove", function () { if (menuSel !== i) { menuSel = i; renderMenu(); } });
        d.addEventListener("click", function () { runEntry(i); });
      }
      menuBox.appendChild(d);
    });
  }
  function runEntry(i) {
    var x = menuEntries[i];
    if (!x || x === "-" || x.head || x.off) return;
    hideMenu();
    x.run();
  }
  function showMenu(x, y, ctx) {
    ensureMenu();
    menuHost.setAttribute("data-theme", menuTheme);
    menuEntries = buildItems(ctx);
    menuSel = -1; renderMenu();
    menuBox.classList.add("show"); menuOpen = true;
    var w = menuBox.offsetWidth, h = menuBox.offsetHeight, vw = window.innerWidth, vh = window.innerHeight;
    menuBox.style.left = Math.max(4, Math.min(x, vw - w - 4)) + "px";
    menuBox.style.top = Math.max(4, (y + h > vh - 4 ? Math.max(4, y - h) : y)) + "px";
  }
  function hideMenu() { if (menuOpen) { menuBox.classList.remove("show"); menuOpen = false; } }
  function moveSel(d) {
    var n = menuEntries.length, i = menuSel;
    for (var k = 0; k < n; k++) { i = (i + d + n) % n; var x = menuEntries[i]; if (x !== "-" && !x.head && !x.off) { menuSel = i; renderMenu(); return; } }
  }

  window.addEventListener("contextmenu", function (e) {
    // The app's own menu (it called preventDefault) and Shift+right-click keep the browser behaviour.
    if (!CFG.menu || e.defaultPrevented || e.shiftKey) return;
    e.preventDefault();
    var x = e.clientX, y = e.clientY;
    if (!x && !y && e.target && e.target.getBoundingClientRect) { var r = e.target.getBoundingClientRect(); x = r.left + 8; y = r.top + 8; }
    showMenu(x, y, context(e));
  });
  window.addEventListener("mousedown", function (e) { if (menuOpen && e.target !== menuHost) hideMenu(); }, true);
  window.addEventListener("blur", hideMenu);
  window.addEventListener("resize", hideMenu);
  window.addEventListener("scroll", hideMenu, true);
  window.addEventListener("keydown", function (e) {
    if (!menuOpen) return;
    if (e.key === "Escape") { hideMenu(); }
    else if (e.key === "ArrowDown") { moveSel(1); }
    else if (e.key === "ArrowUp") { moveSel(-1); }
    else if (e.key === "Enter") { runEntry(menuSel); }
    else return;
    e.preventDefault(); e.stopImmediatePropagation();
  }, true);

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
    info: function () { return invoke("system.info"); },
    /** Right-click menu: add an item. item = { label, icon?, when?: "always"|"selection"|"link"|"image"|"table"|"editable"|fn(ctx), run(ctx) }. Returns a remove function. */
    menu: Object.freeze({
      add: function (item) { menuCustom.push(item); return function () { var i = menuCustom.indexOf(item); if (i >= 0) menuCustom.splice(i, 1); }; },
      clear: function () { menuCustom.length = 0; }
    })
  });
  var ready = function () { window.dispatchEvent(new CustomEvent("brust:ready", { detail: window.brust })); };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", ready); else setTimeout(ready, 0);
})();
