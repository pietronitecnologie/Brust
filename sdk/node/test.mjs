// Self-test for brust-client.mjs:  node sdk/node/test.mjs
//
// Runs the client (with Node's real global WebSocket) against a tiny fake Brust
// bridge: a minimal RFC 6455 server over `net` that handles text, close and ping frames.
import net from "node:net";
import crypto from "node:crypto";
import assert from "node:assert/strict";
import { BrustClient, BrustError, TimeoutError } from "./brust-client.mjs";

const TOKEN = "secret-token";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/* ------------------------------------------------ minimal WebSocket server */

class FakeConn {
  constructor(socket, query) {
    this.socket = socket;
    this.query = query;
    this.buf = Buffer.alloc(0);
    this.closed = false;
    this.onmessage = () => {};
    this.onclose = () => {};
    socket.on("data", (d) => this._data(d));
    socket.on("close", () => this._closed());
    socket.on("error", () => {});
  }
  _frame(opcode, payload) {
    const len = payload.length;
    let head;
    if (len < 126) head = Buffer.from([0x80 | opcode, len]);
    else if (len < 65536) { head = Buffer.alloc(4); head[0] = 0x80 | opcode; head[1] = 126; head.writeUInt16BE(len, 2); }
    else { head = Buffer.alloc(10); head[0] = 0x80 | opcode; head[1] = 127; head.writeBigUInt64BE(BigInt(len), 2); }
    return Buffer.concat([head, payload]);
  }
  send(obj) {
    if (!this.closed) this.socket.write(this._frame(1, Buffer.from(JSON.stringify(obj))));
  }
  close() {
    if (this.closed) return;
    this.socket.write(this._frame(8, Buffer.from([0x03, 0xe8])));
    this.socket.end();
    this._closed();
  }
  _closed() {
    if (this.closed) return;
    this.closed = true;
    this.onclose();
  }
  _data(d) {
    this.buf = Buffer.concat([this.buf, d]);
    for (;;) {
      if (this.buf.length < 2) return;
      const opcode = this.buf[0] & 0x0f;
      const masked = (this.buf[1] & 0x80) !== 0;
      let len = this.buf[1] & 0x7f;
      let off = 2;
      if (len === 126) { if (this.buf.length < 4) return; len = this.buf.readUInt16BE(2); off = 4; }
      else if (len === 127) { if (this.buf.length < 10) return; len = Number(this.buf.readBigUInt64BE(2)); off = 10; }
      const maskLen = masked ? 4 : 0;
      if (this.buf.length < off + maskLen + len) return;
      const mask = masked ? this.buf.subarray(off, off + 4) : null;
      const payload = Buffer.from(this.buf.subarray(off + maskLen, off + maskLen + len));
      if (mask) for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i & 3];
      this.buf = this.buf.subarray(off + maskLen + len);
      if (opcode === 1) this.onmessage(JSON.parse(payload.toString("utf8"))); // (no fragmentation in tests)
      else if (opcode === 8) { this.close(); return; }
      else if (opcode === 9) this.socket.write(this._frame(10, payload));
    }
  }
}

class FakeBrust {
  constructor() {
    this.conn = null;
    this.connections = 0;
    this.received = [];
    this.waiters = [];
    this.autoAnswer = true;
    this.callHandlers = {};
    this.server = net.createServer((s) => this._accept(s));
  }
  listen(port = 0) {
    return new Promise((r) => this.server.listen(port, "127.0.0.1", () => { this.port = this.server.address().port; r(this); }));
  }
  stop() {
    this.conn?.close();
    return new Promise((r) => this.server.close(() => r()));
  }
  _accept(socket) {
    let head = "";
    const onData = (d) => {
      head += d.toString("latin1");
      const end = head.indexOf("\r\n\r\n");
      if (end < 0) return;
      socket.off("data", onData);
      const lines = head.slice(0, end).split("\r\n");
      const path = lines[0].split(" ")[1];
      const headers = Object.fromEntries(lines.slice(1).map((l) => { const i = l.indexOf(":"); return [l.slice(0, i).trim().toLowerCase(), l.slice(i + 1).trim()]; }));
      const query = new URL(path, "http://x").searchParams;
      if (query.get("token") !== TOKEN) {
        socket.end("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        return;
      }
      const accept = crypto.createHash("sha1").update(headers["sec-websocket-key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest("base64");
      socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
      const conn = new FakeConn(socket, query);
      this.connections++;
      this.conn = conn;
      this.name = query.get("name");
      conn.onmessage = (m) => this._message(conn, m);
      conn.onclose = () => { if (this.conn === conn) this.conn = null; };
      const rest = Buffer.from(head.slice(end + 4), "latin1");
      if (rest.length) conn._data(rest);
      conn.send({ type: "welcome", client: this.connections, version: "test", apps: ["crm"] });
    };
    socket.on("data", onData);
    socket.on("error", () => {});
  }
  _message(conn, m) {
    this.received.push(m);
    this.waiters.splice(0).forEach((w) => w());
    if (!this.autoAnswer) return;
    const id = m.id;
    if (m.type === "call") {
      const fn = this.callHandlers[m.method];
      conn.send(fn ? { type: "result", id, ok: true, result: fn(m.params) } : { type: "result", id, ok: false, error: `'${m.target}' is not open` });
    } else if (m.type === "list") {
      conn.send({ type: "result", id, ok: true, result: { tabs: [], apps: [{ id: "crm" }] } });
    } else if (m.type === "emit" || m.type === "open") {
      conn.send({ type: "result", id, ok: true, result: null });
    } else if (m.type === "focus") {
      conn.send({ type: "result", id, ok: false, error: `app '${m.app}' is not open` });
    }
  }
  send(m) { this.conn.send(m); }
  /** Next received message of the given type (consumes it). */
  async next(type, timeoutMs = 3000) {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const i = this.received.findIndex((m) => !type || m.type === type);
      if (i >= 0) return this.received.splice(i, 1)[0];
      const left = deadline - Date.now();
      if (left <= 0) throw new Error(`timeout waiting for '${type}'`);
      await new Promise((r) => { this.waiters.push(r); setTimeout(r, left); });
    }
  }
}

async function until(cond, timeoutMs = 3000) {
  const deadline = Date.now() + timeoutMs;
  while (!cond()) {
    if (Date.now() > deadline) throw new Error("condition not met in time");
    await sleep(20);
  }
}

/* ------------------------------------------------------------------ tests */

const tests = [];
const test = (name, fn) => tests.push({ name, fn });

async function withClient(fn) {
  const fake = await new FakeBrust().listen();
  const client = new BrustClient({ token: TOKEN, name: "tester", port: fake.port, minBackoffMs: 50, maxBackoffMs: 200 });
  client.start();
  try {
    await client.waitConnected(3000);
    await until(() => client.welcome);
    await fn(fake, client);
  } finally {
    await client.stop();
    await fake.stop();
  }
}

test("method call gets one reply, with progress events to the caller", () => withClient(async (fake, client) => {
  assert.equal(fake.name, "tester");
  client.method("startTest", async (params, ctx) => {
    assert.equal(ctx.caller, "crm");
    for (let i = 0; i < params.repetitions; i++) await ctx.emitToCaller("test.progress", { code: params.code, step: i + 1 });
    return { outcome: "OK", measurements: Array(params.repetitions).fill(1.5) };
  });
  fake.send({ type: "call", id: 7, method: "startTest", params: { code: "PZ-001", repetitions: 2 }, from: "crm" });
  const reply = await fake.next("reply");
  assert.deepEqual(reply, { type: "reply", id: 7, ok: true, result: { outcome: "OK", measurements: [1.5, 1.5] } });
  const e1 = await fake.next("emit");
  const e2 = await fake.next("emit");
  assert.equal(e1.target, "crm");
  assert.equal(e1.event, "test.progress");
  assert.equal(e2.data.step, 2);
  await sleep(50);
  assert.equal(fake.received.filter((m) => m.type === "reply").length, 0, "exactly one reply");
}));

test("handler errors and unknown methods reply ok:false", () => withClient(async (fake, client) => {
  client.method("fail", () => { throw new Error("instrument busy"); });
  client.method("bad", async () => ({ n: 10n })); // BigInt is not JSON
  fake.send({ type: "call", id: 1, method: "fail", params: {}, from: "crm" });
  assert.deepEqual(await fake.next("reply"), { type: "reply", id: 1, ok: false, error: "instrument busy" });
  fake.send({ type: "call", id: 2, method: "nope", from: "crm" });
  const r2 = await fake.next("reply");
  assert.equal(r2.ok, false);
  assert.match(r2.error, /unknown method: nope/);
  fake.send({ type: "call", id: 3, method: "bad", from: "crm" });
  const r3 = await fake.next("reply");
  assert.equal(r3.id, 3);
  assert.equal(r3.ok, false);
}));

test("long handlers do not block other calls", () => withClient(async (fake, client) => {
  let release;
  const gate = new Promise((r) => (release = r));
  client.method("slow", async () => { await gate; return "slow"; });
  client.method("fast", () => "fast");
  fake.send({ type: "call", id: 10, method: "slow", from: "crm" });
  fake.send({ type: "call", id: 11, method: "fast", from: "crm" });
  assert.deepEqual((await fake.next("reply")).result, "fast");
  release();
  const r = await fake.next("reply");
  assert.equal(r.id, 10);
  assert.equal(r.result, "slow");
}));

test("client.call / list / open / focus", () => withClient(async (fake, client) => {
  fake.callHandlers.sum = (p) => p.a + p.b;
  assert.equal(await client.call("crm", "sum", { a: 2, b: 40 }), 42);
  const c1 = await fake.next("call");
  assert.equal(c1.target, "crm");
  await assert.rejects(client.call("erp", "missing", {}), (e) => e instanceof BrustError && /is not open/.test(e.message));
  const c2 = await fake.next("call");
  assert.ok(c2.id > c1.id, "incrementing ids");
  assert.deepEqual((await client.list()).apps, [{ id: "crm" }]);
  assert.equal(await client.open({ app: "erp" }), null);
  assert.equal((await fake.next("open")).app, "erp");
  await assert.rejects(client.focus("erp"), BrustError);
}));

test("call timeout", () => withClient(async (fake, client) => {
  fake.autoAnswer = false;
  await assert.rejects(client.call("crm", "never", {}, { timeoutMs: 150 }), TimeoutError);
  assert.equal(client._pending.size, 0);
}));

test("emit and incoming events", () => withClient(async (fake, client) => {
  const got = [];
  client.on("order.saved", (data, meta) => got.push(["saved", data, meta]));
  const off = client.on("*", (data, meta) => got.push(["*", data, meta]));
  await client.emit("scale.weight", { kg: 1.5 });
  const m = await fake.next("emit");
  assert.deepEqual([m.event, m.target, m.data], ["scale.weight", "*", { kg: 1.5 }]);
  await client.emit("x", null, "crm", { wait: true });
  assert.equal((await fake.next("emit")).target, "crm");
  fake.send({ type: "event", event: "order.saved", data: { number: 12 }, source: "crm" });
  await until(() => got.length === 2);
  assert.deepEqual(got.map((g) => g[0]).sort(), ["*", "saved"]);
  for (const [, data, meta] of got) {
    assert.deepEqual(data, { number: 12 });
    assert.deepEqual(meta, { event: "order.saved", source: "crm" });
  }
  off();
  fake.send({ type: "event", event: "other", data: 1, source: "crm" });
  await sleep(100);
  assert.equal(got.length, 2);
}));

test("reconnects after a disconnect; pending calls fail immediately", () => withClient(async (fake, client) => {
  const welcomes = [];
  client.onConnect((w) => welcomes.push(w.client));
  fake.autoAnswer = false;
  const pending = client.call("crm", "never", {}, { timeoutMs: 5000 });
  await fake.next("call");
  fake.conn.close();
  await assert.rejects(pending, /connection to Brust lost/);
  await until(() => fake.connections === 2 && client.connected);
  await until(() => welcomes.length === 1);
  assert.deepEqual(welcomes, [2]);
  fake.autoAnswer = true;
  client.method("ping", () => "pong");
  fake.send({ type: "call", id: 99, method: "ping", from: "crm" });
  assert.equal((await fake.next("reply")).result, "pong");
}));

test("connects when the bridge starts later", async () => {
  const probe = await new FakeBrust().listen();
  const port = probe.port;
  await probe.stop();
  const client = new BrustClient({ token: TOKEN, name: "tester", port, minBackoffMs: 50, maxBackoffMs: 100 });
  client.start();
  await sleep(300);
  assert.equal(client.connected, false);
  const fake = await new FakeBrust().listen(port);
  try {
    await client.waitConnected(3000);
    assert.equal(fake.connections, 1);
  } finally {
    await client.stop();
    await fake.stop();
  }
});

test("token from BRUST_TOKEN and URL encoding", () => {
  const prev = process.env.BRUST_TOKEN;
  process.env.BRUST_TOKEN = "from env";
  try {
    const c = new BrustClient({ name: "a b" });
    assert.equal(c.url, "ws://127.0.0.1:47800/?token=from%20env&name=a%20b");
    delete process.env.BRUST_TOKEN;
    assert.throws(() => new BrustClient({ name: "x" }), /token missing/);
  } finally {
    if (prev === undefined) delete process.env.BRUST_TOKEN; else process.env.BRUST_TOKEN = prev;
  }
});

let failed = 0;
for (const t of tests) {
  try {
    await t.fn();
    console.log(`ok   ${t.name}`);
  } catch (e) {
    failed++;
    console.log(`FAIL ${t.name}\n     ${e && e.stack}`);
  }
}
console.log(`\n${tests.length - failed} passed, ${failed} failed`);
process.exit(failed ? 1 : 0);
