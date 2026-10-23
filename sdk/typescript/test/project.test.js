import test from "node:test";
import assert from "node:assert/strict";
import { inspect } from "node:util";
import { OojuroProjectClient, OojuroCaptureClient } from "../index.js";
const id = c => c.repeat(64), token = "ojp_" + "secret_".repeat(7);
const error = code => e => e.code === code && e.outcome === "none";
const descriptor = () => ({ id: id("a"), text_bytes: 16, generation: 4,
  capture: { event_id: id("b"), run_id: id("c") },
  record: { schema: 1, id: 1, writer: id("d"), adapter: "fixture", recorded_at: 20,
    agent_id: id("e"), parent_id: null, valid_at: 15, kind: "output", tool: null,
    status: null, exit: null, key: null, attrs: {}, indexed: true } });
const page = () => ({ snapshot: id("f"), records: [{ journal: id("1"), record: descriptor() }], next: "opaque_cursor-1" });
const json = value => new Response(JSON.stringify(value), { headers: { "content-type": "application/json" } });
const client = transport => new OojuroProjectClient({ baseUrl: "https://example.invalid", project: id("2"), token, fetch: transport });

test("project adapter keeps cursors/bearers out of URLs and sends no cookies or native generation", async () => {
  const requests = [];
  const c = client(async (url, init) => { requests.push({ url, init }); return json(page()); });
  const selection = { from: 10, through: 30, agentId: id("e"), limit: 1 };
  const first = await c.records(selection);
  const second = await c.records({ ...selection, cursor: first.next, limit: 2 });
  assert.equal(second.snapshot, first.snapshot);
  const { url, init } = requests[1];
  assert.equal(url, `https://example.invalid/api/projects/${id("2")}/records`);
  assert.equal(init.method, "POST"); assert.equal(init.credentials, "omit");
  assert.equal(init.redirect, "error"); assert.equal(init.cache, "no-store");
  assert.equal(init.referrerPolicy, "no-referrer");
  assert.deepEqual(init.headers, { authorization: `Bearer ${token}`, "content-type": "application/x-www-form-urlencoded" });
  assert.equal(new URLSearchParams(init.body).get("cursor"), first.next);
  assert.equal(new URLSearchParams(init.body).get("agent_id"), id("e"));
  assert.ok(!JSON.stringify(c).includes(token)); assert.ok(!inspect(c).includes(token));
});

test("project and capture credentials cannot be interchanged; invalid selectors make no network call", async () => {
  for (const bad of ["ojc_" + id("3") + "_" + "A".repeat(43), "ojv2_" + "A".repeat(43), "ojp_x", "ojp_" + "x".repeat(101)]) {
    assert.throws(() => new OojuroProjectClient({ baseUrl: "https://example.invalid", project: id("2"), token: bad }), e => e.code === "invalid_configuration");
  }
  assert.throws(() => new OojuroCaptureClient({ baseUrl: "https://example.invalid", token }), e => e.code === "invalid_configuration");
  for (const url of ["http://example.invalid", "https://secret@example.invalid", "https://example.invalid/path", "https://example.invalid/?key=secret"]) {
    assert.throws(() => new OojuroProjectClient({ baseUrl: url, project: id("2"), token }), e => e.code === "invalid_configuration");
  }
  let calls = 0;
  const c = client(async () => { calls++; throw Error("must not call"); });
  for (const input of [{ limit: 0 }, { limit: 1001 }, { from: 1.5 }, { from: Number.MAX_SAFE_INTEGER + 1 },
    { from: 2, through: 1 }, { agentId: "../" }, { cursor: "bad cursor" }, { query: "unsupported" },
    { get from() { throw Error("getter executed"); } }, { cursor: "a".repeat(2 * 1024 * 1024) }]) {
    await assert.rejects(c.records(input), error("invalid_request"));
  }
  await assert.rejects(c.content({ journal: "../", recordId: id("a") }), error("invalid_request"));
  assert.equal(calls, 0);
});

test("metadata rejects malformed/ambiguous schemas, foreign selections and unsafe numeric lexemes", async () => {
  const cases = [
    p => { p.generation = 9; },
    p => { p.records.push(p.records[0]); },
    p => { p.records[0].record.record.text = "unexpected payload"; },
    p => { p.records[0].record.capture.run_id = "bad"; },
    p => { p.records[0].record.text_bytes = 16 * 1024 * 1024 + 1; },
    p => { p.records[0].record.record.agent_id = id("9"); },
    p => { p.records[0].record.record.valid_at = 31; },
    p => { p.records = []; },
    p => { p.next = "not/a/cursor"; },
  ];
  for (const change of cases) {
    const p = page(); change(p);
    await assert.rejects(client(async () => json(p)).records({ from: 10, through: 30, agentId: id("e"), limit: 1 }), error("invalid_response"));
  }
  for (const wire of [
    JSON.stringify(page()).replace('"generation":4', '"generation":4.0000000000000001'),
    JSON.stringify(page()).replace('"generation":4', '"generation":4,"gener\\u0061tion":4'),
    JSON.stringify(page()).replace('"text_bytes":16', '"text_bytes":9007199254740993'),
  ]) await assert.rejects(client(async () => new Response(wire, { headers: { "content-type": "application/json" } })).records(), error("invalid_response"));
  const old = page(); delete old.records[0].record.capture; old.next = null;
  assert.deepEqual(await client(async () => json(old)).records(), old);
});

test("content preserves exact 16 MiB UTF8/BOM/NUL, and refuses partial or oversized results", async () => {
  const text = "\ufeffα🦉\0" + "x".repeat(16 * 1024 * 1024 - 10);
  assert.equal(Buffer.byteLength(text), 16 * 1024 * 1024);
  const c = client(async (url, init) => {
    assert.equal(url, `https://example.invalid/api/projects/${id("2")}/journals/${id("1")}/records/${id("a")}/content`);
    assert.equal(init.method, "GET"); assert.ok(!Object.hasOwn(init, "body"));
    return new Response(text, { headers: { "content-type": "text/plain; charset=utf-8" } });
  });
  assert.equal(await c.content({ journal: id("1"), recordId: id("a") }), text);
  const read = transport => client(transport).content({ journal: id("1"), recordId: id("a") });
  await assert.rejects(read(async () => new Response("short", { headers: { "content-type": "text/plain", "content-length": "99" } })), error("truncated_response"));
  await assert.rejects(read(async () => new Response("large", { headers: { "content-type": "text/plain", "content-length": String(16 * 1024 * 1024 + 1) } })), error("response_too_large"));
  await assert.rejects(read(async () => new Response(new Uint8Array([0xff]), { headers: { "content-type": "text/plain" } })));
  await assert.rejects(read(async () => new Response("html", { headers: { "content-type": "text/html" } })), error("invalid_response"));
});

test("EOF failure, remote denial, cancellation and transport errors never return partial records or provider secrets", async () => {
  let reads = 0;
  const c = client(async () => new Response(new ReadableStream({ pull(controller) {
    if (reads++ === 0) controller.enqueue(new TextEncoder().encode(JSON.stringify(page())));
    else controller.error(Error("private provider body " + token));
  } }), { headers: { "content-type": "application/json" } }));
  await assert.rejects(c.records(), e => e.code === "transport" && !String(e).includes(token));
  await assert.rejects(client(async () => new Response(token, { status: 404 })).records(), e => e.code === "http" && e.status === 404 && e.outcome === "none" && !String(e).includes(token));
  const controller = new AbortController(); controller.abort();
  await assert.rejects(client(async () => { throw Error("must not call"); }).records({}, { signal: controller.signal }), error("aborted"));
  const noBodyEnd = client(async () => new Response(new ReadableStream({ start(c) { c.enqueue(new TextEncoder().encode("partial private text")); } }), { headers: { "content-type": "text/plain" } }));
  const signal = AbortSignal.timeout(20);
  const keepAlive = setTimeout(() => {}, 1000);
  try { await assert.rejects(noBodyEnd.content({ journal: id("1"), recordId: id("a") }, { signal }), error("aborted")); }
  finally { clearTimeout(keepAlive); }
});
