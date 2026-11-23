# Ponte

Client desktop in Rust dedicato agli applicativi web (Ordo, Metodo, …). È un browser "chiuso",
senza Avanti/Indietro, che fa da **ponte** fra le applicazioni web, il PC locale (processi, file,
appunti, porte seriali) e altri software installati.

* **Schede con tileset**: ogni scheda divide la vista in 1, 2, 3 o 4 pannelli (7 layout).
* **Pannelli**: applicativo configurato, indirizzo web libero o **terminale** (PowerShell, cmd, bash).
* **Pannelli ridimensionabili**: si trascinano i separatori (doppio clic = ripristino). Le
  proporzioni sono **ricordate per la combinazione di applicazioni** aperte nel layout.
* Cambiare layout non chiude nulla: i pannelli in eccesso restano attivi e nascosti, e
  ricompaiono quando si torna a un layout con più tile.
* **Aggiorna** (pulsante, F5, Ctrl+R) ricarica l'intera scheda / tileset; ogni pannello ha anche
  il proprio pulsante. Avanti/Indietro sono disattivati (Alt+frecce, tasti laterali del mouse).
* Sessione (schede, layout, contenuti) e accessi (cookie) persistono tra un avvio e l'altro.

## Compilazione

Requisiti Windows: Rust (toolchain MSVC) e WebView2 Runtime (già presente su Windows 10/11).

```bash
cargo build --release
```

L'eseguibile è `target\release\ponte.exe`, un file unico con tutte le risorse incluse, xterm.js compreso.
In Linux servono `libwebkit2gtk-4.1-dev` e `libgtk-3-dev`.

## Configurazione

Al primo avvio viene creato `config.json` (⚙ → *Modifica config.json*):

* Windows: `%APPDATA%\Pietroni\Ponte\config\config.json`
* Linux: `~/.config/ponte/config.json`

```json
{
  "apps": [
    {
      "id": "ordo",
      "name": "Ordo",
      "url": "https://ordo.miodominio.it",
      "icon": "📦",
      "allowed_origins": ["https://auth.miodominio.it"],
      "external_links_in_browser": true,
      "permissions": ["messaging", "exec", "fs", "clipboard", "serial", "open"],
      "exec_allow": ["powershell", "notepad", "C:\\Strumenti\\etichettatrice.exe"],
      "fs_roots": ["C:\\Scambio\\Ordo"],
      "zoom": 1.0,
      "user_agent": null,
      "inject_script": "C:\\Ponte\\adattatore-ordo.js"
    }
  ],
  "shells": [
    { "id": "powershell", "name": "PowerShell", "program": "powershell.exe", "args": ["-NoLogo"] },
    { "id": "pwsh", "name": "PowerShell 7", "program": "pwsh.exe", "args": ["-NoLogo"], "cwd": "C:\\Progetti" }
  ],
  "bridge": { "enabled": true, "port": 47800, "token": "generato-automaticamente" },
  "devtools": false
}
```

| Campo | Significato |
|---|---|
| `allowed_origins` | Origini che appartengono all'app: navigazione interna e accesso al bridge. |
| `external_links_in_browser` | `true`: i link ad altri siti si aprono nel browser di sistema. `false`: tutto resta nel pannello (es. SSO), ma il bridge resta limitato alle origini dell'app. |
| `permissions` | Capacità concesse (vedi sotto). Senza permessi l'app può usare solo info e comandi UI. |
| `exec_allow` | Programmi avviabili con `shell.exec` (confronto sul nome file). `"*"` = tutti. |
| `fs_roots` | Cartelle accessibili con `fs.*`. `"*"` = tutto il disco. |
| `inject_script` | JS iniettato in ogni pagina: adattatore per app che non conoscono Ponte. |

Dopo una modifica: ⚙ → *Ricarica configurazione*. I pannelli già aperti mantengono le
impostazioni precedenti finché non vengono riaperti. Le modifiche al bridge WebSocket valgono dal
prossimo avvio.

## API per gli applicativi (`window.ponte`)

Lo script è iniettato in ogni pagina dell'app e risponde **solo alle origini dell'app**.
L'evento `ponte:ready` segnala che è disponibile.

```js
window.addEventListener("ponte:ready", async () => {
  const info = await ponte.info();                       // os, hostname, username, versione…

  // Capacità locali (richiedono il permesso corrispondente)
  const r = await ponte.exec("powershell", ["-Command", "Get-Date"], { timeout_ms: 10000 });
  await ponte.invoke("fs.write", { path: "C:\\Scambio\\Ordo\\ordine.txt", data: "..." });
  const testo = await ponte.invoke("clipboard.read");
  const porte = await ponte.invoke("serial.list");
  const { handle } = await ponte.invoke("serial.open", { port: "COM3", baud: 9600 });
  ponte.on("serial.data", d => console.log(d.handle, d.text, d.base64));
  await ponte.invoke("serial.write", { handle, data: "PESO\r\n" });

  // Messaggistica fra app e con software esterni (permesso "messaging")
  ponte.handle("apriOrdine", async ({ numero }, { from }) => { /* … */ return true; });
  const ok = await ponte.call("metodo", "calcola", { codice: "A1" });   // invoca un'altra app
  ponte.emit("ordine.salvato", { numero: 12 }, "*");   // "*" = tutte le app e i software esterni
  ponte.emit("ordine.salvato", { numero: 12 });        // solo i software esterni
  ponte.on("ordine.nuovo", (data, meta) => console.log(meta.source, data));

  // Interfaccia
  await ponte.invoke("ui.open", { app: "metodo" });          // nuova scheda
  await ponte.invoke("ui.set_title", { title: "Ordine 12" });
  await ponte.invoke("ui.notify", { message: "Stampa completata" });
});
```

| Comando | Permesso | Argomenti |
|---|---|---|
| `system.info` | — | |
| `ui.open`, `ui.set_title`, `ui.focus`, `ui.notify`, `bridge.apps` | — | `{app?, url?}`, `{title}`, `{}`, `{message}` |
| `shell.exec` | `exec` | `{program, args[], cwd?, env?, stdin?, timeout_ms?}` → `{code, stdout, stderr, timed_out}` |
| `env.get` | `exec` | `{name}` |
| `shell.open` | `open` | `{target}`: file, cartella o URL con l'app predefinita |
| `fs.read` / `fs.write` | `fs` | `{path, encoding?: "utf8"\|"base64"}` / `{path, data, encoding?, append?}` |
| `fs.list`, `fs.exists`, `fs.mkdir`, `fs.remove` | `fs` | `{path}` (percorso assoluto, dentro `fs_roots`) |
| `clipboard.read` / `clipboard.write` | `clipboard` | — / `{text}` |
| `serial.list`, `serial.open`, `serial.write`, `serial.close` | `serial` | `{port, baud, data_bits?, parity?, stop_bits?}`, `{handle, data, encoding?}`, `{handle}` |
| `bridge.call`, `bridge.clients` | `messaging` | `{target, method, params}` |

Eventi ricevuti con `ponte.on`: quelli emessi da altre app o software, più `serial.data`,
`serial.closed`, `bridge.client_connected` e `bridge.client_disconnected`. Con `"*"` si ricevono tutti.

## Bridge per software locali (WebSocket)

Software esterni (gestionali, driver, script, servizi) si collegano a
`ws://127.0.0.1:47800/?token=TOKEN&name=nome-client`, oppure inviano il token nell'header
`X-Ponte-Token`. Indirizzo e token sono in ⚙ Impostazioni. Il server accetta solo connessioni
locali. Messaggi JSON:

```jsonc
{"type":"emit", "target":"ordo", "event":"ordine.nuovo", "data":{...}}   // target "*" = tutte
{"type":"call", "id":1, "target":"ordo", "method":"apriOrdine", "params":{...}}
//  → {"type":"result","id":1,"ok":true,"result":...}
{"type":"open", "app":"metodo"}        // oppure {"type":"open","url":"https://..."}
{"type":"focus", "app":"ordo"}
{"type":"list", "id":2}                // schede, pannelli e app configurate
{"type":"reload"}
{"type":"ping"}
```

Il client riceve inoltre `{"type":"event","source":"ordo","event":...,"data":...}` per ogni
`ponte.emit` diretto ai software esterni. Esempio completo in PowerShell:
`examples/cliente_esterno.ps1`. In `examples/demo.html` c'è una pagina di prova di tutte le API.

## Uso

| Azione | Come |
|---|---|
| Nuova scheda | `+` oppure Ctrl+T |
| Cambiare layout | icone in alto a destra |
| Scegliere/cambiare contenuto di un pannello | selettore nel pannello vuoto, oppure ⇄ nell'intestazione |
| Scambiare due pannelli | trascinare l'intestazione di uno sull'altro |
| Rinominare / riordinare / chiudere schede | doppio clic / trascinamento / ✕ o clic centrale |
| Chiudere un pannello | ✕ (se è già vuoto, il tileset si riduce) |
| Navigare tra schede e pannelli | Ctrl+Tab, Ctrl+Shift+Tab, Alt+1…4 |
| Terminale: copia/incolla | Ctrl+Shift+C / Ctrl+Shift+V o tasto destro |
| Riavviare un terminale terminato | Invio, oppure ⟳ nell'intestazione |

## Struttura

```
src/main.rs      finestra, schede/pannelli, IPC, instradamento messaggi
src/layout.rs    geometria dei 7 layout e separatori
src/config.rs    config.json, session.json, sizes.json
src/locals.rs    capacità locali (processi, file, appunti, seriale)
src/pty.rs       terminali (ConPTY su Windows)
src/ws.rs        server WebSocket locale
assets/          shell.html (interfaccia), terminal.html, bridge.js, xterm.js
```

## Sicurezza

* Il bridge risponde solo alle origini dell'app. Un sito esterno aperto nello stesso pannello non
  riceve permessi.
* Ogni capacità va concessa esplicitamente per app. `exec_allow` e `fs_roots` restringono
  ulteriormente l'accesso.
* Il WebSocket ascolta solo su 127.0.0.1 e richiede il token.
