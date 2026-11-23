"""Tests for brust_client against an in-process fake Brust bridge.

    python3 -m pytest -q sdk/python

No pytest plugins are needed: each test runs its own event loop.
"""
import asyncio
import json
import os
import sys
from urllib.parse import parse_qs, urlparse

import pytest
import websockets

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from brust_client import BrustClient, BrustError  # noqa: E402

TOKEN = "secret-token"


class FakeBrust:
    """Minimal imitation of the Brust WebSocket bridge."""

    def __init__(self):
        self.server = None
        self.port = None
        self.conn = None
        self.connections = 0
        self.received = asyncio.Queue()
        self.connected = asyncio.Event()
        self.call_handlers = {}  # method -> fn(params) for client.call

    async def start(self):
        self.server = await websockets.serve(self._handler, "127.0.0.1", 0)
        self.port = self.server.sockets[0].getsockname()[1]
        return self

    async def stop(self):
        self.server.close()
        await self.server.wait_closed()

    async def _handler(self, conn):
        q = parse_qs(urlparse(conn.request.path).query)
        if q.get("token", [""])[0] != TOKEN:
            await conn.close(4001, "invalid token")
            return
        self.connections += 1
        self.conn = conn
        self.name = q.get("name", [""])[0]
        await conn.send(json.dumps({"type": "welcome", "client": self.connections, "version": "test", "apps": ["crm"]}))
        self.connected.set()
        try:
            async for raw in conn:
                msg = json.loads(raw)
                await self.received.put(msg)
                await self._auto_answer(conn, msg)
        except websockets.ConnectionClosed:
            pass
        finally:
            if self.conn is conn:
                self.conn = None
                self.connected.clear()

    async def _auto_answer(self, conn, msg):
        t = msg.get("type")
        mid = msg.get("id")
        if t == "call":
            fn = self.call_handlers.get(msg["method"])
            if fn is None:
                out = {"type": "result", "id": mid, "ok": False, "error": f"'{msg['target']}' is not open"}
            else:
                out = {"type": "result", "id": mid, "ok": True, "result": fn(msg.get("params"))}
            await conn.send(json.dumps(out))
        elif t == "list":
            await conn.send(json.dumps({"type": "result", "id": mid, "ok": True, "result": {"tabs": [], "apps": [{"id": "crm"}]}}))
        elif t in ("emit", "open"):
            await conn.send(json.dumps({"type": "result", "id": mid, "ok": True, "result": None}))
        elif t == "focus":
            await conn.send(json.dumps({"type": "result", "id": mid, "ok": False, "error": f"app '{msg['app']}' is not open"}))

    async def send(self, msg):
        await self.conn.send(json.dumps(msg))

    async def next(self, type_=None, timeout=3):
        while True:
            msg = await asyncio.wait_for(self.received.get(), timeout)
            if type_ is None or msg.get("type") == type_:
                return msg


def run(coro_fn):
    """Starts a fake server and a client, runs the test body, cleans up."""

    async def main():
        fake = await FakeBrust().start()
        client = BrustClient(TOKEN, "tester", port=fake.port, min_backoff=0.05, max_backoff=0.2)
        task = asyncio.create_task(client.serve())
        try:
            await asyncio.wait_for(fake.connected.wait(), 3)
            await client.wait_connected(3)
            await coro_fn(fake, client)
        finally:
            await client.close()
            await asyncio.wait_for(task, 3)
            await fake.stop()

    asyncio.run(main())


def test_method_call_gets_reply_with_progress_events():
    async def body(fake, client):
        @client.method("startTest")
        async def start_test(params, ctx):
            assert ctx.caller == "crm"
            for i in range(params["repetitions"]):
                await ctx.emit_to_caller("test.progress", {"code": params["code"], "step": i + 1})
            return {"outcome": "OK", "measurements": [1.0] * params["repetitions"]}

        await fake.send({"type": "call", "id": 7, "method": "startTest",
                         "params": {"code": "PZ-001", "repetitions": 2}, "from": "crm"})
        e1 = await fake.next()
        e2 = await fake.next()
        assert [e1["type"], e2["type"]] == ["emit", "emit"]
        assert e1["target"] == "crm" and e1["event"] == "test.progress" and e2["data"]["step"] == 2
        reply = await fake.next("reply")
        assert reply == {"type": "reply", "id": 7, "ok": True,
                         "result": {"outcome": "OK", "measurements": [1.0, 1.0]}}
        assert fake.name == "tester"

    run(body)


def test_handler_error_and_unknown_method_give_ok_false():
    async def body(fake, client):
        @client.method("fail")
        def fail(params, ctx):  # sync handlers work too
            raise RuntimeError("instrument busy")

        @client.method("bad")
        async def bad(params, ctx):
            return {"x": object()}  # not JSON-serializable

        await fake.send({"type": "call", "id": 1, "method": "fail", "params": {}, "from": "crm"})
        r = await fake.next("reply")
        assert r == {"type": "reply", "id": 1, "ok": False, "error": "instrument busy"}

        await fake.send({"type": "call", "id": 2, "method": "nope", "params": None, "from": "crm"})
        r = await fake.next("reply")
        assert r["id"] == 2 and r["ok"] is False and "unknown method: nope" in r["error"]

        await fake.send({"type": "call", "id": 3, "method": "bad", "params": None, "from": "crm"})
        r = await fake.next("reply")
        assert r["id"] == 3 and r["ok"] is False

    run(body)


def test_long_handler_does_not_block_others():
    async def body(fake, client):
        release = asyncio.Event()

        @client.method("slow")
        async def slow(params, ctx):
            await release.wait()
            return "slow"

        @client.method("fast")
        async def fast(params, ctx):
            return "fast"

        await fake.send({"type": "call", "id": 10, "method": "slow", "from": "crm"})
        await fake.send({"type": "call", "id": 11, "method": "fast", "from": "crm"})
        r = await fake.next("reply")
        assert (r["id"], r["result"]) == (11, "fast")
        release.set()
        r = await fake.next("reply")
        assert (r["id"], r["result"]) == (10, "slow")

    run(body)


def test_client_call_list_open_focus():
    async def body(fake, client):
        fake.call_handlers["sum"] = lambda p: p["a"] + p["b"]
        assert await client.call("crm", "sum", {"a": 2, "b": 40}) == 42
        msg = await fake.next("call")
        assert msg["target"] == "crm" and msg["method"] == "sum" and isinstance(msg["id"], int)

        with pytest.raises(BrustError, match="is not open"):
            await client.call("erp", "missing", {})

        # ids are distinct and incrementing
        ids = [msg["id"], (await fake.next("call"))["id"]]
        assert ids[1] > ids[0]

        assert (await client.list())["apps"] == [{"id": "crm"}]
        assert await client.open(app="erp") is None
        assert (await fake.next("open"))["app"] == "erp"
        with pytest.raises(BrustError):
            await client.focus("erp")

    run(body)


def test_call_timeout():
    async def body(fake, client):

        async def silent(conn, msg):
            return None

        fake._auto_answer = silent
        with pytest.raises(asyncio.TimeoutError):
            await client.call("crm", "never", {}, timeout=0.2)
        assert client._pending == {}

    run(body)


def test_emit_and_events():
    async def body(fake, client):
        got = asyncio.Queue()

        @client.on("order.saved")
        async def saved(data, meta):
            await got.put(("saved", data, meta))

        @client.on("*")
        def anything(data, meta):
            got.put_nowait(("*", data, meta))

        await client.emit("scale.weight", {"kg": 1.5})
        m = await fake.next("emit")
        assert (m["event"], m["target"], m["data"]) == ("scale.weight", "*", {"kg": 1.5})
        await client.emit("x", None, target="crm", wait=True)
        m = await fake.next("emit")
        assert m["target"] == "crm"

        await fake.send({"type": "event", "event": "order.saved", "data": {"number": 12}, "source": "crm"})
        items = [await asyncio.wait_for(got.get(), 2) for _ in range(2)]
        kinds = sorted(i[0] for i in items)
        assert kinds == ["*", "saved"]
        for _, data, meta in items:
            assert data == {"number": 12} and meta == {"event": "order.saved", "source": "crm"}

    run(body)


def test_reconnect_after_disconnect():
    async def body(fake, client):
        welcomes = []
        client.on_connect(lambda w: welcomes.append(w["client"]))
        assert client.welcome["client"] == 1

        # A pending call fails immediately when the connection drops.
        async def silent(conn, msg):
            return None

        fake._auto_answer = silent
        pending = asyncio.create_task(client.call("crm", "never", {}, timeout=5))
        await fake.next("call")
        await fake.conn.close()
        with pytest.raises(ConnectionError):
            await asyncio.wait_for(pending, 2)

        # The client comes back on its own.
        for _ in range(100):
            if fake.connections >= 2 and client.connected:
                break
            await asyncio.sleep(0.05)
        assert fake.connections == 2
        await client.wait_connected(2)
        for _ in range(40):
            if welcomes:
                break
            await asyncio.sleep(0.05)
        assert welcomes == [2]

        @client.method("ping")
        async def ping(params, ctx):
            return "pong"

        await fake.send({"type": "call", "id": 99, "method": "ping", "from": "crm"})
        assert (await fake.next("reply"))["result"] == "pong"

    run(body)


def test_reconnects_when_server_starts_later():
    async def main():
        fake = FakeBrust()
        # Reserve a port, then start the server on it after the client is already retrying.
        tmp = await websockets.serve(lambda c: None, "127.0.0.1", 0)
        port = tmp.sockets[0].getsockname()[1]
        tmp.close()
        await tmp.wait_closed()
        client = BrustClient(TOKEN, "tester", port=port, min_backoff=0.05, max_backoff=0.1)
        task = asyncio.create_task(client.serve())
        await asyncio.sleep(0.3)
        assert not client.connected
        fake.server = await websockets.serve(fake._handler, "127.0.0.1", port)
        try:
            await client.wait_connected(3)
            assert fake.connections == 1
        finally:
            await client.close()
            await asyncio.wait_for(task, 3)
            await fake.stop()

    asyncio.run(main())


def test_token_from_env(monkeypatch):
    monkeypatch.setenv("BRUST_TOKEN", "from env")
    c = BrustClient(name="a b")
    assert c.token == "from env"
    assert c.url == "ws://127.0.0.1:47800/?token=from%20env&name=a%20b"
    monkeypatch.delenv("BRUST_TOKEN")
    with pytest.raises(ValueError):
        BrustClient(name="x")
