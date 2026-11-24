# Guide: designing web apps and local software for Brust

This guide explains how to build web applications and local programs that use Brust as a bridge
between the browser and the PC. For the full list of commands see `README.md`.

## 1. The three actors and the three message types

```
 ┌──────────────┐   window.brust    ┌──────────────┐   WebSocket 127.0.0.1   ┌──────────────────┐
 │   Web app    │ ◄───────────────► │    Brust     │ ◄─────────────────────► │  Local software  │
 │ (CRM, …)    │    (injected)     │   (client)   │  token + client name    │ (tester, driver) │
 └──────────────┘                   └──────────────┘                         └──────────────────┘
        app id: "crm"                                                         name: "tester"
```

Every participant has a **name**:

* web apps have the `id` defined in `config.json` (e.g. `crm`);
* local programs have the name they connect with (`?name=tester`).

Brust routes messages based on that name. There are three message types:

| Type | What it is for | Web app | Local software |
|---|---|---|---|
| **Local command** (`invoke`) | Brust itself performs an action on the PC: processes, files, serial, clipboard, notes, printing | `brust.invoke("fs.read", …)` | — |
| **Call** (`call`) | Request → **one** reply (or error). For "do this and tell me how it went" | `brust.call(name, method, params)` · `brust.handle(method, fn)` | receives `{"type":"call"}` → replies `{"type":"reply"}`; sends `{"type":"call"}` → receives `{"type":"result"}` |
| **Event** (`emit`/`on`) | Notification with no reply: progress, state changes, alarms | `brust.emit(event, data, target)` · `brust.on(event, fn)` | sends `{"type":"emit"}` · receives `{"type":"event"}` |

A call to a name is delivered to the open web app with that `id` (preferring the active tab).
If no app has that `id`, it goes to the local program connected with that name. The
`external:` prefix (e.g. `"external:tester"`) forces the local program.

## 2. Full example: starting a test

```
Web app (crm)                     Brust                          Local software (tester)
     │ brust.call("tester","startTest",{code})                              │
     │──────────────────────────────►│ {"type":"call","id":7,"method":"startTest",
     │                               │  "params":{…},"from":"crm"} ───────►│
     │                               │                                      │ runs the test
     │                               │◄── {"type":"emit","target":"crm",   │
     │◄── brust.on("test.progress")  │     "event":"test.progress",…}       │ (0..n times)
     │                               │                                      │
     │                               │◄── {"type":"reply","id":7,"ok":true, │
     │◄── the Promise resolves       │     "result":{"outcome":"OK",…}}     │
     │    with the outcome           │                                      │
```

**Web app**

```js
brust.on("test.progress", d => showProgress(d.percent));
try {
  const result = await brust.call("tester", "startTest", { code: "PZ-001" }, { timeout_ms: 120000 });
  showResult(result);                 // { code, outcome: "OK", measurements: [...] }
} catch (e) {
  showError(e.message);               // software not connected, test error, timeout, disconnection
}
```

**Local software** (Python, full version in `examples/tester.py`)

```python
async for raw in ws:
    msg = json.loads(raw)
    if msg["type"] == "call" and msg["method"] == "startTest":
        # ... work, with progress events sent to msg["from"] ...
        await ws.send(json.dumps({"type": "emit", "target": msg["from"],
                                  "event": "test.progress", "data": {"percent": 50}}))
        await ws.send(json.dumps({"type": "reply", "id": msg["id"], "ok": True,
                                  "result": {"outcome": "OK"}}))
```

This is where the "derived command" is built. Brust forwards the data as is; the translation
(e.g. from `{code}` to the instrument's protocol) belongs **in the local software**, which knows
the hardware. Alternatively, the web app transforms the data before calling.

If instead the local software needs to start something in the web app, the flow is mirrored:
the web app registers `brust.handle("openOrder", fn)` and the software sends
`{"type":"call","id":1,"target":"crm","method":"openOrder","params":{…}}`, receiving
`{"type":"result","id":1,…}`.

## 3. Structuring the web app

### 3.1 A single integration module

All code that uses `window.brust` goes into **a single module** (e.g. `src/integration/brust.js`).
The rest of the app calls domain functions such as `startTest()`, not `brust.call` directly.
That way the same app works both in Brust and in a normal browser.

```js
// integration/brust.js
const inBrust = () => typeof window !== "undefined" && !!window.brust;

export function ready() {
  return new Promise(res => {
    if (inBrust()) return res(true);
    window.addEventListener("brust:ready", () => res(true), { once: true });
    setTimeout(() => res(inBrust()), 1500);     // in a normal browser: false
  });
}

// State of the local programs (to enable/disable buttons)
const connected = new Set();
export async function startMonitor(onChange) {
  if (!(await ready())) return;
  (await brust.invoke("bridge.clients")).forEach(c => connected.add(c.name));
  brust.on("bridge.client_connected",    d => { connected.add(d.name);    onChange(); });
  brust.on("bridge.client_disconnected", d => { connected.delete(d.name); onChange(); });
  onChange();
}
export const isAvailable = name => connected.has(name);

// Domain API
export async function startTest(code, onProgress) {
  if (!inBrust()) throw new Error("Only available in the Brust client");
  const stop = brust.on("test.progress", d => d.code === code && onProgress(d));
  try {
    return await brust.call("tester", "startTest", { v: 1, code }, { timeout_ms: 120000 });
  } finally { stop(); }
}
```

### 3.2 Practical rules

1. **The server stays the source of truth.** Brust carries commands; it does not store business data.
   When a test outcome arrives, the web app sends it to its own backend like any other data.
2. **State-aware interface.** Disable "Start test" if `isAvailable("tester")` is
   false, show progress, and always handle the Promise's error.
3. **Always set a timeout** on calls to hardware (`timeout_ms`). If the software disconnects,
   Brust rejects the call immediately; the timeout covers software that hangs.
4. **Correlation identifiers.** Include an id from your domain in the parameters (part code,
   order id) and filter events by it, as in the example: several tabs may be open.
5. **Minimal permissions** in `config.json`. `messaging` is often enough. `exec`/`fs` only if needed,
   with tight `exec_allow` and `fs_roots`. `notes` and `print` are separate permissions.
6. **Expect the confirmation dialog.** `shell.exec`, `shell.open`, `fs.write`, `fs.remove`, `fs.mkdir`,
   `serial.open` and `print.file` wait until the user answers (unless `privacy.confirm_sensitive` /
   the app's `confirm` is `false`, or the user chose "Always allow"). A refusal rejects the Promise
   with `denied by the user`: show it as a normal, recoverable error. Don't put a short
   `timeout_ms` on these calls, and group writes (one `fs.write` of a whole file instead of many
   small ones) so the user is not asked repeatedly.
7. **Incoming commands with `brust.handle`.** Always validate the parameters: they come from another
   program.
8. **Existing apps that cannot be modified** are adapted with `inject_script`. It is a local JS
   file injected into the pages, which can read the DOM, add buttons and call `brust`.

### 3.3 When to use what

| Need | Recommended solution |
|---|---|
| Run a command-line program and read its output | `brust.exec(...)` (`exec` permission). Have the program print JSON to stdout. |
| Talk to a simple serial instrument | `serial.open/write` + `serial.data` event |
| Long-running, stateful operations, complex hardware, proprietary SDKs | **local software** connected to the bridge (`call` + events) |
| Two web apps working together (CRM ↔ ERP) | `brust.handle` in one, `brust.call` in the other |
| Notify several recipients | `brust.emit(event, data, "*")` |
| Read/write exchange files with other programs | `fs.*` (`fs` permission + `fs_roots`) |
| Keep a log or a note the operator can read and edit | `notes.append` (`notes` permission) |
| Print a label, a report or the current page | `print.page`, `print.file` (`print` permission) |
| Show that something needs attention | `ui.badge` + `ui.notify` with `native: true` |

### 3.4 Your own items in the right-click menu

Brust shows its own right-click menu in app panes (copy table/row/column, notes, search…).
An app can add items that fit its domain; `when` decides where they appear:

```js
const remove = brust.menu.add({
  label: "Open order",
  icon: "📦",
  when: (ctx) => ctx.cell && /^PZ-\d+$/.test(ctx.cell.text),   // or "always" | "selection" | "link" | "image" | "table" | "editable"
  run: (ctx) => openOrder(ctx.cell.text),
});
// ctx = { selection, link, image, cell: { text, row, column } | null, table: { rows: string[][] } | null, target }
remove();   // take the item away again
```

If the app already has its own right-click menu (its `contextmenu` handler calls
`preventDefault()`), Brust leaves it alone. The whole menu can be turned off per app
(Settings → Apps → *Brust right-click menu*, or `"context_menu": false`).

### 3.5 Notes, badges, notifications and printing

```js
// Append to a notebook (Markdown, "notes" permission). The default notebook is "Inbox".
await brust.invoke("notes.append", { notebook: "Test log", text: `- ${new Date().toISOString()} PZ-001: OK` });
const nbs = await brust.invoke("notes.list");                 // [{name, modified, size}]
const text = await brust.invoke("notes.read", { notebook: "Test log" });

// Counter on the pane header/tab and in the window title; 0 removes it.
await brust.invoke("ui.badge", { count: pendingOrders.length });
// Toast in Brust; native desktop notification if the window is not focused or native: true.
await brust.invoke("ui.notify", { title: "CRM", message: "New order 12", native: true });

// Printing ("print" permission)
const printers = await brust.invoke("print.list");            // [{name, default}]
await brust.invoke("print.page", { printer: "Office", silent: true });
await brust.invoke("print.file", { path: "C:\\Exchange\\CRM\\label.pdf", printer: "Zebra" });
```

* Notebook names are file names (`<name>.md` in the notes folder): avoid `/ \ : * ? " < > |`, a leading
  dot and names longer than 80 characters. The text is appended as is (a final newline is added if missing),
  so write complete Markdown lines. The notes pane showing that notebook updates immediately.
* `print.page` prints the page as rendered: use a print stylesheet (`@media print`) to hide the
  app's chrome. Silent printing works on Windows; on Linux/macOS the dialog opens, so don't assume
  the job is finished when the Promise resolves. Without `printer`, `default_printer` from
  `config.json` (or the system default) is used.
* `print.file` is sensitive (confirmation) and the file must be inside `fs_roots`; it is printed by
  the application associated with the file type (Windows) or by `lp` (CUPS). Generate the file
  (e.g. a PDF) with `fs.write` first, or let local software produce it.
* With the JS SDK (`sdk/js/brust.js`) the same calls are `notes.append(text, notebook)`,
  `ui.badge(count)`, `ui.notify(message, title)`, `print.printers()`, `print.page(opts)`.

### 3.6 Profiles, logins and private pages

* Each app has its **own browser profile** by default (`isolated: true`): cookies and storage are
  not shared between apps. With a common SSO the user logs in once per app; if the apps must share
  the login, set `"isolated": false` on them (they then share one profile).
* `incognito: true` and `clear_on_exit: true` mean the app often starts logged out: keep the login
  flow fast and don't rely on `localStorage` surviving a restart.
* Camera, microphone, location and screen capture are denied unless the app lists them in
  `web_permissions` (e.g. `{"camera": "allow"}` for a barcode scanner using the webcam).
* With tracker blocking on, third-party scripts on the blocked lists fail to load: test the app
  with `privacy.block_trackers: true`, and add the app's own CDNs to `privacy.allow_domains` if needed.

## 4. Structuring the local software

### 4.1 Recommended structure

```
local-software/
  main            bridge connection, reconnection, message dispatch
  methods/        one function per method: startTest, stopTest, status, …
  hardware/       drivers, protocols, SDKs: no dependency on Brust
  config          port, token, client name
```

* **The name is the contract.** Apps call `brust.call("tester", …)`. Pick a stable,
  configurable name, and don't connect two instances with the same name (the last one connected wins).
* **Automatic reconnection.** Brust may be restarted: retry every few seconds, as in
  `examples/tester.py`. The software can start before Brust (e.g. as a service or at startup).
* **Method table.** `{"startTest": fn, "status": fn, …}`: an unknown method replies
  with an error rather than ignoring the request.
* **Always reply, exactly once**, with `{"type":"reply","id":<received id>,"ok":…}`.
  Exceptions become `ok:false` with a message the operator can read.
* **Long work in parallel.** Run the method in a separate task or thread, so the software
  keeps receiving messages (e.g. `stopTest`).
* **Explicit concurrency.** If the instrument handles one test at a time, reply immediately with
  `ok:false, error:"instrument busy"`, instead of silently queueing requests.
* **Progress via events** sent to `msg["from"]` (the calling app), with the same domain id.
* **Versioning.** Include `v` in the parameters (`{"v":1,…}`) and reject unknown versions.
  Add fields without breaking existing ones.
* **Keep the token out of the code.** Read it from a configuration file or an environment variable.
  The token is in Brust → ⚙ Settings. It is generated and stored encrypted (`bridge-token.dat`)
  unless you set a fixed one in `config.json` → `bridge.token` (useful when it is distributed to many PCs).

### 4.2 Message reference, software side

Connection: `ws://127.0.0.1:47800/?token=TOKEN&name=NAME`

| Received by the software | Meaning |
|---|---|
| `{"type":"welcome","client":3,"version":"0.1.0","apps":[…]}` | connection accepted |
| `{"type":"call","id":7,"method":"…","params":{…},"from":"crm"}` | an app is asking for something: answer with `reply` |
| `{"type":"event","event":"…","data":{…},"source":"crm"}` | event emitted by an app to external software or `"*"` |
| `{"type":"result","id":…,"ok":…,"result"/"error":…}` | reply to one of your `call`, `list`, `emit`, `open`, … |
| `{"type":"pong","id":…}` | reply to `ping` |

| Sent by the software | Meaning |
|---|---|
| `{"type":"reply","id":7,"ok":true,"result":{…}}` | reply to a received `call` (`ok:false,"error":"…"` for errors) |
| `{"type":"emit","target":"crm","event":"…","data":{…}}` | event to an app (`"*"` = all) |
| `{"type":"call","id":1,"target":"crm","method":"…","params":{…}}` | calls a method an app registered with `brust.handle` |
| `{"type":"open","app":"erp"}`, `{"type":"focus","app":"crm"}` | opens an app or brings it to the front |
| `{"type":"list","id":2}` | tabs, panes and configured apps |
| `{"type":"open_template","id":3,"name":"Testing"}` | opens a template in a new tab |
| `{"type":"notes_append","id":4,"notebook":"Test log","text":"…"}` | appends to a notebook (default `Inbox`); recorded in the activity log |
| `{"type":"cli","id":5,"args":["--open","crm"]}` | same as the command line (`--open`, `--url`, `--template`, `--notes`, `brust://…`) and brings Brust to the front |
| `{"type":"reload"}`, `{"type":"ping","id":6}` | reloads the active tab; connection check |

`notes_append` is useful for a readable trace of what the software did (e.g. one line per test),
which the operator can find in the notes without opening a log file.
### 4.3 SDKs and other languages

`sdk/` contains ready-made clients (see `sdk/README.md`):

| SDK | For | Status |
|---|---|---|
| `sdk/js/brust.js` (+ `brust.d.ts`) | web apps: wraps `window.brust`, works outside Brust too | smoke-tested |
| `sdk/python/brust_client.py` | local software (`websockets`) | tested |
| `sdk/node/brust-client.mjs` | local software (Node ≥ 22, no dependencies) | tested |
| `sdk/csharp/BrustClient.cs` | local software (.NET 6+) | **not compiled** |

They implement the rules of 4.1: method table, exactly one reply, handlers in parallel,
reconnection with backoff, the token from `BRUST_TOKEN`. Prefer them to a hand-written client.

Any language with a WebSocket client will do. The pattern is always the same: connect,
read in a loop, dispatch on `type`, answer with `reply`. The minimal C#/.NET skeleton below
shows the pattern and **has not been compiled** (the full client is `sdk/csharp/BrustClient.cs`).

```csharp
using System.Net.WebSockets; using System.Text; using System.Text.Json; using System.Text.Json.Nodes;

var ws = new ClientWebSocket();
await ws.ConnectAsync(new Uri($"ws://127.0.0.1:47800/?token={token}&name=tester"), default);
var buf = new byte[65536];
while (ws.State == WebSocketState.Open) {
    var r = await ws.ReceiveAsync(buf, default);           // messages > 64 KB: accumulate until EndOfMessage
    var msg = JsonNode.Parse(Encoding.UTF8.GetString(buf, 0, r.Count))!;
    if ((string?)msg["type"] != "call") continue;
    _ = Task.Run(async () => {
        JsonObject reply;
        try {
            var result = await Execute((string)msg["method"]!, msg["params"], (string?)msg["from"]);
            reply = new() { ["type"] = "reply", ["id"] = msg["id"]!.GetValue<long>(), ["ok"] = true, ["result"] = result };
        } catch (Exception e) {
            reply = new() { ["type"] = "reply", ["id"] = msg["id"]!.GetValue<long>(), ["ok"] = false, ["error"] = e.Message };
        }
        await Send(ws, reply);    // guard SendAsync with a lock: only one send at a time
    });
}
```

A working PowerShell client-side example is in `examples/external_client.ps1`.

## 5. Deploying to many PCs

### 5.1 Links and command line

Other programs can open Brust where it is needed: `brust --open crm --url https://…/orders/12`,
`brust --template "Line 1"`, or a link `brust://open?app=crm&url=https%3A%2F%2F…` (URL-encode the
values; `brust://` is registered for the current user on Windows when `register_protocol` is `true`).
If Brust is already running, the request is forwarded to it (via the WebSocket bridge, which must be
enabled) instead of starting a second instance. Local software that is already connected can send
`{"type":"cli","args":[…]}` instead.

### 5.2 Signed central configuration

1. Once, on an admin PC: `brust --keygen C:\BrustKeys`. Keep `brust-private.key` secret
   (outside the share, ideally offline); `brust-public.key` goes into every client.
2. Write the central part of the configuration, e.g. `central.json`:

   ```json
   {
     "apps": [
       { "id": "crm", "name": "CRM", "url": "https://crm.mydomain.com", "permissions": ["messaging", "notes", "print"] }
     ],
     "privacy": { "block_trackers": true, "confirm_sensitive": true, "audit_log": true, "lock_after_minutes": 15 },
     "default_printer": "Office"
   }
   ```

3. Sign it: `brust --sign central.json --key C:\BrustKeys\brust-private.key --out \\server\brust\config.signed.json`.
   Check it with `brust --verify \\server\brust\config.signed.json --pubkey C:\BrustKeys\brust-public.key`.
4. On each client, `config.json` only needs the pointer (plus any local keys):

   ```json
   { "remote_config": { "url": "\\\\server\\brust\\config.signed.json", "public_key": "<content of brust-public.key>", "refresh_minutes": 60 } }
   ```

Rules: every **top-level key** in the central file replaces the local one entirely (a central
`"apps"` replaces all local apps; keys absent from the central file stay local); `remote_config`
cannot be changed centrally. A file with a wrong signature is ignored and the last verified copy
(cached in `remote-config.cache.json`) stays in use, also when the server is unreachable. Changes
to `bridge` apply at the next launch; open panes keep their settings until reopened. The `url` can
be `https://…`, `file://…` or a local/UNC path.

### 5.3 Signed updates

1. Build or obtain the new `brust.exe` and compute its hash: `brust --hash-file brust.exe`
   (or `Get-FileHash -Algorithm SHA256 brust.exe`).
2. Publish the executable and write `manifest.json`:

   ```json
   { "version": "0.2.0", "url": "https://intranet.example.com/brust/0.2.0/brust.exe", "sha256": "<hex>", "notes": "Fixes printing on line 2" }
   ```

3. Sign it: `brust --sign manifest.json --key brust-private.key --out manifest.signed.json` and
   publish it at the `manifest_url` of the clients:

   ```json
   { "update": { "manifest_url": "https://intranet.example.com/brust/manifest.signed.json", "public_key": "<base64>", "check_on_start": true } }
   ```

The client installs only if `version` is greater than its own (numeric comparison, a leading `v` is
allowed) and the downloaded file matches `sha256`. The user confirms with Settings → *Install … and
restart*; the old executable stays next to it as `brust.exe.old` until the next start (if the
replacement fails, the old one is put back). The `update` section can itself be distributed with the central
configuration. The update replaces only the executable.

On Windows these commands print into the terminal they are started from (PowerShell or cmd).

### 5.4 Kiosk PCs

For shared terminals on the shop floor: save the tileset as a template (Settings → Templates), set a
PIN (Settings → Privacy) **before** enabling kiosk mode, then
`"kiosk": {"enabled": true, "template": "Line 1"}`. Brust starts full screen with that template and
the tab structure, layouts, settings and developer tools are locked; Ctrl+Alt+Shift+K + PIN
leaves kiosk mode until the next launch. Kiosk mode and the PIN screen are a convenience, not an OS
lockdown: combine them with a restricted Windows account.

## 6. Recommended conventions

* **Events**: `domain.action`, in the past tense or progressive form: `test.progress`, `test.completed`,
  `order.saved`, `scale.weight`.
* **Methods**: imperative verb, camelCase: `startTest`, `stopTest`, `openOrder`, `status`.
* **Parameters and results**: JSON objects, never bare values, so they can be extended. Include `v`.
* **Errors**: messages the operator can understand. If you need machine codes, use
  `"error":"BUSY: test already running"`.
* **Document the contract** of each program (methods, parameters, events) in a file next to the code.

## 7. Testing and troubleshooting

* `examples/demo.html` + `examples/tester.py`: a complete run of the example flow
  (`python -m http.server 3000` in the `examples` folder, app with url `http://localhost:3000/demo.html`).
* With `"devtools": true` in `config.json` every pane has the `</>` button (JS console).
* To simulate a local program you can use a generic WebSocket client (e.g. `wscat`):

```bash
wscat -c "ws://127.0.0.1:47800/?token=TOKEN&name=tester"
```

* Settings → Privacy → *Activity log* shows which commands an app invoked and whether they were
  allowed, confirmed, denied or refused (missing permission); the full file is `audit.log` in the
  data folder. An error `'X' permission not granted to the app` means `permissions` lacks `X`.
* SDK tests: `python3 -m pytest -q sdk/python`, `node sdk/node/test.mjs`.

## 8. Checklist

- [ ] App in `config.json` with `id`, `url`, any `allowed_origins` and only the permissions it needs
- [ ] Web app: a single integration module, also works outside Brust
- [ ] Web app: buttons tied to the software's availability (`bridge.clients` + events)
- [ ] Web app: `timeout_ms`, error handling, correlation ids in events
- [ ] Web app: handles `denied by the user` on sensitive commands; tested with tracker blocking on
- [ ] Apps sharing an SSO login: decide on `isolated`; devices (camera, …) in `web_permissions`
- [ ] Software: stable name, reconnection, token from configuration
- [ ] Software: method table, always one `reply`, long work async, "busy" handling
- [ ] Contract (methods, parameters, events, version) documented
- [ ] Fleet: private key kept safe, central configuration and manifest signed and verified with `--verify`
