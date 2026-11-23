"""Esempio di software locale collegato a Brust (es. un banco di collaudo).

Riceve il comando "startTest" da un'app web, invia l'avanzamento con eventi
e restituisce l'esito come risposta alla chiamata.

    pip install websockets
    python tester.py --token <token da Impostazioni di Brust>
"""
import argparse
import asyncio
import json

import websockets


async def start_test(ws, params, caller):
    """Qui va la logica reale: strumenti, PLC, porte seriali, altri programmi…"""
    codice = params.get("codice")
    ripetizioni = int(params.get("ripetizioni", 1))
    misure = []
    for i in range(ripetizioni):
        await asyncio.sleep(1)  # simula una misura
        misure.append(12.0 + i * 0.1)
        # Avanzamento intermedio: evento verso l'app che ha chiesto il test.
        await ws.send(json.dumps({
            "type": "emit", "target": caller, "event": "test.progress",
            "data": {"codice": codice, "step": i + 1, "percent": round((i + 1) * 100 / ripetizioni)},
        }))
    return {"codice": codice, "esito": "OK", "misure": misure}


METODI = {"startTest": start_test}


async def gestisci_chiamata(ws, msg):
    metodo = METODI.get(msg["method"])
    caller = msg.get("from", "*")
    try:
        if metodo is None:
            raise ValueError(f"metodo sconosciuto: {msg['method']}")
        risultato = await metodo(ws, msg.get("params") or {}, caller)
        risposta = {"type": "reply", "id": msg["id"], "ok": True, "result": risultato}
    except Exception as e:  # l'errore arriva all'app come Promise rifiutata
        risposta = {"type": "reply", "id": msg["id"], "ok": False, "error": str(e)}
    await ws.send(json.dumps(risposta))


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--token", required=True)
    ap.add_argument("--port", type=int, default=47800)
    ap.add_argument("--name", default="tester", help="nome con cui le app lo chiamano")
    args = ap.parse_args()
    url = f"ws://127.0.0.1:{args.port}/?token={args.token}&name={args.name}"
    while True:  # si ricollega se Brust viene riavviato
        try:
            async with websockets.connect(url) as ws:
                print("collegato a Brust come", args.name)
                async for raw in ws:
                    msg = json.loads(raw)
                    if msg.get("type") == "call":
                        print("comando ricevuto:", msg["method"], msg.get("params"), "da", msg.get("from"))
                        asyncio.create_task(gestisci_chiamata(ws, msg))
                    elif msg.get("type") == "event":
                        print("evento:", msg.get("event"), msg.get("data"))
        except (OSError, websockets.ConnectionClosed) as e:
            print("Brust non raggiungibile, nuovo tentativo tra 3 s:", e)
            await asyncio.sleep(3)


if __name__ == "__main__":
    asyncio.run(main())
