# Guide: designing web apps and local software for Brust

This guide explains how to build web applications and local programs that use Brust as a bridge
between the browser and the PC. For the full list of commands see `README.md`.

## 1. The three actors and the three message types

```
 ┌──────────────┐   window.brust    ┌──────────────┐   WebSocket 127.0.0.1   ┌──────────────────┐
 │   Web app    │ ◄───────────────► │    Brust     │ ◄─────────────────────► │  Local software  │
 │ (Ordo, …)    │    (injected)     │   (client)   │  token + client name    │ (tester, driver) │
 └──────────────┘                   └──────────────┘                         └──────────────────┘
        app id: "ordo"                                                         name: "tester"
```

Every participant has a **name**:

* web apps have the `id` defined in `config.json` (e.g. `ordo`);
* local programs have the name they connect with (`?name=tester`).

Brust routes messages based on that name. There are three message types:

| Type | What it is for | Web app | Local software |
|---|---|---|---|
| **Local command** (`invoke`) | Brust itself performs an action on the PC: processes, files, serial, clipboard | `brust.invoke("fs.read", …)` | — |
| **Call** (`call`) | Request → **one** reply (or error). For "do this and tell me how it went" | `brust.call(name, method, params)` · `brust.handle(method, fn)` | receives `{"type":"call"}` → replies `{"type":"reply"}`; sends `{"type":"call"}` → receives `{"type":"result"}` |
| **Event** (`emit`/`on`) | Notification with no reply: progress, state changes, alarms | `brust.emit(event, data, target)` · `brust.on(event, fn)` | sends `{"type":"emit"}` · receives `{"type":"event"}` |

A call to a name is delivered to the open web app with that `id` (preferring the active tab).
If no app has that `id`, it goes to the local program connected with that name. The
`external:` prefix (e.g. `"external:tester"`) forces the local program.

## 2. Full example: starting a test

```
Web app (ordo)                     Brust                          Local software (tester)
     │ brust.call("tester","startTest",{code})                              │
     │──────────────────────────────►│ {"type":"call","id":7,"method":"startTest",
     │                               │  "params":{…},"from":"ordo"} ───────►│
     │                               │                                      │ runs the test
     │                               │◄── {"type":"emit","target":"ordo",   │
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
`{"type":"call","id":1,"target":"ordo","method":"openOrder","params":{…}}`, receiving
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
   with tight `exec_allow` and `fs_roots`.
6. **Incoming commands with `brust.handle`.** Always validate the parameters: they come from another
   program.
7. **Existing apps that cannot be modified** are adapted with `inject_script`. It is a local JS
   file injected into the pages, which can read the DOM, add buttons and call `brust`.

### 3.3 When to use what

| Need | Recommended solution |
|---|---|
| Run a command-line program and read its output | `brust.exec(...)` (`exec` permission). Have the program print JSON to stdout. |
| Talk to a simple serial instrument | `serial.open/write` + `serial.data` event |
| Long-running, stateful operations, complex hardware, proprietary SDKs | **local software** connected to the bridge (`call` + events) |
| Two web apps working together (Ordo ↔ Metodo) | `brust.handle` in one, `brust.call` in the other |
| Notify several recipients | `brust.emit(event, data, "*")` |
| Read/write exchange files with other programs | `fs.*` (`fs` permission + `fs_roots`) |

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
  The token is in Brust → ⚙ Settings (also in `config.json` → `bridge.token`).

### 4.2 Message reference, software side

Connection: `ws://127.0.0.1:47800/?token=TOKEN&name=NAME`

| Received by the software | Meaning |
|---|---|
| `{"type":"welcome","client":3,"apps":[…]}` | connection accepted |
| `{"type":"call","id":7,"method":"…","params":{…},"from":"ordo"}` | an app is asking for something: answer with `reply` |
| `{"type":"event","event":"…","data":{…},"source":"ordo"}` | event emitted by an app to external software or `"*"` |
| `{"type":"result","id":…,"ok":…,"result"/"error":…}` | reply to one of your `call`, `list`, `emit`, `open`, … |

| Sent by the software | Meaning |
|---|---|
| `{"type":"reply","id":7,"ok":true,"result":{…}}` | reply to a received `call` (`ok:false,"error":"…"` for errors) |
| `{"type":"emit","target":"ordo","event":"…","data":{…}}` | event to an app (`"*"` = all) |
| `{"type":"call","id":1,"target":"ordo","method":"…","params":{…}}` | calls a method an app registered with `brust.handle` |
| `{"type":"open","app":"metodo"}`, `{"type":"focus","app":"ordo"}` | opens an app or brings it to the front |
| `{"type":"list","id":2}` | tabs, panes and configured apps |

### 4.3 Other languages

Any language with a WebSocket client will do. The pattern is always the same: connect,
read in a loop, dispatch on `type`, answer with `reply`. The C#/.NET skeleton below is a
starting point and **has not been compiled**.

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

## 5. Recommended conventions

* **Events**: `domain.action`, in the past tense or progressive form: `test.progress`, `test.completed`,
  `order.saved`, `scale.weight`.
* **Methods**: imperative verb, camelCase: `startTest`, `stopTest`, `openOrder`, `status`.
* **Parameters and results**: JSON objects, never bare values, so they can be extended. Include `v`.
* **Errors**: messages the operator can understand. If you need machine codes, use
  `"error":"BUSY: test already running"`.
* **Document the contract** of each program (methods, parameters, events) in a file next to the code.

## 6. Testing and troubleshooting

* `examples/demo.html` + `examples/tester.py`: a complete run of the example flow
  (`python -m http.server 3000` in the `examples` folder, app with url `http://localhost:3000/demo.html`).
* With `"devtools": true` in `config.json` every pane has the `</>` button (JS console).
* To simulate a local program you can use a generic WebSocket client (e.g. `wscat`):

```bash
wscat -c "ws://127.0.0.1:47800/?token=TOKEN&name=tester"
```

## 7. Checklist

- [ ] App in `config.json` with `id`, `url`, any `allowed_origins` and only the permissions it needs
- [ ] Web app: a single integration module, also works outside Brust
- [ ] Web app: buttons tied to the software's availability (`bridge.clients` + events)
- [ ] Web app: `timeout_ms`, error handling, correlation ids in events
- [ ] Software: stable name, reconnection, token from configuration
- [ ] Software: method table, always one `reply`, long work async, "busy" handling
- [ ] Contract (methods, parameters, events, version) documented
