import assert from "node:assert/strict";
import { test } from "node:test";
import { inspect } from "node:util";
import { mkdtempSync, rmSync, chmodSync, symlinkSync, writeFileSync, statSync } from "node:fs";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import { OojuroCaptureClient, OojuroError } from "../index.js";
import { CaptureSpool } from "../capture.js";
const id = (c) => c.repeat(64), A = id("1"), P = id("2"), K = id("3"), J = id("4"), R = id("5");
const TOKEN = `ojc_${P}_${"A".repeat(43)}`, PRIVATE = "private capture 🦉\0";
const info = () => ({ format: 1, journal: J, agent_id: A, key_id: K, producer_id: P,
  label: "test", expires_us: 999999999999999, limits: { events: 100, metadata_bytes: 65536, text_bytes: 16777216 } });
const event = (c = "a", text = PRIVATE) => ({ eventId: id(c), runId: R, adapter: "sdk-test",
  input: { agent_id: A, parent_id: null, valid_at: 42, kind: "output", tool: null, status: null,
    exit: null, key: null, text, indexed: true, attrs: {} } });
const decode = (bytes) => {
  const b = Buffer.from(bytes), n = b.readUInt32BE(0);
  return { meta: JSON.parse(b.subarray(4, 4 + n)), text: b.subarray(4 + n) };
};
const ack = (wire) => { const { meta } = decode(wire); return { request_id: meta.request_id, generation: 8,
  recorded_bytes: 0, records: meta.events.map((e) => ({ event_id: e.event_id, record_id: e.event_id, replayed: false })) }; };
const json = (body) => new Response(JSON.stringify(body), { headers: { "content-type": "application/json" } });
const client = (fetch, extra = {}) => new OojuroCaptureClient({ baseUrl: "https://fixture.invalid", token: TOKEN, fetch, ...extra });
const error = (code, outcome) => (e) => e instanceof OojuroError && e.code === code && (!outcome || e.outcome === outcome);
async function directory(run) { const path = mkdtempSync("/tmp/oojuro-spool-test-"); try { await run(path); } finally { rmSync(path, { recursive: true, force: true }); } }
test("capture freezes original raw UTF8 bytes and retries privately without cookie/redirect", async () => {
  const wires = [], original = event(); let calls = 0;
  const c = client(async (url, init) => {
    assert.equal(url, "https://fixture.invalid/api/capture/events");
    assert.equal(init.credentials, "omit"); assert.equal(init.redirect, "error"); assert.equal(init.cache, "no-store");
    assert.equal(init.headers.authorization, "Bearer " + TOKEN);
    assert.equal(init.headers["content-type"], "application/vnd.oojuro.capture.v1");
    assert.equal(Number(init.headers["content-length"]), init.body.length);
    wires.push(Buffer.from(init.body));
    const result = ack(init.body); init.body.fill(0);
    if (!calls++) throw new Error(TOKEN + PRIVATE);
    return json(result);
  });
  const prepared = c.prepareBatch({ requestId: "original", events: [original] });
  original.input.text = "changed"; original.eventId = id("b");
  await assert.rejects(prepared.send(), error("transport", "unknown"));
  assert.equal((await prepared.send()).request_id, "original");
  assert.deepEqual(wires[0], wires[1]);
  assert.equal(decode(wires[0]).text.toString(), PRIVATE);
  assert.equal(decode(wires[0]).meta.events[0].input.text, "");
  for (const value of [c, prepared]) for (const text of [JSON.stringify(value), inspect(value)]) {
    assert.ok(!text.includes(TOKEN)); assert.ok(!text.includes(PRIVATE));
  }
});
test("capture bounds raw text and metadata without executing getters", async () => {
  const c = client(async (_url, init) => json(ack(init.body)));
  await c.prepareBatch({ requestId: "maximum", events: [event("a", "\0".repeat(16777216))] }).send();
  for (const text of ["x".repeat(16777217), "\ud800"]) assert.throws(() => c.prepareBatch({ requestId: "bad", events: [event("a", text)] }), error("invalid_request"));
  const e = event(); Object.defineProperty(e.input, "text", { enumerable: true, get() { throw new Error("must not execute"); } });
  assert.throws(() => c.prepareBatch({ requestId: "getter", events: [e] }), error("invalid_request"));
  assert.throws(() => c.prepareBatch({ requestId: "duplicate", events: [event(), event()] }), error("invalid_request"));
  for (const token of ["ojv2_" + "A".repeat(43), TOKEN + "x", TOKEN.slice(0, -1) + "B"]) assert.throws(() => client(null, { token }), error("invalid_configuration"));
});
test("strict discovery includes stable producer; invalid/truncated ACK cannot confirm a write", async () => {
  assert.deepEqual(await client(async () => json(info())).discover(), info());
  const missing = info(); delete missing.producer_id;
  await assert.rejects(client(async () => json(missing)).discover(), error("invalid_response"));
  const cases = [
    (wire) => json({ ...ack(wire), request_id: "wrong" }),
    (wire) => json({ ...ack(wire), records: [] }),
    () => new Response('{"request_id":"test","request_id":"test","generation":8,"recorded_bytes":0,"records":[]}', { headers: { "content-type": "application/json" } }),
    () => new Response("{}", { headers: { "content-type": "application/json", "content-length": "100" } }),
    () => new Response(PRIVATE, { status: 503 }),
  ];
  for (const reply of cases) await assert.rejects(client(async (_url, init) => reply(init.body)).prepareBatch({ requestId: "test", events: [event()] }).send(), (e) => e instanceof OojuroError && e.outcome === "unknown" && !String(e).includes(PRIVATE));
});
test("spool persists uncertain batch, reopens offline, rotates only the same producer and preserves originals", async () => directory(async (path) => {
  let online = true, key = K, producer = P, fail = true; const wires = [];
  const c = client(async (_url, init) => {
    if (!online) throw Error("offline");
    if (init.method === "GET") return json({ ...info(), key_id: key, producer_id: producer });
    wires.push(Buffer.from(init.body));
    if (fail) throw Error("accepted but response lost");
    return json(ack(init.body));
  });
  let s = await CaptureSpool.open({ directory: path, client: c, maxEvents: 2 });
  assert.deepEqual(s.enqueue(event()), { eventId: id("a"), queued: true });
  assert.equal(statSync(path + "/capture.sqlite").mode & 0o777, 0o600);
  await assert.rejects(s.flush(), error("transport", "unknown"));
  assert.equal(s.pending().events, 1); s.close(); online = false;
  s = await CaptureSpool.open({ directory: path, client: c });
  s.enqueue(event("b")); assert.equal(s.pending().events, 2);
  await assert.rejects(s.flush(), error("transport")); online = true;
  producer = id("6"); await assert.rejects(s.flush(), error("spool_binding_mismatch")); assert.equal(s.pending().events, 2);
  producer = P; await assert.rejects(s.flush(), error("transport", "unknown")); assert.deepEqual(wires[0], wires[1]);
  key = id("7"); fail = false;
  assert.deepEqual(await s.flush(), { delivered: 2, remaining: 0 });
  assert.notEqual(decode(wires[0]).meta.request_id, decode(wires[2]).meta.request_id);
  assert.deepEqual(decode(wires[0]).meta.events, decode(wires[2]).meta.events);
  assert.deepEqual(decode(wires[0]).text, decode(wires[2]).text);
  assert.ok(!JSON.stringify(s).includes(PRIVATE)); s.close();
}));
test("spool enforces queue limits, original conflicts, metadata packing and close/drain lifecycle", async () => directory(async (path) => {
  const batches = [];
  const c = client(async (_url, init) => {
    if (init.method === "GET") return json(info());
    batches.push(decode(init.body).meta.events.length); return json(ack(init.body));
  });
  const s = await CaptureSpool.open({ directory: path, client: c, maxEvents: 2, maxBytes: 4096 });
  s.enqueue(event()); s.enqueue(event()); assert.equal(s.pending().events, 1);
  assert.throws(() => s.enqueue(event("a", "changed")), error("event_conflict"));
  s.enqueue(event("b")); assert.throws(() => s.enqueue(event("c")), error("spool_full"));
  await s.flush(); s.close();
  const bigger = await CaptureSpool.open({ directory: path, client: c });
  for (let i = 1; i <= 30; i++) { const e = event(); e.eventId = i.toString(16).padStart(64, "0"); e.input.attrs = { large: "m".repeat(3500) }; bigger.enqueue(e); }
  assert.equal((await bigger.flush()).delivered, 30); assert.ok(batches.length >= 3);
  bigger.close(); assert.throws(() => bigger.enqueue(event()), error("spool_closed"));
}));
test("private spool refuses symlinks, unsafe permissions and unknown databases", async () => directory(async (path) => {
  const c = client(async () => json(info()));
  chmodSync(path, 0o755); await assert.rejects(CaptureSpool.open({ directory: path, client: c }), error("private_spool_required")); chmodSync(path, 0o700);
  writeFileSync(path + "/outside", "private", { mode: 0o600 }); symlinkSync(path + "/outside", path + "/capture.sqlite");
  await assert.rejects(CaptureSpool.open({ directory: path, client: c })); rmSync(path + "/capture.sqlite");
  writeFileSync(path + "/capture.sqlite", "not a database", { mode: 0o600 });
  await assert.rejects(CaptureSpool.open({ directory: path, client: c }), error("spool_io"));
}));
test("SIGKILL after server acceptance recovers the persisted identical request over real HTTP", async () => directory(async (path) => {
  const wires = []; let accept;
  const accepted = new Promise(resolve => { accept = resolve; });
  const sockets = new Set();
  const server = createServer(async (req, res) => {
    if (req.method === "GET") { res.writeHead(200, { "content-type": "application/json" }); res.end(JSON.stringify(info())); return; }
    const chunks = []; for await (const b of req) chunks.push(b); const wire = Buffer.concat(chunks); wires.push(wire);
    if (wires.length === 1) { accept(); return; }
    res.writeHead(200, { "content-type": "application/json" }); res.end(JSON.stringify(ack(wire)));
  });
  server.on("connection", s => { sockets.add(s); s.on("close", () => sockets.delete(s)); });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  const url = `http://127.0.0.1:${server.address().port}`, module = new URL("../capture.js", import.meta.url).href, index = new URL("../index.js", import.meta.url).href;
  const script = `import {CaptureSpool} from ${JSON.stringify(module)}; import {OojuroCaptureClient} from ${JSON.stringify(index)}; const c=new OojuroCaptureClient({baseUrl:${JSON.stringify(url)},token:${JSON.stringify(TOKEN)}});const s=await CaptureSpool.open({directory:${JSON.stringify(path)},client:c});s.enqueue(${JSON.stringify(event())});await s.flush();`;
  const child = spawn(process.execPath, ["--input-type=module", "-e", script], { stdio: "ignore" });
  try {
    await Promise.race([accepted, new Promise((_, reject) => { const t=setTimeout(()=>reject(Error("acceptance timeout")),10000); t.unref(); })]);
    child.kill("SIGKILL"); const [, signal] = await once(child, "exit"); assert.equal(signal, "SIGKILL");
    const s = await CaptureSpool.open({ directory: path, client: client(globalThis.fetch, { baseUrl: url }) });
    assert.equal(s.pending().events, 1); assert.deepEqual(await s.flush(), { delivered: 1, remaining: 0 });
    assert.deepEqual(wires[0], wires[1]); s.close();
  } finally { child.kill("SIGKILL"); for (const socket of sockets) socket.destroy(); await new Promise(resolve=>server.close(resolve)); }
}));
test("concurrent spool handles share one persisted batch and never remove later events", async () => directory(async (path) => {
  const wires = [], waiting = []; let release;
  const gate = new Promise(resolve => { release = resolve; });
  const c = client(async (_url, init) => {
    if (init.method === "GET") return json(info());
    wires.push(Buffer.from(init.body));
    if (wires.length === 1) a.enqueue(event("b"));
    if (wires.length <= 2) { waiting.push(true); if (waiting.length === 2) release(); await gate; }
    return json(ack(init.body));
  });
  const a = await CaptureSpool.open({ directory: path, client: c });
  const b = await CaptureSpool.open({ directory: path, client: c });
  a.enqueue(event());
  const first = a.flush(), second = b.flush();
  await assert.rejects(a.flush(), error("spool_busy")); assert.throws(() => a.close(), error("spool_busy"));
  await Promise.all([first, second]);
  assert.deepEqual(wires[0], wires[1]); assert.equal(a.pending().events, 0);
  assert.notEqual(decode(wires[0]).meta.request_id, decode(wires[2]).meta.request_id);
  a.close(); b.close();
}));
