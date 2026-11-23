# Brust

A Rust desktop client for any web application (ERP, CRM, MES, dashboards, intranet tools, …). It is a "closed" browser,
with no Back/Forward, that acts as a **bridge** between the web applications, the local PC (processes,
files, clipboard, serial ports, printers) and other installed software.

* **Tabs with tilesets**: each tab splits the view into 1, 2, 3 or 4 panes (7 layouts).
* **Panes**: a configured app, any web address (optionally **private**), a **terminal**
  (PowerShell, cmd, bash) or the **notes**.
* **Resizable panes**: drag the dividers (double-click = reset). The proportions are
  **remembered per combination of applications** open in the layout.
* Changing layout closes nothing: extra panes stay active and hidden, and reappear when you
  switch back to a layout with more tiles.
* **Refresh** (button, F5, Ctrl+R) reloads the whole tab / tileset; each pane also has its own
  button. Back/Forward are disabled (Alt+arrows, side mouse buttons).
* The session (tabs, layouts, contents) and logins (cookies) persist between launches.
* **Privacy**: a separate browser profile per app, private mode, tracker/ad blocking, per-app
  browser permissions, confirmation of sensitive commands, activity log, PIN lock.
* **Productivity**: local Markdown notes with quick capture, templates (saved tilesets),
  command palette (Ctrl+K), command line and `brust://` links, native notifications and badges,
  panes detached into their own window.
* **Industrial**: printing, kiosk mode, signed central configuration, signed self-update.
* **SDKs** for web apps (JS) and local software (Python, Node.js, C#) in [`sdk/`](sdk/README.md).

## Building

Windows requirements: Rust with the **MSVC toolchain** and the WebView2 Runtime (already present on
Windows 10/11). `cargo build` on Windows needs the Visual Studio Build Tools (C/C++ workload),
because the HTTPS stack (`ring`) compiles C code.

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
      "id": "crm",
      "name": "CRM",
      "url": "https://crm.mydomain.com",
      "icon": "📦",
      "allowed_origins": ["https://auth.mydomain.com"],
      "external_links_in_browser": true,
      "permissions": ["messaging", "exec", "fs", "clipboard", "serial", "open", "notes", "print"],
      "exec_allow": ["powershell", "notepad", "C:\\Tools\\labeler.exe"],
      "fs_roots": ["C:\\Exchange\\CRM"],
      "zoom": 1.0,
      "user_agent": null,
      "inject_script": "C:\\Brust\\crm-adapter.js",
      "isolated": true,
      "incognito": false,
      "clear_on_exit": false,
      "block_trackers": true,
      "confirm": true,
      "web_permissions": { "camera": "allow", "notifications": "deny" }
    }
  ],
  "shells": [
    { "id": "powershell", "name": "PowerShell", "program": "powershell.exe", "args": ["-NoLogo"] },
    { "id": "pwsh", "name": "PowerShell 7", "program": "pwsh.exe", "args": ["-NoLogo"], "cwd": "C:\\Projects" }
  ],
  "bridge": { "enabled": true, "port": 47800, "token": "" },
  "devtools": false,
  "privacy": {
    "block_trackers": false,
    "blocklists": ["C:\\Brust\\hosts.txt"],
    "block_domains": ["ads.example.com"],
    "allow_domains": ["cdn.example.com"],
    "confirm_sensitive": true,
    "audit_log": true,
    "lock_after_minutes": 0
  },
  "notes": { "dir": null },
  "kiosk": { "enabled": false, "template": null },
  "remote_config": null,
  "update": null,
  "register_protocol": true,
  "default_printer": null
}
```

### App fields

| Field | Meaning |
|---|---|
| `allowed_origins` | Origins that belong to the app: in-app navigation and access to the bridge. |
| `external_links_in_browser` | `true`: links to other sites open in the system browser. `false`: everything stays in the pane (e.g. SSO), but the bridge is still limited to the app's origins. |
| `permissions` | Granted capabilities (see below). Without permissions the app can only use info and UI commands. |
| `exec_allow` | Programs that can be started with `shell.exec` (matched on the file name). `"*"` = all. |
| `fs_roots` | Folders accessible through `fs.*` and `print.file`. `"*"` = the whole disk. |
| `inject_script` | JS injected into every page: an adapter for apps that don't know about Brust. |
| `isolated` | Default `true`: the app has its own browser profile (cookies, storage), separate from the other apps. `false`: it shares one profile with the other non-isolated apps. |
| `incognito` | `true`: nothing (cookies, storage) is stored on disk; the app starts logged out every time. |
| `clear_on_exit` | `true`: the app's stored browser data is deleted when Brust exits (and at the next start, in case of a crash). Only for isolated apps. |
| `block_trackers` | Overrides `privacy.block_trackers` for this app (`true`/`false`). |
| `confirm` | Overrides `privacy.confirm_sensitive` for this app. |
| `web_permissions` | Browser permissions: name → `"allow"`, `"deny"` or `"default"` (browser default). |

Browser permission names: `camera`, `microphone`, `geolocation`, `notifications`, `clipboard_read`,
`display_capture`, `midi`, `sensors`, `local_fonts`, `window_management`, `pointer_lock`,
`automatic_downloads`, `file_system_access`, `autoplay`, `media_key_system_access`.
**`camera`, `microphone`, `geolocation` and `display_capture` are denied** unless set to
`"allow"`; the others follow the browser default unless configured. Free web pages (not apps)
always use this default policy.

### Other sections

| Key | Default | Meaning |
|---|---|---|
| `bridge.token` | `""` | Token of the WebSocket bridge. If empty, Brust generates one and stores it in `bridge-token.dat` in the config folder (encrypted with Windows DPAPI for the current user; on other systems a file readable only by the user). |
| `privacy.block_trackers` | `false` | Blocks trackers, ads and analytics for apps and web pages through a local filtering proxy (built-in list of well-known domains). |
| `privacy.blocklists` | `[]` | Extra block lists: local files in hosts format or one domain per line. |
| `privacy.block_domains` / `allow_domains` | `[]` | Extra domains to block / never block. A domain also covers its subdomains. |
| `privacy.confirm_sensitive` | `true` | Ask the user before sensitive bridge commands (see *Privacy and security*). |
| `privacy.audit_log` | `true` | Record bridge activity in `audit.log`. |
| `privacy.lock_after_minutes` | `0` | Lock Brust after this many idle minutes (0 = never). Requires a PIN. |
| `notes.dir` | `null` | Folder of the notebooks. Default: `Documents\Brust Notes`. |
| `kiosk.enabled` / `kiosk.template` | `false` / `null` | Kiosk mode and the template opened at start (see *Kiosk mode*). |
| `remote_config` | `null` | Signed central configuration: `{url, public_key, refresh_minutes}`. |
| `update` | `null` | Self-update: `{manifest_url, public_key, check_on_start}`. |
| `register_protocol` | `true` | Windows: register `brust://` links for the current user (HKCU) at every start. `false` stops the registration (an existing one is not removed). |
| `default_printer` | `null` | Printer used by `print.page` when the app does not name one (system default if empty). |

After a change: ⚙ → *Reload configuration*. Panes that are already open keep their previous
settings until they are reopened. Changes to the WebSocket bridge take effect on the next launch.
The tracker-blocking proxy is started at launch (or on reload, if blocking was just enabled).

Files in the config folder: `config.json`, `session.json`, `sizes.json`, `templates.json`,
`grants.json`, `remote-config.cache.json`, `bridge-token.dat`, `pin.dat`.
Files in the data folder (`%LOCALAPPDATA%\Pietroni\Brust\data`, Linux `~/.local/share/brust`):
`profiles\` (browser profiles) and `audit.log`.

## API for web apps (`window.brust`)

The script is injected into every page of the app and answers **only the app's origins**.
The `brust:ready` event signals that it is available. A ready-made wrapper is in `sdk/js/brust.js`.

```js
window.addEventListener("brust:ready", async () => {
  const info = await brust.info();                       // os, hostname, username, version…

  // Local capabilities (each requires the matching permission)
  const r = await brust.exec("powershell", ["-Command", "Get-Date"], { timeout_ms: 10000 });
  await brust.invoke("fs.write", { path: "C:\\Exchange\\CRM\\order.txt", data: "..." });
  const text = await brust.invoke("clipboard.read");
  const ports = await brust.invoke("serial.list");
  const { handle } = await brust.invoke("serial.open", { port: "COM3", baud: 9600 });
  brust.on("serial.data", d => console.log(d.handle, d.text, d.base64));
  await brust.invoke("serial.write", { handle, data: "WEIGHT\r\n" });

  // Messaging between apps and with external software ("messaging" permission)
  brust.handle("openOrder", async ({ number }, { from }) => { /* … */ return true; });
  const ok = await brust.call("erp", "compute", { code: "A1" });   // calls another app
  brust.emit("order.saved", { number: 12 }, "*");   // "*" = all apps and external software
  brust.emit("order.saved", { number: 12 });        // external software only
  brust.on("order.created", (data, meta) => console.log(meta.source, data));

  // Notes ("notes" permission) and printing ("print" permission)
  await brust.invoke("notes.append", { notebook: "Shipping", text: "Order 12 shipped" });
  const printers = await brust.invoke("print.list");            // [{name, default}]
  await brust.invoke("print.page", { printer: "Label printer", silent: true });
  await brust.invoke("print.file", { path: "C:\\Exchange\\CRM\\label.pdf" });

  // User interface
  await brust.invoke("ui.open", { app: "erp" });          // new tab
  await brust.invoke("ui.set_title", { title: "Order 12" });
  await brust.invoke("ui.notify", { message: "Printing completed", native: true });
  await brust.invoke("ui.badge", { count: 3 });              // 0 removes the badge
});
```

| Command | Permission | Arguments |
|---|---|---|
| `system.info` | — | |
| `ui.open`, `ui.set_title`, `ui.focus`, `bridge.apps` | — | `{app?, url?, private?}`, `{title}`, `{}` |
| `ui.notify` | — | `{message, title?, native?}`: in-app toast; also a native desktop notification when the window is not focused or `native: true` |
| `ui.badge` | — | `{count}`: counter on the pane header and tab, summed in the window title |
| `shell.exec` ⚠ | `exec` | `{program, args[], cwd?, env?, stdin?, timeout_ms?}` → `{code, stdout, stderr, timed_out}` |
| `env.get` | `exec` | `{name}` |
| `shell.open` ⚠ | `open` | `{target}`: file, folder or URL, opened with the default app |
| `fs.read` / `fs.write` ⚠ | `fs` | `{path, encoding?: "utf8"\|"base64"}` / `{path, data, encoding?, append?}` |
| `fs.list`, `fs.exists`, `fs.mkdir` ⚠, `fs.remove` ⚠ | `fs` | `{path}` (absolute path, inside `fs_roots`) |
| `clipboard.read` / `clipboard.write` | `clipboard` | — / `{text}` |
| `serial.list`, `serial.open` ⚠, `serial.write`, `serial.close` | `serial` | `{port, baud, data_bits?, parity?, stop_bits?}`, `{handle, data, encoding?}`, `{handle}` |
| `notes.append`, `notes.read`, `notes.list` | `notes` | `{notebook?, text}`, `{notebook?}` → text, `{}` → `[{name, modified, size}]`. Default notebook: `Inbox` |
| `print.list` | `print` | → `[{name, default}]` |
| `print.file` ⚠ | `print` | `{path, printer?}`: file inside `fs_roots`, printed without a dialog (system default printer if `printer` is omitted) |
| `print.page` | `print` | `{printer?, silent?}` (default `silent: true`): prints the current page. Silent printing works on Windows (WebView2); elsewhere the print dialog opens. Without `printer`, `default_printer` or the system default is used |
| `bridge.call`, `bridge.clients` | `messaging` | `{target, method, params}`: `target` is an app or a local program |

⚠ = sensitive: the user is asked to confirm (see *Privacy and security*).

Events received with `brust.on`: those emitted by other apps or programs, plus `serial.data`,
`serial.closed`, `bridge.client_connected` and `bridge.client_disconnected`. With `"*"` you receive all of them.

## Bridge for local software (WebSocket)

External software (management systems, drivers, scripts, services) connects to
`ws://127.0.0.1:47800/?token=TOKEN&name=client-name`, or sends the token in the
`X-Brust-Token` header. The address and token are in ⚙ Settings. The server only accepts local
connections. JSON messages:

```jsonc
{"type":"emit", "target":"crm", "event":"order.created", "data":{...}}   // target "*" = all
{"type":"call", "id":1, "target":"crm", "method":"openOrder", "params":{...}}
//  → {"type":"result","id":1,"ok":true,"result":...}
{"type":"open", "app":"erp"}        // or {"type":"open","url":"https://...","private":true}
{"type":"focus", "app":"crm"}
{"type":"list", "id":2}                // tabs, panes and configured apps
{"type":"open_template", "name":"Testing"}
{"type":"notes_append", "notebook":"Test log", "text":"PZ-001: OK"}   // notebook default "Inbox"
{"type":"cli", "args":["--open","crm"]}   // same arguments as the command line
{"type":"reload"}
{"type":"ping"}
```

Web apps can also **call local software**: `brust.call("tester", "startTest", {...})`
is delivered to the client connected with `name=tester` as
`{"type":"call","id":7,"method":"startTest","params":{...},"from":"crm"}`.
The software answers with `{"type":"reply","id":7,"ok":true,"result":{...}}` (or `"ok":false,"error":"..."`)
and the app's Promise resolves with the result. If the software disconnects before replying,
the call fails immediately. A call first looks for an open app with that id, then for a program
with that name (`"external:name"` forces the program).

The client also receives `{"type":"event","source":"crm","event":...,"data":...}` for every
`brust.emit` aimed at external software. Full PowerShell example:
`examples/external_client.ps1`. `examples/tester.py` contains a sample local program
(a test bench) and `examples/demo.html` a test page for the whole API. Client libraries for
Python, Node.js and C# are in [`sdk/`](sdk/README.md).

**How to structure web apps and local software: see [GUIDE.md](GUIDE.md).**

## Privacy and security

* **Separate profiles.** Each app (`isolated`, default `true`) has its own cookies and storage, so a
  login in one app is not visible to the others. Free web pages share one "web" profile. Enabling or
  disabling tracker blocking for an app moves it to a different profile folder, so you may need to
  log in again.
* **Private mode.** `incognito: true` on an app, or the *Private page* checkbox in the pane picker
  (also *New private web page* in Ctrl+K): nothing is stored on disk. `clear_on_exit: true` keeps
  data during the session and deletes it on exit.
* **Tracker and ad blocking** (`privacy.block_trackers` or per-app `block_trackers`): the panes'
  traffic goes through a filtering proxy on 127.0.0.1 that refuses connections to blocked domains.
  Settings → Privacy shows how many requests were blocked.
* **Browser permissions** (`web_permissions`): camera, microphone, location and screen capture
  are denied unless allowed per app.
* **Confirmation of sensitive commands.** `shell.exec`, `shell.open`, `fs.write`, `fs.remove`,
  `fs.mkdir`, `serial.open` and `print.file` show a dialog with the app and the details:
  **Allow once**, **Always allow** or **Deny** (the app's Promise is rejected with
  `denied by the user`). "Always allow" is remembered per app and command — per program for
  `shell.exec`, per port for `serial.open`, for any argument otherwise — in `grants.json`, and can be
  revoked in Settings → Privacy. Disable with `privacy.confirm_sensitive: false` or per app with
  `confirm: false`.
* **Activity log** (`privacy.audit_log`): `audit.log` in the data folder, one JSON object per line
  (`time`, `app`, `cmd`, `detail`, `decision`: allowed/confirmed/always/denied/refused, `outcome`).
  Local commands, refused commands, `notes.append`, printing and failed unlocks are recorded; file
  contents and clipboard text are not. The file is rotated at 5 MB (`audit.log.1`). Settings →
  Privacy shows the latest entries.
* **PIN lock.** Set a PIN (at least 4 characters) in Settings → Privacy, then lock with
  Ctrl+Shift+L, the palette or *Lock now*; `privacy.lock_after_minutes` locks automatically after
  inactivity. While locked all panes (also detached windows) are hidden. The PIN is stored as a
  salted hash (`pin.dat`, DPAPI-protected on Windows).
* The bridge only answers the app's origins. An external site opened in the same pane gets
  no permissions.
* Each capability must be granted explicitly per app. `exec_allow` and `fs_roots` restrict
  access further.
* The WebSocket listens only on 127.0.0.1 and requires the token.

## Notes

A notes pane edits local Markdown notebooks: one `<name>.md` file per notebook in
`Documents\Brust Notes` (or `notes.dir`). Nothing leaves the PC.

* Open it from the pane picker (*Notes*), Ctrl+K → *Open notes*, `brust --notes [notebook]` or
  `brust://notes?notebook=…`.
* Notebook list (new, rename, delete), search across all notebooks, Edit / Preview (Ctrl+E);
  changes are saved automatically.
* **Quick capture**: select text in any app page and press **Ctrl+Shift+S**. The text is appended
  as a quote with the page title, link and time to the notebook open in the active tab, otherwise
  to `Inbox`.
* Apps with the `notes` permission use `notes.append` / `notes.read` / `notes.list`; local software
  sends `{"type":"notes_append",…}`.
* Settings → Notes shows the folder (*Open folder*).

## Templates, command palette, command line

* **Templates** are saved tilesets (layout, proportions, contents): Settings → Templates → *Save
  current tab*. Open them from Settings, Ctrl+K, `--template`, `brust://template?name=…` or the
  WebSocket message `open_template`. Each one opens in a new tab.
* **Command palette (Ctrl+K)**: open apps (new tab or focused pane), go to tabs and windows, open
  templates, terminals, notes, a private page, change layout, detach the pane, settings, reload, lock.
* **Separate windows**: the ⧉ button in a pane header (or the palette) moves the pane into its own
  window. Settings → *Separate windows* → *Bring back as tab* docks it again; closing the window
  closes the pane.
* **Command line and links**:

```text
brust --open crm                    open an app in a new tab
brust --open crm --url <url>        open a page of the app
brust --url https://example.com      open a web page
brust --template "Testing"           open a template
brust --notes [notebook]             open the notes
brust brust://open?app=crm          the same as links: brust://open?app=…&url=…, brust://url?u=…,
                                     brust://template?name=…, brust://notes?notebook=…
```

  On Windows `brust://` is registered for the current user (`register_protocol`), so these links
  work from other programs and web pages.
* **Single instance**: if Brust is already running, a second launch forwards its arguments to it
  through the WebSocket bridge and exits (this requires the bridge to be enabled).

## Printing

`print.list`, `print.file` and `print.page` (permission `print`, see the API table). On Windows
printers are listed and files printed through PowerShell (`Print`/`PrintTo` of the file's
associated application), and pages are printed silently by WebView2. On Linux/macOS the CUPS tools
(`lpstat`, `lp`) are used and `print.page` opens the print dialog.

## Kiosk mode

```json
{ "kiosk": { "enabled": true, "template": "Line 1" } }
```

Brust starts full screen with the template (otherwise with the saved session). Creating, closing,
renaming and moving tabs, changing layouts or pane contents, detaching panes, the settings and the
developer tools are disabled, and the window cannot be closed (apps and templates can still be
opened from Ctrl+K).
To leave kiosk mode press **Ctrl+Alt+Shift+K** and enter the PIN: Brust leaves full screen and
works normally until the next launch. **Set the PIN before enabling kiosk mode**: without a PIN,
anybody can leave it by pressing Enter.

## Central configuration and updates

For fleets of PCs, the configuration and new versions can be distributed as **signed** files
(ed25519). The signing tools are in Brust itself:

```text
brust --keygen <dir>                                  writes brust-private.key and brust-public.key
brust --sign <in.json> --key <private.key> --out <file>   writes the signed envelope {payload, signature}
brust --verify <envelope.json> --pubkey <b64 or file> prints the payload if the signature is valid
brust --hash-file <file>                              prints the SHA-256 (hex)
```

On Windows these commands print into the terminal they are started from (PowerShell or cmd),
even though `brust.exe` is a windowed program. In PowerShell, `Get-FileHash -Algorithm SHA256`
gives the same hash as `--hash-file` (the comparison ignores case).

**Central configuration**:

```json
{ "remote_config": { "url": "\\\\server\\brust\\config.signed.json", "public_key": "<base64>", "refresh_minutes": 60 } }
```

The signed file (http(s), `file://` URL or local/UNC path, max 10 MB) contains a JSON object whose
top-level keys **replace** those of the local `config.json` (e.g. `"apps"` replaces the whole app
list; `remote_config` itself cannot be overridden). It is checked at start and every
`refresh_minutes` (0 = only at start); a verified copy is cached and used at the next start and
while offline. Status and *Refresh*: Settings → Central configuration & updates.

**Updates**:

```json
{ "update": { "manifest_url": "https://intranet.example.com/brust/manifest.json", "public_key": "<base64>", "check_on_start": true } }
```

The manifest is a signed envelope whose payload is
`{"version":"0.2.0","url":"https://…/brust.exe","sha256":"…","notes":"…"}`. If `version` is newer,
Brust notifies the user; Settings → *Install … and restart* downloads the executable, checks its
SHA-256, replaces `brust.exe` (the previous one is kept as `brust.exe.old` until the next start)
and restarts. Without `check_on_start` use *Check* in Settings. See GUIDE.md for the procedure.

## Usage

| Action | How |
|---|---|
| New tab | `+` or Ctrl+T |
| Change layout | icons at the top right |
| Choose/change a pane's content | picker in the empty pane, or ⇄ in the header |
| Swap two panes | drag one pane's header onto the other |
| Rename / reorder / close tabs | double-click / drag / ✕ or middle-click |
| Close a pane | ✕ (if it is already empty, the tileset shrinks) |
| Open a pane in a separate window | ⧉ in the header |
| Move between tabs and panes | Ctrl+Tab, Ctrl+Shift+Tab, Alt+1…4 |
| Terminal: copy/paste | Ctrl+Shift+C / Ctrl+Shift+V or right-click |
| Restart a terminal that has exited | Enter, or ⟳ in the header |

Shortcuts:

| Key | Action |
|---|---|
| Ctrl+K | Command palette |
| F5 · Ctrl+R | Reload the tab / tileset |
| Ctrl+T | New tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Alt+1…4 | Focus pane |
| Ctrl+Shift+S | Save the selected text to the notes |
| Ctrl+Shift+L | Lock Brust (PIN) |
| Ctrl+E · Ctrl+Shift+F | Notes: switch Edit / Preview · search |
| Ctrl+Shift+C / V | Copy / paste in a terminal (also right-click) |
| Ctrl+Alt+Shift+K | Leave kiosk mode (PIN) |
| Double-click tab / splitter | Rename / reset proportions |

## Structure

```
src/main.rs      window, tabs/panes, IPC, message routing
src/layout.rs    geometry of the 7 layouts and dividers
src/config.rs    config.json, session.json, sizes.json, templates
src/locals.rs    local capabilities (processes, files, clipboard, serial, printing files)
src/pty.rs       terminals (ConPTY on Windows)
src/ws.rs        local WebSocket server
src/cli.rs       command line, brust:// links, single instance
src/adblock.rs   tracker-blocking proxy
src/audit.rs     activity log and remembered grants
src/secrets.rs   DPAPI storage of the token, PIN hashing
src/notes.rs     Markdown notebooks
src/print.rs     printers and printing
src/trust.rs     signatures (keygen/sign/verify), central configuration
src/update.rs    self-update
assets/          shell.html (interface), terminal.html, notes.html, bridge.js, xterm.js
sdk/             JS, Python, Node.js and C# client libraries
```

## License

Brust is licensed under the GPL-3.0-or-later (see `LICENSE`). The bundled xterm.js is MIT-licensed
(`assets/vendor/LICENSE-xterm.txt`).
