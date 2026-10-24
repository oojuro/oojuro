import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { once } from "node:events";
import { gunzipSync } from "node:zlib";
import { OojuroCaptureClient } from "../index.js";

const id = c => c.repeat(64), token = `ojc_${id("1")}_${"A".repeat(43)}`;
const info = () => ({ format: 1, journal: id("2"), agent_id: id("3"), key_id: id("4"),
  producer_id: id("5"), label: "fixture", expires_us: 999999999999999,
  limits: { events: 100, metadata_bytes: 65536, text_bytes: 16777216 } });
const event = text => ({ eventId: id("a"), runId: id("b"), adapter: "gzip-fixture",
  input: { agent_id: id("3"), parent_id: null, valid_at: 42, kind: "output", tool: null,
    status: null, exit: null, key: null, text, indexed: true, attrs: {} } });
const json = (value, headers = {}) => new Response(JSON.stringify(value), {
  headers: { "content-type": "application/json", ...headers } });
function decode(body, encoding) {
  const raw = encoding === "gzip" ? gunzipSync(body) : Buffer.from(body);
  const n = raw.readUInt32BE(0);
  return { raw, meta: JSON.parse(raw.subarray(4, 4 + n)), text: raw.subarray(4 + n) };
}
function ack(frame) {
  return { request_id: frame.meta.request_id, generation: 1, recorded_bytes: frame.text.length,
    records: frame.meta.events.map(e => ({ event_id: e.event_id, record_id: e.event_id, replayed: false })) };
}
test("capture gzip is negotiated, lossless and owns each mutated transport retry", async () => {
  const text = "synthetic tool output π 🦉\0\n".repeat(4096), wires = [];
  let advertised = true, lose = true;
  const client = new OojuroCaptureClient({ baseUrl: "https://fixture.invalid", token, compression: "auto",
    fetch: async (_url, init) => {
      if (init.method === "GET") return json(info(), advertised ? { "x-oojuro-capture-encoding": "gzip" } : {});
      const encoding = init.headers["content-encoding"];
      const frame = decode(init.body, encoding);
      assert.equal(frame.text.toString(), text);
      assert.equal(Number(init.headers["content-length"]), init.body.length);
      wires.push({ encoding, body: Buffer.from(init.body) });
      init.body.fill(0);
      if (lose) { lose = false; throw Error("ACK lost"); }
      return json(ack(frame));
    } });
  await client.discover();
  const prepared = client.prepareBatch({ requestId: "same_request", events: [event(text)] });
  await assert.rejects(prepared.send(), e => e.outcome === "unknown");
  await prepared.send();
  assert.equal(wires[0].encoding, "gzip");
  assert.deepEqual(wires[0], wires[1]);
  assert.ok(wires[0].body.length < Buffer.byteLength(text) / 20);
  advertised = false;
  await client.discover();
  await prepared.send();
  assert.equal(wires[2].encoding, "identity");
  assert.deepEqual(decode(wires[0].body, "gzip").raw, wires[2].body);
});
test("old discovery, explicit identity, tiny batches and combined capabilities keep identity", async () => {
  for (const [capability, compression, text] of [[null, "auto", "x".repeat(8192)],
    ["gzip", undefined, "x".repeat(8192)],
    ["gzip", "identity", "x".repeat(8192)], ["gzip", "auto", "tiny"],
    ["gzip, gzip", "auto", "x".repeat(8192)]]) {
    const client = new OojuroCaptureClient({ baseUrl: "https://fixture.invalid", token, compression,
      fetch: async (_url, init) => {
        if (init.method === "GET") return json(info(), capability ? { "x-oojuro-capture-encoding": capability } : {});
        assert.equal(init.headers["content-encoding"], "identity");
        return json(ack(decode(init.body, "identity")));
      } });
    await client.discover();
    await client.prepareBatch({ requestId: "fallback", events: [event(text)] }).send();
  }
  assert.throws(() => new OojuroCaptureClient({ baseUrl: "https://fixture.invalid", token, compression: "br" }));
});
test("invalid discovery and abort cannot enable or dispatch compressed writes", async () => {
  let invalid = false, posts = 0;
  const client = new OojuroCaptureClient({ baseUrl: "https://fixture.invalid", token, compression: "auto",
    fetch: async (_url, init) => {
      if (init.method === "GET") return json(invalid ? { ...info(), format: 2 } : info(), { "x-oojuro-capture-encoding": "gzip" });
      posts++;
      assert.equal(init.headers["content-encoding"], "identity");
      return json(ack(decode(init.body, "identity")));
    } });
  await client.discover(); invalid = true;
  await assert.rejects(client.discover());
  const batch = client.prepareBatch({ requestId: "original", events: [event("x".repeat(8192))] });
  await batch.send();
  const controller = new AbortController(); controller.abort();
  await assert.rejects(batch.send({ signal: controller.signal }));
  assert.equal(posts, 1);
});
test("real HTTP capture retries a gzip body after accepted ACK loss", async () => {
  const received = [];
  let lost = false;
  const server = createServer(async (req, res) => {
    assert.equal(req.headers.authorization, `Bearer ${token}`);
    if (req.method === "GET") {
      res.writeHead(200, { "content-type": "application/json", "x-oojuro-capture-encoding": "gzip" });
      res.end(JSON.stringify(info())); return;
    }
    const chunks = []; for await (const chunk of req) chunks.push(chunk);
    const body = Buffer.concat(chunks);
    assert.equal(req.headers["content-encoding"], "gzip");
    assert.equal(Number(req.headers["content-length"]), body.length);
    received.push(body);
    if (!lost) { lost = true; req.socket.destroy(); return; }
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(ack(decode(body, "gzip"))));
  });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  try {
    const client = new OojuroCaptureClient({ baseUrl: `http://127.0.0.1:${server.address().port}`, token, compression: "auto" });
    await client.discover();
    const text = "real loopback exact π\0\n".repeat(8192);
    const prepared = client.prepareBatch({ requestId: "lost_ack", events: [event(text)] });
    await assert.rejects(prepared.send(), e => e.outcome === "unknown");
    assert.equal((await prepared.send()).request_id, "lost_ack");
    assert.deepEqual(received[0], received[1]);
    assert.equal(decode(received[1], "gzip").text.toString(), text);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});
