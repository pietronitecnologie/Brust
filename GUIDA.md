# Guida: progettare web app e software locali per Brust

Questa guida spiega come costruire applicativi web e programmi locali che usano Brust come ponte
fra il browser e il PC. Per l'elenco completo dei comandi vedi `README.md`.

## 1. I tre attori e i tre tipi di messaggio

```
 ┌──────────────┐   window.brust    ┌──────────────┐   WebSocket 127.0.0.1   ┌──────────────────┐
 │   Web app    │ ◄───────────────► │    Brust     │ ◄─────────────────────► │ Software locale  │
 │ (Ordo, …)    │   (iniettato)     │   (client)   │   token + nome client   │ (tester, driver) │
 └──────────────┘                   └──────────────┘                         └──────────────────┘
        id app: "ordo"                                                         nome: "tester"
```

Ogni partecipante ha un **nome**:

* le web app hanno l'`id` definito in `config.json` (es. `ordo`);
* i software locali hanno il nome con cui si collegano (`?name=tester`).

Brust instrada i messaggi in base a quel nome. Esistono tre tipi di messaggio:

| Tipo | A cosa serve | Web app | Software locale |
|---|---|---|---|
| **Comando locale** (`invoke`) | Brust stesso esegue un'azione sul PC: processi, file, seriale, appunti | `brust.invoke("fs.read", …)` | — |
| **Chiamata** (`call`) | Richiesta → **una** risposta (o errore). Per "fai questo e dimmi com'è andata" | `brust.call(nome, metodo, params)` · `brust.handle(metodo, fn)` | riceve `{"type":"call"}` → risponde `{"type":"reply"}`; invia `{"type":"call"}` → riceve `{"type":"result"}` |
| **Evento** (`emit`/`on`) | Notifica senza risposta: avanzamento, cambi di stato, allarmi | `brust.emit(evento, dati, target)` · `brust.on(evento, fn)` | invia `{"type":"emit"}` · riceve `{"type":"event"}` |

Una chiamata verso un nome viene recapitata alla web app aperta con quell'`id` (preferendo la scheda
attiva). Se nessuna app ha quell'`id`, va al software locale collegato con quel nome. Con il
prefisso `external:` (es. `"external:tester"`) si forza il software locale.

## 2. Esempio completo: avvio di un test

```
Web app (ordo)                     Brust                          Software locale (tester)
     │ brust.call("tester","startTest",{codice})                            │
     │──────────────────────────────►│ {"type":"call","id":7,"method":"startTest",
     │                               │  "params":{…},"from":"ordo"} ───────►│
     │                               │                                      │ esegue il test
     │                               │◄── {"type":"emit","target":"ordo",   │
     │◄── brust.on("test.progress")  │     "event":"test.progress",…}       │ (0..n volte)
     │                               │                                      │
     │                               │◄── {"type":"reply","id":7,"ok":true, │
     │◄── la Promise si risolve      │     "result":{"esito":"OK",…}}       │
     │    con l'esito                │                                      │
```

**Web app**

```js
brust.on("test.progress", d => mostraAvanzamento(d.percent));
try {
  const esito = await brust.call("tester", "startTest", { codice: "PZ-001" }, { timeout_ms: 120000 });
  mostraEsito(esito);                 // { codice, esito: "OK", misure: [...] }
} catch (e) {
  mostraErrore(e.message);            // software non collegato, errore del test, timeout, disconnessione
}
```

**Software locale** (Python, completo in `examples/tester.py`)

```python
async for raw in ws:
    msg = json.loads(raw)
    if msg["type"] == "call" and msg["method"] == "startTest":
        # ... lavoro, con eventi di avanzamento verso msg["from"] ...
        await ws.send(json.dumps({"type": "emit", "target": msg["from"],
                                  "event": "test.progress", "data": {"percent": 50}}))
        await ws.send(json.dumps({"type": "reply", "id": msg["id"], "ok": True,
                                  "result": {"esito": "OK"}}))
```

Il "comando derivato" si costruisce qui. Brust inoltra i dati così come sono; la traduzione
(es. da `{codice}` al protocollo dello strumento) va fatta **nel software locale**, che conosce
l'hardware. In alternativa, la web app trasforma i dati prima di chiamare.

Se invece è il software locale a dover avviare qualcosa nella web app, il flusso è speculare:
la web app registra `brust.handle("apriOrdine", fn)` e il software invia
`{"type":"call","id":1,"target":"ordo","method":"apriOrdine","params":{…}}`, ricevendo
`{"type":"result","id":1,…}`.

## 3. Strutturare la web app

### 3.1 Un solo modulo di integrazione

Tutto il codice che usa `window.brust` va in **un solo modulo** (es. `src/integrazione/brust.js`).
Il resto dell'app chiama funzioni di dominio come `avviaTest()`, non `brust.call` direttamente.
Così la stessa app funziona sia in Brust sia in un browser normale.

```js
// integrazione/brust.js
const inBrust = () => typeof window !== "undefined" && !!window.brust;

export function pronto() {
  return new Promise(res => {
    if (inBrust()) return res(true);
    window.addEventListener("brust:ready", () => res(true), { once: true });
    setTimeout(() => res(inBrust()), 1500);     // in un browser normale: false
  });
}

// Stato dei software locali (per abilitare/disabilitare i pulsanti)
const collegati = new Set();
export async function avviaMonitor(onChange) {
  if (!(await pronto())) return;
  (await brust.invoke("bridge.clients")).forEach(c => collegati.add(c.name));
  brust.on("bridge.client_connected",    d => { collegati.add(d.name);    onChange(); });
  brust.on("bridge.client_disconnected", d => { collegati.delete(d.name); onChange(); });
  onChange();
}
export const disponibile = nome => collegati.has(nome);

// API di dominio
export async function avviaTest(codice, onProgress) {
  if (!inBrust()) throw new Error("Funzione disponibile solo nel client Brust");
  const stop = brust.on("test.progress", d => d.codice === codice && onProgress(d));
  try {
    return await brust.call("tester", "startTest", { v: 1, codice }, { timeout_ms: 120000 });
  } finally { stop(); }
}
```

### 3.2 Regole pratiche

1. **Il server resta la fonte di verità.** Brust trasporta comandi; non salva dati di business.
   Quando arriva l'esito di un test, la web app lo invia al proprio backend come qualsiasi altro dato.
2. **Interfaccia consapevole dello stato.** Disabilita "Avvia test" se `disponibile("tester")` è
   falso, mostra l'avanzamento e gestisci sempre l'errore della Promise.
3. **Timeout sempre** sulle chiamate verso hardware (`timeout_ms`). Se il software si disconnette,
   Brust rifiuta subito la chiamata; il timeout copre i software bloccati.
4. **Identificativi di correlazione.** Includi nei parametri un id del tuo dominio (codice pezzo,
   id ordine) e filtra gli eventi con quello, come nell'esempio: più schede possono essere aperte.
5. **Permessi minimi** in `config.json`. Spesso basta `messaging`. `exec`/`fs` solo se servono,
   con `exec_allow` e `fs_roots` stretti.
6. **Comandi in ingresso con `brust.handle`.** Valida sempre i parametri: arrivano da un altro
   programma.
7. **App esistenti che non si possono modificare**: si adattano con `inject_script`. È un file JS
   locale iniettato nelle pagine, che può leggere il DOM, aggiungere pulsanti e chiamare `brust`.

### 3.3 Quando usare cosa

| Esigenza | Soluzione consigliata |
|---|---|
| Lanciare un programma a riga di comando e leggerne l'output | `brust.exec(...)` (permesso `exec`). Fai stampare JSON su stdout al programma. |
| Dialogare con uno strumento seriale semplice | `serial.open/write` + evento `serial.data` |
| Operazioni lunghe, con stato, hardware complesso, SDK proprietari | **software locale** collegato al bridge (`call` + eventi) |
| Due web app che collaborano (Ordo ↔ Metodo) | `brust.handle` in una, `brust.call` nell'altra |
| Notificare più destinatari | `brust.emit(evento, dati, "*")` |
| Leggere/scrivere file di scambio con altri programmi | `fs.*` (permesso `fs` + `fs_roots`) |

## 4. Strutturare il software locale

### 4.1 Struttura consigliata

```
software-locale/
  main            connessione al bridge, riconnessione, smistamento messaggi
  metodi/         una funzione per metodo: startTest, stopTest, stato, …
  hardware/       driver, protocolli, SDK: nessuna dipendenza da Brust
  config          porta, token, nome client
```

* **Il nome è il contratto.** Le app chiamano `brust.call("tester", …)`. Scegli un nome stabile e
  configurabile, e non collegare due istanze con lo stesso nome (vince l'ultima collegata).
* **Riconnessione automatica.** Brust può essere riavviato: ritenta ogni pochi secondi, come in
  `examples/tester.py`. Il software può partire prima di Brust (es. come servizio o all'avvio).
* **Tabella dei metodi.** `{"startTest": fn, "stato": fn, …}`: un metodo sconosciuto risponde
  con un errore, senza ignorare la richiesta.
* **Rispondi sempre, una sola volta**, con `{"type":"reply","id":<id ricevuto>,"ok":…}`. Le
  eccezioni diventano `ok:false` con un messaggio leggibile dall'operatore.
* **Lavoro lungo in parallelo.** Esegui il metodo in un task o thread separato, così il software
  continua a ricevere messaggi (es. `stopTest`).
* **Concorrenza esplicita.** Se lo strumento gestisce un test alla volta, rispondi subito
  `ok:false, error:"strumento occupato"`, senza accodare richieste in silenzio.
* **Avanzamento con eventi** verso `msg["from"]` (l'app chiamante), con lo stesso id di dominio.
* **Versioning.** Includi `v` nei parametri (`{"v":1,…}`) e rifiuta versioni sconosciute.
  Aggiungi campi senza rompere quelli esistenti.
* **Token fuori dal codice.** Leggilo da un file di configurazione o da una variabile d'ambiente.
  Il token è in Brust → ⚙ Impostazioni (anche in `config.json` → `bridge.token`).

### 4.2 Riferimento messaggi lato software

Connessione: `ws://127.0.0.1:47800/?token=TOKEN&name=NOME`

| Ricevuti dal software | Significato |
|---|---|
| `{"type":"welcome","client":3,"apps":[…]}` | connessione accettata |
| `{"type":"call","id":7,"method":"…","params":{…},"from":"ordo"}` | un'app chiede qualcosa: rispondi con `reply` |
| `{"type":"event","event":"…","data":{…},"source":"ordo"}` | evento emesso da un'app verso i software esterni o `"*"` |
| `{"type":"result","id":…,"ok":…,"result"/"error":…}` | risposta a un tuo `call`, `list`, `emit`, `open`, … |

| Inviati dal software | Significato |
|---|---|
| `{"type":"reply","id":7,"ok":true,"result":{…}}` | risposta a una `call` ricevuta (`ok:false,"error":"…"` per errori) |
| `{"type":"emit","target":"ordo","event":"…","data":{…}}` | evento verso un'app (`"*"` = tutte) |
| `{"type":"call","id":1,"target":"ordo","method":"…","params":{…}}` | chiama un metodo registrato da un'app con `brust.handle` |
| `{"type":"open","app":"metodo"}`, `{"type":"focus","app":"ordo"}` | apre o porta in primo piano un'app |
| `{"type":"list","id":2}` | schede, pannelli e app configurate |

### 4.3 Altri linguaggi

Qualsiasi linguaggio con un client WebSocket va bene. Lo schema è sempre lo stesso: connetti,
leggi in ciclo, smista per `type`, rispondi con `reply`. Lo scheletro C#/.NET qui sotto è uno
schema di partenza e **non è stato compilato**.

```csharp
using System.Net.WebSockets; using System.Text; using System.Text.Json; using System.Text.Json.Nodes;

var ws = new ClientWebSocket();
await ws.ConnectAsync(new Uri($"ws://127.0.0.1:47800/?token={token}&name=tester"), default);
var buf = new byte[65536];
while (ws.State == WebSocketState.Open) {
    var r = await ws.ReceiveAsync(buf, default);           // messaggi > 64 KB: accumulare fino a EndOfMessage
    var msg = JsonNode.Parse(Encoding.UTF8.GetString(buf, 0, r.Count))!;
    if ((string?)msg["type"] != "call") continue;
    _ = Task.Run(async () => {
        JsonObject reply;
        try {
            var result = await Esegui((string)msg["method"]!, msg["params"], (string?)msg["from"]);
            reply = new() { ["type"] = "reply", ["id"] = msg["id"]!.GetValue<long>(), ["ok"] = true, ["result"] = result };
        } catch (Exception e) {
            reply = new() { ["type"] = "reply", ["id"] = msg["id"]!.GetValue<long>(), ["ok"] = false, ["error"] = e.Message };
        }
        await Invia(ws, reply);   // proteggere SendAsync con un lock: un solo invio alla volta
    });
}
```

In PowerShell c'è un esempio funzionante lato client in `examples/cliente_esterno.ps1`.

## 5. Convenzioni consigliate

* **Eventi**: `dominio.azione` al passato o progressivo: `test.progress`, `test.completed`,
  `ordine.salvato`, `bilancia.peso`.
* **Metodi**: verbo all'imperativo, camelCase: `startTest`, `stopTest`, `apriOrdine`, `stato`.
* **Parametri e risultati**: oggetti JSON, mai valori nudi, così si possono estendere. Includi `v`.
* **Errori**: messaggi comprensibili all'operatore. Se servono codici macchina, usa
  `"error":"OCCUPATO: test già in corso"`.
* **Documenta il contratto** di ogni software (metodi, parametri, eventi) in un file accanto al codice.

## 6. Collaudo e diagnosi

* `examples/demo.html` + `examples/tester.py`: prova completa del flusso dell'esempio
  (`python -m http.server 3000` nella cartella `examples`, app con url `http://localhost:3000/demo.html`).
* Con `"devtools": true` in `config.json` ogni pannello ha il pulsante `</>` (console JS).
* Per simulare un software locale si può usare un client WebSocket generico (es. `wscat`):

```bash
wscat -c "ws://127.0.0.1:47800/?token=TOKEN&name=tester"
```

## 7. Checklist

- [ ] App in `config.json` con `id`, `url`, eventuali `allowed_origins` e i soli permessi necessari
- [ ] Web app: un solo modulo di integrazione, funziona anche fuori da Brust
- [ ] Web app: pulsanti legati alla disponibilità del software (`bridge.clients` + eventi)
- [ ] Web app: `timeout_ms`, gestione errori, id di correlazione negli eventi
- [ ] Software: nome stabile, riconnessione, token da configurazione
- [ ] Software: tabella metodi, sempre una `reply`, lavoro lungo asincrono, gestione "occupato"
- [ ] Contratto (metodi, parametri, eventi, versione) documentato
