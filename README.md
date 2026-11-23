# Brust

A Rust desktop client dedicated to web applications (Ordo, Metodo, …). It is a "closed" browser,
with no Back/Forward, that acts as a **bridge** between the web applications, the local PC (processes,
files, clipboard, serial ports) and other installed software.

* **Tabs with tilesets**: each tab splits the view into 1, 2, 3 or 4 panes (7 layouts).
* **Panes**: a configured app, any web address, or a **terminal** (PowerShell, cmd, bash).
* **Resizable panes**: drag the dividers (double-click = reset). The proportions are
  **remembered per combination of applications** open in the layout.
* Changing layout closes nothing: extra panes stay active and hidden, and reappear when you
  switch back to a layout with more tiles.
* **Refresh** (button, F5, Ctrl+R) reloads the whole tab / tileset; each pane also has its own
  button. Back/Forward are disabled (Alt+arrows, side mouse buttons).
* The session (tabs, layouts, contents) and logins (cookies) persist between launches.

## Building

Windows requirements: Rust (MSVC toolchain) and the WebView2 Runtime (already present on Windows 10/11).

```bash
cargo build --release
```

The executable is `target\release\brust.exe`, a single file with all resources embedded, xterm.js included.
With the MSVC toolchain the WebView2 loader is linked statically. With the GNU/MinGW toolchain
you must ship `WebView2Loader.dll` next to the exe.
On Linux you need `libwebkit2gtk-4.1-dev` and `libgtk-3-dev`.

## Configuration

On first launch `config.json` is created (⚙ → *Edit config.json*):

* Windows: `%APPDATA%\Pietroni\Brust\config\config.json`
* Linux: `~/.config/brust/config.json`

```json
{
  "apps": [
    {
      "id": "ordo",
      "name": "Ordo",
      "url": "https://ordo.mydomain.com",
      "icon": "📦",
      "allowed_origins": ["https://auth.mydomain.com"],
      "external_links_in_browser": true,
      "permissions": ["messaging", "exec", "fs", "clipboard", "serial", "open"],
      "exec_allow": ["powershell", "notepad", "C:\\Tools\\labeler.exe"],
      "fs_roots": ["C:\\Exchange\\Ordo"],
      "zoom": 1.0,
      "user_agent": null,
      "inject_script": "C:\\Brust\\ordo-adapter.js"
    }
  ],
  "shells": [
    { "id": "powershell", "name": "PowerShell", "program": "powershell.exe", "args": ["-NoLogo"] },
    { "id": "pwsh", "name": "PowerShell 7", "program": "pwsh.exe", "args": ["-NoLogo"], "cwd": "C:\\Projects" }
  ],
  "bridge": { "enabled": true, "port": 47800, "token": "auto-generated" },
  "devtools": false
}
```

| Field | Meaning |
|---|---|
| `allowed_origins` | Origins that belong to the app: in-app navigation and access to the bridge. |
| `external_links_in_browser` | `true`: links to other sites open in the system browser. `false`: everything stays in the pane (e.g. SSO), but the bridge is still limited to the app's origins. |
| `permissions` | Granted capabilities (see below). Without permissions the app can only use info and UI commands. |
| `exec_allow` | Programs that can be started with `shell.exec` (matched on the file name). `"*"` = all. |
| `fs_roots` | Folders accessible through `fs.*`. `"*"` = the whole disk. |
| `inject_script` | JS injected into every page: an adapter for apps that don't know about Brust. |

After a change: ⚙ → *Reload configuration*. Panes that are already open keep their previous
settings until they are reopened. Changes to the WebSocket bridge take effect on the next launch.

## API for web apps (`window.brust`)

The script is injected into every page of the app and answers **only the app's origins**.
The `brust:ready` event signals that it is available.

```js
window.addEventListener("brust:ready", async () => {
  const info = await brust.info();                       // os, hostname, username, version…

  // Local capabilities (each requires the matching permission)
  const r = await brust.exec("powershell", ["-Command", "Get-Date"], { timeout_ms: 10000 });
  await brust.invoke("fs.write", { path: "C:\\Exchange\\Ordo\\order.txt", data: "..." });
  const text = await brust.invoke("clipboard.read");
  const ports = await brust.invoke("serial.list");
  const { handle } = await brust.invoke("serial.open", { port: "COM3", baud: 9600 });
  brust.on("serial.data", d => console.log(d.handle, d.text, d.base64));
  await brust.invoke("serial.write", { handle, data: "WEIGHT\r\n" });

  // Messaging between apps and with external software ("messaging" permission)
  brust.handle("openOrder", async ({ number }, { from }) => { /* … */ return true; });
  const ok = await brust.call("metodo", "compute", { code: "A1" });   // calls another app
  brust.emit("order.saved", { number: 12 }, "*");   // "*" = all apps and external software
  brust.emit("order.saved", { number: 12 });        // external software only
  brust.on("order.created", (data, meta) => console.log(meta.source, data));

  // User interface
  await brust.invoke("ui.open", { app: "metodo" });          // new tab
  await brust.invoke("ui.set_title", { title: "Order 12" });
  await brust.invoke("ui.notify", { message: "Printing completed" });
});
```

| Command | Permission | Arguments |
|---|---|---|
| `system.info` | — | |
| `ui.open`, `ui.set_title`, `ui.focus`, `ui.notify`, `bridge.apps` | — | `{app?, url?}`, `{title}`, `{}`, `{message}` |
| `shell.exec` | `exec` | `{program, args[], cwd?, env?, stdin?, timeout_ms?}` → `{code, stdout, stderr, timed_out}` |
| `env.get` | `exec` | `{name}` |
| `shell.open` | `open` | `{target}`: file, folder or URL, opened with the default app |
| `fs.read` / `fs.write` | `fs` | `{path, encoding?: "utf8"\|"base64"}` / `{path, data, encoding?, append?}` |
| `fs.list`, `fs.exists`, `fs.mkdir`, `fs.remove` | `fs` | `{path}` (absolute path, inside `fs_roots`) |
| `clipboard.read` / `clipboard.write` | `clipboard` | — / `{text}` |
| `serial.list`, `serial.open`, `serial.write`, `serial.close` | `serial` | `{port, baud, data_bits?, parity?, stop_bits?}`, `{handle, data, encoding?}`, `{handle}` |
| `bridge.call`, `bridge.clients` | `messaging` | `{target, method, params}`: `target` is an app or a local program |

Events received with `brust.on`: those emitted by other apps or programs, plus `serial.data`,
`serial.closed`, `bridge.client_connected` and `bridge.client_disconnected`. With `"*"` you receive all of them.

## Bridge for local software (WebSocket)

External software (management systems, drivers, scripts, services) connects to
`ws://127.0.0.1:47800/?token=TOKEN&name=client-name`, or sends the token in the
`X-Brust-Token` header. The address and token are in ⚙ Settings. The server only accepts local
connections. JSON messages:

```jsonc
{"type":"emit", "target":"ordo", "event":"order.created", "data":{...}}   // target "*" = all
{"type":"call", "id":1, "target":"ordo", "method":"openOrder", "params":{...}}
//  → {"type":"result","id":1,"ok":true,"result":...}
{"type":"open", "app":"metodo"}        // or {"type":"open","url":"https://..."}
{"type":"focus", "app":"ordo"}
{"type":"list", "id":2}                // tabs, panes and configured apps
{"type":"reload"}
{"type":"ping"}
```

Web apps can also **call local software**: `brust.call("tester", "startTest", {...})`
is delivered to the client connected with `name=tester` as
`{"type":"call","id":7,"method":"startTest","params":{...},"from":"ordo"}`.
The software answers with `{"type":"reply","id":7,"ok":true,"result":{...}}` (or `"ok":false,"error":"..."`)
and the app's Promise resolves with the result. If the software disconnects before replying,
the call fails immediately. A call first looks for an open app with that id, then for a program
with that name (`"external:name"` forces the program).

The client also receives `{"type":"event","source":"ordo","event":...,"data":...}` for every
`brust.emit` aimed at external software. Full PowerShell example:
`examples/external_client.ps1`. `examples/tester.py` contains a sample local program
(a test bench) and `examples/demo.html` a test page for the whole API.

**How to structure web apps and local software: see [GUIDE.md](GUIDE.md).**

## Usage

| Action | How |
|---|---|
| New tab | `+` or Ctrl+T |
| Change layout | icons at the top right |
| Choose/change a pane's content | picker in the empty pane, or ⇄ in the header |
| Swap two panes | drag one pane's header onto the other |
| Rename / reorder / close tabs | double-click / drag / ✕ or middle-click |
| Close a pane | ✕ (if it is already empty, the tileset shrinks) |
| Move between tabs and panes | Ctrl+Tab, Ctrl+Shift+Tab, Alt+1…4 |
| Terminal: copy/paste | Ctrl+Shift+C / Ctrl+Shift+V or right-click |
| Restart a terminal that has exited | Enter, or ⟳ in the header |

## Structure

```
src/main.rs      window, tabs/panes, IPC, message routing
src/layout.rs    geometry of the 7 layouts and dividers
src/config.rs    config.json, session.json, sizes.json
src/locals.rs    local capabilities (processes, files, clipboard, serial)
src/pty.rs       terminals (ConPTY on Windows)
src/ws.rs        local WebSocket server
assets/          shell.html (interface), terminal.html, bridge.js, xterm.js
```

## Security

* The bridge only answers the app's origins. An external site opened in the same pane gets
  no permissions.
* Each capability must be granted explicitly per app. `exec_allow` and `fs_roots` restrict
  access further.
* The WebSocket listens only on 127.0.0.1 and requires the token.
