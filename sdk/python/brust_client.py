"""Brust bridge client for local software (Python 3.9+, asyncio).

Connects to the Brust local WebSocket bridge, exposes methods that web apps can
call with ``brust.call(name, method, params)``, emits and receives events, and
calls methods registered by web apps with ``brust.handle``.

    pip install websockets

    from brust_client import BrustClient

    client = BrustClient(name="tester")          # token from $BRUST_TOKEN

    @client.method("startTest")
    async def start_test(params, ctx):
        await ctx.emit_to_caller("test.progress", {"percent": 50})
        return {"outcome": "OK"}

    @client.on("order.saved")
    def order_saved(data, meta):
        print(meta["source"], data)

    client.run()

Protocol: see README.md / GUIDE.md of Brust ("Bridge for local software").
"""
from __future__ import annotations

import asyncio
import inspect
import itertools
import json
import logging
import os
import random
from typing import Any, Awaitable, Callable, Dict, List, Optional, Set
from urllib.parse import quote

import websockets
from websockets.exceptions import ConnectionClosed, InvalidHandshake

__all__ = ["BrustClient", "CallContext", "BrustError"]
__version__ = "1.0.0"

log = logging.getLogger("brust")

Handler = Callable[[Any, "CallContext"], Any]
Listener = Callable[[Any, Dict[str, Any]], Any]


class BrustError(Exception):
    """An error returned by Brust or by the remote side of a call."""


class CallContext:
    """Context passed to a method handler."""

    def __init__(self, client: "BrustClient", call_id: Any, method: str, caller: str):
        self.client = client
        self.id = call_id
        self.method = method
        #: The ``from`` field of the call: an app id (``"crm"``) or ``"external:<name>"``.
        self.caller = caller

    async def emit_to_caller(self, event: str, data: Any = None) -> None:
        """Sends an event to the app (or program) that made the call."""
        await self.client.emit(event, data, target=self.caller or "*")

    def __repr__(self) -> str:
        return f"CallContext(id={self.id!r}, method={self.method!r}, caller={self.caller!r})"


async def _maybe_await(value: Any) -> Any:
    if inspect.isawaitable(value):
        return await value
    return value


class BrustClient:
    """Client for the Brust WebSocket bridge.

    :param token: bridge token (Brust -> Settings). Defaults to ``$BRUST_TOKEN``.
    :param name: client name; web apps call this program with ``brust.call(name, ...)``.
    :param port: bridge port (default 47800).
    :param host: bridge host (Brust only listens on 127.0.0.1).
    :param reconnect: reconnect automatically when the connection drops.
    :param min_backoff, max_backoff: reconnection delay bounds, in seconds.
    """

    def __init__(
        self,
        token: Optional[str] = None,
        name: str = "client",
        port: int = 47800,
        host: str = "127.0.0.1",
        *,
        reconnect: bool = True,
        min_backoff: float = 0.5,
        max_backoff: float = 15.0,
    ):
        token = token or os.environ.get("BRUST_TOKEN")
        if not token:
            raise ValueError("Brust token missing: pass token=... or set BRUST_TOKEN")
        self.token = token
        self.name = name
        self.port = int(port)
        self.host = host
        self.reconnect = reconnect
        self.min_backoff = min_backoff
        self.max_backoff = max_backoff

        self._methods: Dict[str, Handler] = {}
        self._listeners: Dict[str, List[Listener]] = {}
        self._connect_hooks: List[Callable[[Dict[str, Any]], Any]] = []
        self._ws = None
        self._connected = asyncio.Event() if _has_running_loop() else None
        self._pending: Dict[int, asyncio.Future] = {}
        self._ids = itertools.count(1)
        self._tasks: Set[asyncio.Task] = set()
        self._stopping = False
        self._send_lock: Optional[asyncio.Lock] = None
        #: The last ``welcome`` message received (``{"type":"welcome","client":3,...}``).
        self.welcome: Optional[Dict[str, Any]] = None

    # ------------------------------------------------------------ registration

    @property
    def url(self) -> str:
        return (
            f"ws://{self.host}:{self.port}/?token={quote(self.token, safe='')}"
            f"&name={quote(self.name, safe='')}"
        )

    @property
    def connected(self) -> bool:
        return self._ws is not None

    def method(self, name: str) -> Callable[[Handler], Handler]:
        """Decorator: registers ``fn(params, ctx)`` (sync or async) as method ``name``."""

        def deco(fn: Handler) -> Handler:
            self._methods[name] = fn
            return fn

        return deco

    def add_method(self, name: str, fn: Handler) -> None:
        self._methods[name] = fn

    def on(self, event: str) -> Callable[[Listener], Listener]:
        """Decorator: registers ``fn(data, meta)`` for ``event`` (``"*"`` = all events).

        ``meta`` is ``{"event": ..., "source": ...}``.
        """

        def deco(fn: Listener) -> Listener:
            self._listeners.setdefault(event, []).append(fn)
            return fn

        return deco

    def off(self, event: str, fn: Listener) -> None:
        try:
            self._listeners.get(event, []).remove(fn)
        except ValueError:
            pass

    def on_connect(self, fn: Callable[[Dict[str, Any]], Any]) -> Callable:
        """Decorator: ``fn(welcome)`` runs after every (re)connection."""
        self._connect_hooks.append(fn)
        return fn

    # ---------------------------------------------------------------- requests

    async def wait_connected(self, timeout: Optional[float] = None) -> None:
        self._ensure_loop_state()
        if self._ws is not None:
            return
        try:
            await asyncio.wait_for(self._connected.wait(), timeout)
        except asyncio.TimeoutError:
            raise ConnectionError("not connected to Brust") from None

    async def emit(self, event: str, data: Any = None, target: Optional[str] = "*", *, wait: bool = False) -> None:
        """Sends an event to an app (``target`` = app id) or to all apps (``"*"``).

        With ``wait=True`` waits for Brust to acknowledge it.
        """
        msg = {"type": "emit", "target": target if target is not None else "*", "event": event, "data": data}
        if wait:
            await self._request(msg, timeout=10)
        else:
            if self._ws is None:
                raise ConnectionError("not connected to Brust")
            msg["id"] = next(self._ids)  # the acknowledgement is ignored
            await self._send(msg)

    async def call(self, target: str, method: str, params: Any = None, timeout: Optional[float] = 30) -> Any:
        """Calls ``method`` registered with ``brust.handle`` by the app ``target``.

        Returns the result or raises :class:`BrustError` (``asyncio.TimeoutError`` on timeout).
        """
        return await self._request(
            {"type": "call", "target": target, "method": method, "params": params}, timeout=timeout
        )

    async def list(self, timeout: Optional[float] = 10) -> Any:
        """Tabs, panes and configured apps."""
        return await self._request({"type": "list"}, timeout=timeout)

    async def open(self, app: Optional[str] = None, url: Optional[str] = None, timeout: Optional[float] = 10) -> Any:
        """Opens an app (optionally at ``url``) or a web address in a new tab."""
        if not app and not url:
            raise ValueError("specify app or url")
        msg: Dict[str, Any] = {"type": "open"}
        if app:
            msg["app"] = app
        if url:
            msg["url"] = url
        return await self._request(msg, timeout=timeout)

    async def focus(self, app: str, timeout: Optional[float] = 10) -> Any:
        """Brings an open app to the front."""
        return await self._request({"type": "focus", "app": app}, timeout=timeout)

    async def reload(self, timeout: Optional[float] = 10) -> Any:
        """Reloads the active tab."""
        return await self._request({"type": "reload"}, timeout=timeout)

    async def _request(self, msg: Dict[str, Any], timeout: Optional[float]) -> Any:
        self._ensure_loop_state()
        loop = asyncio.get_running_loop()
        deadline = None if timeout is None else loop.time() + timeout
        await self.wait_connected(timeout)
        call_id = next(self._ids)
        fut: asyncio.Future = loop.create_future()
        self._pending[call_id] = fut
        try:
            await self._send(dict(msg, id=call_id))
            remaining = None if deadline is None else max(0.0, deadline - loop.time())
            try:
                return await asyncio.wait_for(fut, remaining)
            except asyncio.TimeoutError:
                what = msg.get("method") or msg["type"]
                raise asyncio.TimeoutError(f"timeout: no reply to {what!r} in {timeout} s") from None
        finally:
            self._pending.pop(call_id, None)

    async def _send(self, msg: Dict[str, Any]) -> None:
        ws = self._ws
        if ws is None:
            raise ConnectionError("not connected to Brust")
        text = json.dumps(msg)
        async with self._send_lock:
            await ws.send(text)

    # --------------------------------------------------------------- main loop

    def run(self) -> None:
        """Blocking: connects and serves until interrupted (Ctrl+C)."""
        try:
            asyncio.run(self.serve())
        except KeyboardInterrupt:
            pass

    async def serve(self) -> None:
        """Connects, dispatches messages and reconnects with backoff until :meth:`close`."""
        self._ensure_loop_state()
        self._stopping = False
        backoff = self.min_backoff
        while not self._stopping:
            try:
                async with websockets.connect(self.url, max_size=None, open_timeout=10) as ws:
                    backoff = self.min_backoff
                    await self._session(ws)
            except asyncio.CancelledError:
                raise
            except (OSError, ConnectionClosed, asyncio.TimeoutError, InvalidHandshake) as e:
                if not self._stopping:
                    log.warning("Brust unreachable (%s)", e)
            finally:
                self._disconnected()
            if self._stopping or not self.reconnect:
                break
            delay = backoff * (0.8 + 0.4 * random.random())
            log.info("reconnecting in %.1f s", delay)
            await asyncio.sleep(delay)
            backoff = min(backoff * 2, self.max_backoff)

    async def close(self) -> None:
        """Stops :meth:`serve` and closes the connection."""
        self._stopping = True
        ws = self._ws
        if ws is not None:
            await ws.close()

    async def _session(self, ws) -> None:
        self._ws = ws
        self._connected.set()
        log.info("connected to Brust as %r", self.name)
        async for raw in ws:
            try:
                msg = json.loads(raw)
            except ValueError:
                log.warning("invalid JSON from Brust: %r", raw[:200])
                continue
            if isinstance(msg, dict):
                self._dispatch(msg)

    def _disconnected(self) -> None:
        self._ws = None
        if self._connected is not None:
            self._connected.clear()
        for fut in self._pending.values():
            if not fut.done():
                fut.set_exception(ConnectionError("connection to Brust lost"))
        self._pending.clear()

    def _dispatch(self, msg: Dict[str, Any]) -> None:
        kind = msg.get("type")
        if kind == "call":
            self._spawn(self._handle_call(msg))
        elif kind == "result" or kind == "pong":
            fut = self._pending.get(msg.get("id"))
            if fut is not None and not fut.done():
                if kind == "pong" or msg.get("ok", True):
                    fut.set_result(msg.get("result"))
                else:
                    fut.set_exception(BrustError(msg.get("error") or "error"))
        elif kind == "event":
            event = msg.get("event") or ""
            meta = {"event": event, "source": msg.get("source")}
            for name in (event, "*"):
                for fn in list(self._listeners.get(name, ())):
                    self._spawn(self._run_listener(fn, msg.get("data"), meta))
        elif kind == "welcome":
            self.welcome = msg
            for fn in list(self._connect_hooks):
                self._spawn(self._run_listener(fn, msg))
        elif kind == "error":
            log.warning("Brust error: %s", msg.get("error"))

    def _spawn(self, coro: Awaitable) -> None:
        task = asyncio.ensure_future(coro)
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)

    async def _run_listener(self, fn: Callable, *args: Any) -> None:
        try:
            await _maybe_await(fn(*args))
        except Exception:
            log.exception("error in Brust listener %r", fn)

    async def _handle_call(self, msg: Dict[str, Any]) -> None:
        call_id = msg.get("id")
        method = msg.get("method") or ""
        ctx = CallContext(self, call_id, method, msg.get("from") or "*")
        try:
            fn = self._methods.get(method)
            if fn is None:
                raise BrustError(f"unknown method: {method}")
            result = await _maybe_await(fn(msg.get("params"), ctx))
            reply = {"type": "reply", "id": call_id, "ok": True, "result": result}
            text = json.dumps(reply)  # fail here (ok:false) if the result is not JSON
        except asyncio.CancelledError:
            await self._send_reply_quietly({"type": "reply", "id": call_id, "ok": False, "error": "cancelled"})
            raise
        except Exception as e:  # the error reaches the app as a rejected Promise
            log.debug("method %s failed", method, exc_info=True)
            await self._send_reply_quietly(
                {"type": "reply", "id": call_id, "ok": False, "error": str(e) or type(e).__name__}
            )
            return
        await self._send_reply_quietly(json.loads(text))

    async def _send_reply_quietly(self, reply: Dict[str, Any]) -> None:
        try:
            await self._send(reply)
        except Exception as e:
            log.warning("could not reply to call %s: %s", reply.get("id"), e)

    def _ensure_loop_state(self) -> None:
        # asyncio primitives must be created inside the running loop.
        if self._connected is None:
            self._connected = asyncio.Event()
        if self._send_lock is None:
            self._send_lock = asyncio.Lock()


def _has_running_loop() -> bool:
    try:
        asyncio.get_running_loop()
        return True
    except RuntimeError:
        return False
