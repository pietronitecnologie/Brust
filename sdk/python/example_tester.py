"""Example local software built with the Brust Python SDK: a test bench.

A web app starts a test with
    brust.call("tester", "startTest", {"v": 1, "code": "PZ-001", "repetitions": 3}, {timeout_ms: 60000})
receives "test.progress" events while it runs and gets back
    {"code": ..., "outcome": "OK", "measurements": [...]}

    pip install websockets
    BRUST_TOKEN=<token from Brust Settings> python example_tester.py
"""
import argparse
import asyncio
import logging

from brust_client import BrustClient

ap = argparse.ArgumentParser()
ap.add_argument("--token", help="bridge token (default: $BRUST_TOKEN)")
ap.add_argument("--port", type=int, default=47800)
ap.add_argument("--name", default="tester", help="name the apps use to call it")
args = ap.parse_args()

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
client = BrustClient(token=args.token, name=args.name, port=args.port)

busy = asyncio.Lock()  # the instrument runs one test at a time


@client.method("startTest")
async def start_test(params, ctx):
    params = params or {}
    if params.get("v", 1) != 1:
        raise ValueError(f"unsupported version: {params.get('v')}")
    code = params.get("code")
    if not code:
        raise ValueError("missing 'code'")
    repetitions = int(params.get("repetitions", 1))
    if not 1 <= repetitions <= 100:
        raise ValueError("'repetitions' must be between 1 and 100")
    if busy.locked():
        raise RuntimeError("BUSY: test already running")
    async with busy:
        logging.info("test %s x%d requested by %s", code, repetitions, ctx.caller)
        measurements = []
        for i in range(repetitions):
            await asyncio.sleep(1)  # the real measurement goes here
            measurements.append(round(12.0 + i * 0.1, 3))
            await ctx.emit_to_caller("test.progress", {
                "code": code, "step": i + 1, "percent": round((i + 1) * 100 / repetitions),
            })
        return {"code": code, "outcome": "OK", "measurements": measurements}


@client.method("status")
async def status(params, ctx):
    return {"busy": busy.locked()}


@client.on_connect
def connected(welcome):
    logging.info("connected to Brust %s, apps: %s", welcome.get("version"), welcome.get("apps"))


@client.on("*")
def any_event(data, meta):
    logging.info("event %s from %s: %s", meta["event"], meta["source"], data)


if __name__ == "__main__":
    client.run()
