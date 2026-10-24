#!/usr/bin/env node
// Synthetic transport probe only: no Cloud credentials, persistence or deployment.
import assert from "node:assert/strict";
import { createServer } from "node:https";
import { readFileSync } from "node:fs";
import { once } from "node:events";
import { randomBytes, createHash } from "node:crypto";
import { gunzipSync } from "node:zlib";
import { performance } from "node:perf_hooks";
import { OojuroCaptureClient } from "../sdk/typescript/index.js";

const id = c => c.repeat(64), token = `ojc_${id("1")}_${"A".repeat(43)}`;
const info = { format: 1, journal: id("2"), agent_id: id("3"), key_id: id("4"),
  producer_id: id("5"), label: "synthetic", expires_us: 999999999999999,
  limits: { events: 100, metadata_bytes: 65536, text_bytes: 16777216 } };
const event = text => ({ eventId: id("a"), runId: id("b"), adapter: "transport-probe",
  input: { agent_id: id("3"), parent_id: null, valid_at: 42, kind: "output", tool: null,
    status: null, exit: null, key: null, text, indexed: true, attrs: {} } });
const decode = (body, encoding) => {
  const raw = encoding === "gzip" ? gunzipSync(body) : Buffer.from(body);
  const n = raw.readUInt32BE(0);
  return { raw, meta: JSON.parse(raw.subarray(4, 4 + n)), text: raw.subarray(4 + n) };
};
const ack = f => ({ request_id: f.meta.request_id, generation: 1, recorded_bytes: f.text.length,
  records: f.meta.events.map(e => ({ event_id: e.event_id, record_id: e.event_id, replayed: false })) });
if (process.argv[2] === "--fixture") {
  let fixture;
  const client = new OojuroCaptureClient({ baseUrl: "https://fixture.invalid", token, compression: "auto",
    fetch: async (_url, init) => {
      if (init.method === "GET") return new Response(JSON.stringify(info), {
        headers: { "content-type": "application/json", "x-oojuro-capture-encoding": "gzip" } });
      assert.equal(init.headers["content-encoding"], "gzip");
      const f = decode(init.body, "gzip");
      fixture = { schema: 1, source: "actual OojuroCaptureClient gzip send; synthetic only", node: process.version,
        sdk_sha256: createHash("sha256").update(readFileSync(new URL("../sdk/typescript/index.js", import.meta.url))).digest("hex"),
        raw_base64: f.raw.toString("base64"), gzip_base64: Buffer.from(init.body).toString("base64"),
        text: f.text.toString() };
      return new Response(JSON.stringify(ack(f)), { headers: { "content-type": "application/json" } });
    } });
  await client.discover();
  await client.prepareBatch({ requestId: "cross_language_original", events: [event("fixture π 🦉\0\n".repeat(128))] }).send();
  console.log(JSON.stringify(fixture, null, 2));
} else {
  assert.notEqual(process.env.NODE_TLS_REJECT_UNAUTHORIZED, "0", "TLS verification must remain enabled");
  const [certPath, keyPath, runsText = "12"] = process.argv.slice(2);
  const runs = Number(runsText);
  if (!certPath || !keyPath || !Number.isInteger(runs) || runs < 4 || runs > 100) {
    throw Error("Usage: NODE_EXTRA_CA_CERTS=ca.crt node scripts/bench-capture-transport.mjs server.crt server.key [runs:4..100]");
  }
  let expected, last, connections = 0;
  const server = createServer({ cert: readFileSync(certPath), key: readFileSync(keyPath), minVersion: "TLSv1.2" }, async (req, res) => {
    try {
      assert.equal(req.headers.authorization, `Bearer ${token}`);
      let value;
      if (req.method === "GET") value = info;
      else {
        const chunks = []; for await (const chunk of req) chunks.push(chunk);
        const wire = Buffer.concat(chunks);
        assert.equal(wire.length, Number(req.headers["content-length"]));
        const f = decode(wire, req.headers["content-encoding"]);
        assert.deepEqual(f.text, expected);
        last = { bytes: wire.length, encoding: req.headers["content-encoding"] };
        value = ack(f);
      }
      res.writeHead(200, { "content-type": "application/json", "x-oojuro-capture-encoding": "gzip" });
      res.end(JSON.stringify(value));
    } catch { res.writeHead(400); res.end(); }
  });
  server.on("secureConnection", () => connections++);
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  const baseUrl = `https://localhost:${server.address().port}`;
  const clients = Object.fromEntries(["identity", "auto"].map(compression => [compression,
    new OojuroCaptureClient({ baseUrl, token, compression })]));
  const results = [];
  try {
    for (const client of Object.values(clients)) await client.discover();
    for (const kind of ["repeated-tool-output", "random-printable"]) {
      for (const size of [128, 8192, 131072, 1048576]) {
        const text = kind === "repeated-tool-output"
          ? "src/example.rs:42 synthetic check passed; output π\n".repeat(Math.ceil(size / 40)).slice(0, size)
          : randomBytes(size).toString("base64").slice(0, size);
        expected = Buffer.from(text);
        const samples = { identity: [], auto: [] }, sizes = { identity: [], auto: [] }, encodings = {};
        for (let i = -3; i < runs; i++) {
          // Alternate order to reduce systematic warmup/order bias.
          for (const mode of i % 2 ? ["identity", "auto"] : ["auto", "identity"]) {
            const start = performance.now();
            await clients[mode].prepareBatch({ requestId: `probe_${kind}_${size}_${i + 3}`, events: [event(text)] }).send();
            const elapsed = performance.now() - start;
            if (i >= 0) { samples[mode].push(elapsed); sizes[mode].push(last.bytes); }
            encodings[mode] = last.encoding;
          }
        }
        const percentile = (values, q) => [...values].sort((a, b) => a - b)[Math.ceil(q * values.length) - 1];
        results.push({ kind, text_bytes: expected.length, runs, modes: Object.fromEntries(Object.keys(samples).map(mode => [mode, {
          encoding: encodings[mode], wire_body_bytes_min: Math.min(...sizes[mode]), wire_body_bytes_max: Math.max(...sizes[mode]),
          p50_ms: percentile(samples[mode], 0.5), p95_ms: percentile(samples[mode], 0.95) }])) });
      }
    }
    console.log(JSON.stringify({ schema: 1, node: process.version, transport: "verified HTTPS loopback; reused default fetch connections",
      tls_connections: connections, complete_acknowledgments: 2 * 8 * (runs + 3), exact_originals: true,
      limits: ["synthetic transport fixture, not Oojuro Cloud", "no disk durability, PostgreSQL, S3, KMS, WAN delay or capacity claim",
        "shared development host; timing is indicative", "p95 is descriptive with few samples; no deployment throughput or latency claim"], results }, null, 2));
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
}
