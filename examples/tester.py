"""Example of local software connected to Brust (e.g. a test bench).

Receives the "startTest" command from a web app, reports progress with events
and returns the outcome as the reply to the call.

    pip install websockets
    python tester.py --token <token from Brust Settings>
"""
import argparse
import asyncio
import json

import websockets


async def start_test(ws, params, caller):
    """The real logic goes here: instruments, PLCs, serial ports, other programs…"""
    code = params.get("code")
    repetitions = int(params.get("repetitions", 1))
    measurements = []
    for i in range(repetitions):
        await asyncio.sleep(1)  # simulate a measurement
        measurements.append(12.0 + i * 0.1)
        # Intermediate progress: event sent to the app that requested the test.
        await ws.send(json.dumps({
            "type": "emit", "target": caller, "event": "test.progress",
            "data": {"code": code, "step": i + 1, "percent": round((i + 1) * 100 / repetitions)},
        }))
    return {"code": code, "outcome": "OK", "measurements": measurements}


METHODS = {"startTest": start_test}


async def handle_call(ws, msg):
    method = METHODS.get(msg["method"])
    caller = msg.get("from", "*")
    try:
        if method is None:
            raise ValueError(f"unknown method: {msg['method']}")
        result = await method(ws, msg.get("params") or {}, caller)
        reply = {"type": "reply", "id": msg["id"], "ok": True, "result": result}
    except Exception as e:  # the error reaches the app as a rejected Promise
        reply = {"type": "reply", "id": msg["id"], "ok": False, "error": str(e)}
    await ws.send(json.dumps(reply))


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--token", required=True)
    ap.add_argument("--port", type=int, default=47800)
    ap.add_argument("--name", default="tester", help="name the apps use to call it")
    args = ap.parse_args()
    url = f"ws://127.0.0.1:{args.port}/?token={args.token}&name={args.name}"
    while True:  # reconnects if Brust is restarted
        try:
            async with websockets.connect(url) as ws:
                print("connected to Brust as", args.name)
                async for raw in ws:
                    msg = json.loads(raw)
                    if msg.get("type") == "call":
                        print("command received:", msg["method"], msg.get("params"), "from", msg.get("from"))
                        asyncio.create_task(handle_call(ws, msg))
                    elif msg.get("type") == "event":
                        print("event:", msg.get("event"), msg.get("data"))
        except (OSError, websockets.ConnectionClosed) as e:
            print("Brust unreachable, retrying in 3 s:", e)
            await asyncio.sleep(3)


if __name__ == "__main__":
    asyncio.run(main())
