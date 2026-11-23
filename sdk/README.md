# Brust SDKs

Client libraries for the two Brust integration surfaces:

| SDK | For | File(s) | Dependencies | Status |
|---|---|---|---|---|
| JavaScript (web) | web apps running inside Brust (`window.brust`) | `js/brust.js`, `js/brust.d.ts` | none | smoke-tested in Node with a fake `window.brust` |
| Python | local software on the WebSocket bridge | `python/brust_client.py` | `websockets` | tested (`python3 -m pytest -q sdk/python`) |
| Node.js | local software on the WebSocket bridge | `node/brust-client.mjs` | none (Node ≥ 22, global `WebSocket`) | tested (`node sdk/node/test.mjs`) |
| C# / .NET | local software on the WebSocket bridge | `csharp/BrustClient.cs` | none (.NET 6+) | **not compiled** – see below |

The protocol itself is described in `../README.md` and `../GUIDE.md`. In short:

* **call** – request → exactly one reply (or error);
* **event** – notification with no reply (`emit` / `on`);
* **invoke** – a local command performed by Brust itself (web apps only).

> **New in this release:** the client commands `notes.append`, `ui.badge`, `print.page` and
> `print.list` are being added to Brust in this release. The web SDK already exposes them
> (`notes.append`, `ui.badge`, `print.page`, `print.printers`); on an older Brust they reject
> with `unknown command: …`.

---

## 1. Web apps – `js/brust.js`

An ES module that wraps `window.brust`. Keep every Brust call behind it so the app also works in a
normal browser: there `ready()` resolves to `false` and the other functions throw (or reject with)
`Error("Not running inside Brust")`.

```js
import * as brust from "./brust.js";

if (await brust.ready()) {                       // false after 1.5 s in a normal browser
  // Enable "Start test" only while the local program is connected
  await brust.watchClients(names => {
    startButton.disabled = !brust.isConnected("tester");
  });

  const stop = brust.on("test.progress", d => d.code === "PZ-001" && showProgress(d.percent));
  try {
    const result = await brust.call("tester", "startTest", { v: 1, code: "PZ-001", repetitions: 3 },
                                    { timeoutMs: 120000 });
    showResult(result);                          // { code, outcome, measurements }
  } catch (e) {
    showError(e.message);                        // not connected, error, timeout, disconnection
  } finally { stop(); }

  // Methods other apps / local software can call
  brust.handle("openOrder", async ({ number }, { from }) => { openOrder(number); return true; });

  brust.emit("order.saved", { number: 12 }, "*");     // "*" = all apps + local software; omit = local software only
  await brust.invoke("clipboard.write", { text: "hello" });

  // New client commands
  await brust.notes.append("Order 12 shipped", "Shipping");   // invoke("notes.append", {text, notebook})
  await brust.ui.notify("Printing completed", "CRM");         // invoke("ui.notify", {message, title})
  await brust.ui.badge(3);                                      // invoke("ui.badge", {count})
  const printers = await brust.print.printers();                // invoke("print.list")
  await brust.print.page({ printer: printers[0]?.name });       // invoke("print.page", opts)
}
```

API: `ready(timeoutMs=1500)`, `inBrust()`, `context()`, `invoke(cmd, args)`,
`call(target, method, params, {timeoutMs})`, `emit(event, data, target)`, `on(event, fn)` /
`once(event, fn)` (return an unsubscribe function), `handle(method, fn)`, `exec(program, args, opts)`,
`info()`, `watchClients(onChange)` (uses `bridge.clients` plus the `bridge.client_connected` /
`bridge.client_disconnected` events; needs the `messaging` permission) and `isConnected(name)`,
`notes.append`, `ui.notify` / `ui.badge` / `ui.setTitle` / `ui.open` / `ui.focus`, `print.page` /
`print.printers`.

TypeScript: `brust.d.ts` declares the module and augments `Window` with `brust?: BrustBridge`.

---

## 2. Local software – Python (`python/brust_client.py`)

```bash
pip install websockets
export BRUST_TOKEN=<token from Brust → ⚙ Settings>
python sdk/python/example_tester.py --name tester
```

```python
from brust_client import BrustClient

client = BrustClient(name="tester")          # token=..., port=47800, host="127.0.0.1"

@client.method("startTest")
async def start_test(params, ctx):           # sync functions work too
    for i in range(params.get("repetitions", 1)):
        await ctx.emit_to_caller("test.progress", {"code": params["code"], "step": i + 1})
    return {"code": params["code"], "outcome": "OK", "measurements": [12.0]}
    # raising an exception → {"type":"reply","ok":false,"error":"<message>"}

@client.on("order.saved")                    # "*" = every event
def order_saved(data, meta):                 # meta = {"event": ..., "source": ...}
    print(meta["source"], data)

@client.on_connect
async def connected(welcome):                # after every (re)connection
    print(await client.list())               # tabs, panes, apps
    total = await client.call("crm", "sum", {"a": 2, "b": 40}, timeout=10)
    await client.emit("scale.weight", {"kg": 1.5}, target="crm")

client.run()                                 # blocking; or: await client.serve()
```

Behaviour:

* every incoming `{"type":"call"}` gets exactly one `{"type":"reply"}`; exceptions, unknown methods
  and non-JSON results become `ok:false`;
* handlers and listeners run as separate tasks, so long jobs do not block other messages;
* `call` / `list` / `open` / `focus` / `reload` use the client's own incrementing ids and wait for the
  matching `{"type":"result"}`; errors raise `BrustError`, timeouts `asyncio.TimeoutError`, and pending
  requests fail with `ConnectionError` as soon as the connection drops;
* `emit` is fire-and-forget (`wait=True` waits for Brust's acknowledgement);
* automatic reconnection with exponential backoff (`min_backoff` … `max_backoff`); `await client.close()`
  stops it.

Tests: `python3 -m pytest -q sdk/python` (needs `pytest`; no pytest plugins). They run the client
against an in-process fake bridge built with `websockets.serve`.

---

## 3. Local software – Node.js (`node/brust-client.mjs`)

Node ≥ 22, no dependencies.

```js
import { BrustClient } from "./brust-client.mjs";

const client = new BrustClient({ name: "tester", logger: console });  // token: or $BRUST_TOKEN; port: 47800

client.method("startTest", async (params, ctx) => {
  await ctx.emitToCaller("test.progress", { code: params.code, percent: 50 });
  return { code: params.code, outcome: "OK", measurements: [12.0] };
});
client.on("order.saved", (data, meta) => console.log(meta.source, data));   // returns unsubscribe
client.onConnect(async () => {
  console.log(await client.list());
  await client.call("crm", "sum", { a: 2, b: 40 }, { timeoutMs: 10000 });
  await client.emit("scale.weight", { kg: 1.5 }, "crm");
  await client.open({ app: "erp" });
  await client.focus("crm");
});
client.start();            // reconnects automatically; await client.stop() to quit
```

Same semantics as the Python SDK (`BrustError` for remote errors, `TimeoutError` for timeouts).
Example: `node sdk/node/example-tester.mjs --name tester`. Tests: `node sdk/node/test.mjs` – they use
Node's real `WebSocket` against a minimal RFC 6455 server written over `net`.

---

## 4. Local software – C# / .NET (`csharp/BrustClient.cs`)

> **Not compiled.** No .NET SDK was available when this file was written, so `BrustClient.cs` and
> `ExampleTester.cs` have been reviewed but never built or run. Build and test them before use.

Single file, .NET 6+, only `System.Net.WebSockets.ClientWebSocket` and `System.Text.Json`. Copy it into
your project.

```csharp
using Brust.Client;

var client = new BrustClient(token: null, name: "tester") { Log = Console.WriteLine }; // null → BRUST_TOKEN

client.RegisterMethod("startTest", async (p, ctx) =>
{
    string code = (string?)p?["code"] ?? throw new Exception("missing 'code'");
    await ctx.EmitToCallerAsync("test.progress", new { code, percent = 50 });
    return new { code, outcome = "OK" };       // exceptions → ok:false
});
using var sub = client.On("order.saved", (data, source) => Console.WriteLine($"{source}: {data}"));
client.Connected += async welcome =>
{
    var sum = await client.CallAsync("crm", "sum", new { a = 2, b = 40 }, TimeSpan.FromSeconds(10));
    await client.EmitAsync("scale.weight", new { kg = 1.5 }, "crm");
};

await client.RunAsync(cancellationToken);      // reconnect loop with backoff
```

Details: sends are serialized with a `SemaphoreSlim`; incoming frames are reassembled until
`EndOfMessage`; each incoming call runs on its own task and gets exactly one reply; `CallAsync`,
`ListAsync`, `OpenAsync`, `FocusAsync` throw `BrustException` (remote error), `TimeoutException` or
`IOException` (connection lost). A complete program is in `csharp/ExampleTester.cs`.
