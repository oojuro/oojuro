import assert from "node:assert/strict";
import { inspect } from "node:util";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  OojuroClient,
  OojuroError,
  PreparedJournalWrite,
  PreparedJournalPeriods,
} from "../index.js";
const P = "1".repeat(64),
  W = "2".repeat(64),
  I = "3".repeat(64),
  D = "4".repeat(64);
const TOKEN = "ojv2_" + "A".repeat(43),
  PRIVATE = "private-fixture-body";
const mark = (generation = 7) => ({ generation, epoch: 2, transaction: { id: I, digest: D } });
const root = () => ({ schema: 1, minimum_reader: 1, project: P, namespace: D });
const query = () => ({
  as_of: { from: -100, valid_at: 1000, known_at: 1000 },
  generation: null,
  writer: W,
  mode: {
    mode: "Range",
    versions: "current",
    filter: { kind: null, tool: null, status: null, agent_id: null },
  },
});
const input = () => ({
  agent_id: "reported-agent",
  parent_id: null,
  valid_at: -1,
  kind: "explicit",
  tool: null,
  status: null,
  exit: null,
  key: "key",
  text: PRIVATE + " 한글 🦉",
  attrs: { reported: true },
  indexed: false,
});
const record = () => ({
  id: I,
  record: { schema: 1, id: 9, writer: W, adapter: "caller-adapter", recorded_at: 100, ...input() },
});
const append = () => ({
  requestId: "original append",
  consent: "intentional-user-file",
  expectedRoot: root(),
  writer: W,
  adapter: "caller-adapter",
  input: input(),
});
const deletion = () => ({ requestId: "original delete", expectedRoot: root(), writer: W, id: I });
const forget = () => ({
  requestId: "original forget",
  expectedRoot: root(),
  writer: W,
  key: "key",
});
const periods = () => ({
  requestId: "original periods",
  expectedRoot: root(),
  query: query(),
  widthUs: 10,
  maxPeriods: 10,
  minGeneration: 3,
});
const usage = () => ({
  request_tag: I,
  intent_fingerprint: D,
  scope_tag: W,
  committed_at_us: 100,
  recorded_bytes: 2,
  kept_bytes: 12,
});
const outcome = (appended = true) => ({
  generation: 7,
  record_ids: appended ? { 0: I } : {},
  blob_revisions: {},
  writer: null,
  key: null,
  destruction: null,
  usage: usage(),
});
const response = (value, generation = 7, headers = {}) =>
  new Response(typeof value === "string" ? value : JSON.stringify(value), {
    headers: {
      "content-type": "application/json",
      "x-oojuro-generation": String(generation),
      ...headers,
    },
  });
const envelope = (value, generation = 7) =>
  response({ watermark: mark(generation), value }, generation);
const sdk = (fetch, extra = {}) =>
  new OojuroClient({
    baseUrl: "https://fixture.invalid",
    project: P,
    token: TOKEN,
    fetch,
    ...extra,
  });
const safe =
  (code, outcome = "none") =>
  (error) => {
    assert.ok(error instanceof OojuroError);
    assert.equal(error.code, code);
    assert.equal(error.outcome, outcome);
    for (const text of [error.message, error.stack, inspect(error), JSON.stringify(error)]) {
      assert.ok(!text.includes(PRIVATE));
      assert.ok(!text.includes(TOKEN));
    }
    assert.equal(error.cause, undefined);
    return true;
  };

test("prepared append/delete freeze exact native bytes across unknown send, rejected retry, recreated same original identity", async () => {
  for (const [method, make, appended] of [
    ["prepareJournalRecord", append, true],
    ["prepareJournalDelete", deletion, false],
    ["prepareJournalForget", forget, false],
  ]) {
    const bodies = [];
    let count = 0;
    const client = sdk(async (url, init) => {
      bodies.push(init.body);
      count++;
      assert.equal(
        url,
        "https://fixture.invalid/v2/projects/" +
          P +
          "/journal/" +
          (appended ? "records" : method === "prepareJournalForget" ? "forget" : "delete"),
      );
      assert.equal(init.headers.authorization, "Bearer " + TOKEN);
      assert.equal(init.redirect, "error");
      assert.equal(init.cache, "no-store");
      assert.equal(init.credentials, "omit");
      if (count === 1) throw Error(PRIVATE + TOKEN);
      if (count === 2) return new Response(PRIVATE, { status: 409 });
      return response({ outcome: outcome(appended) });
    });
    const source = make(),
      original = structuredClone(source),
      prepared = client[method](source);
    assert.ok(prepared instanceof PreparedJournalWrite);
    source.requestId = "changed";
    source.expectedRoot.namespace = P;
    if (appended) {
      source.input.text = "changed";
      source.input.attrs.reported = false;
    }
    await assert.rejects(prepared.send(), safe("transport", "unknown"));
    assert.equal(count, 1);
    await assert.rejects(prepared.send(), safe("http", "unknown"));
    assert.equal(count, 2);
    assert.deepEqual(await client[method](original).send(), outcome(appended));
    assert.equal(new Set(bodies).size, 1);
    const wire = JSON.parse(bodies[0]);
    assert.equal(wire.request_id, original.requestId);
    if (appended) {
      assert.deepEqual(wire.input, original.input);
      assert.equal(wire.consent, "intentional-user-file");
    }
    for (const value of [client, prepared])
      for (const text of [inspect(value), JSON.stringify(value)]) {
        assert.ok(!text.includes(PRIVATE));
        assert.ok(!text.includes(TOKEN));
      }
  }
});

test("root and pinned page keep native schema, current envelope and exact query/cursor separate", async () => {
  const calls = [];
  const q = query();
  const client = sdk(async (url, init) => {
    calls.push({ url, init });
    if (url.endsWith("/root")) return envelope(root(), 9);
    const wire = JSON.parse(init.body);
    assert.deepEqual(wire.expected_root, root());
    assert.deepEqual(wire.query, q);
    assert.equal(init.headers["x-oojuro-min-generation"], "8");
    return envelope(
      {
        watermark: mark(7),
        records: wire.cursor ? [] : [record()],
        next: wire.cursor ? null : "opaque-cursor",
      },
      9,
    );
  });
  assert.deepEqual((await client.journalRoot()).value, root());
  const first = await client.journalPage({ expectedRoot: root(), query: q }, { minGeneration: 8 });
  assert.equal(first.watermark.generation, 9);
  assert.equal(first.value.watermark.generation, 7);
  assert.equal(first.value.records[0].record.indexed, false);
  assert.equal(first.value.records[0].record.text, input().text);
  const next = await client.journalPage(
    { expectedRoot: root(), query: q, cursor: first.value.next },
    { minGeneration: 8 },
  );
  assert.deepEqual(next.value.records, []);
  assert.equal(JSON.parse(calls[2].init.body).cursor, "opaque-cursor");
});

test("stats and billed periods use exact lower-case schema and original search status retries", async () => {
  const calls = [];
  let count = 0;
  const client = sdk(async (url, init) => {
    calls.push({ url, body: init.body });
    if (url.endsWith("/stats"))
      return envelope({
        records: 2,
        text_bytes: 12,
        kinds: { explicit: 2 },
        first_valid_at: -1,
        last_valid_at: 12,
      });
    count++;
    if (count === 1)
      return envelope([
        { start_us: -10, records: 1, text_bytes: 2 },
        { start_us: 10, records: 1, text_bytes: 10 },
      ]);
    return new Response(JSON.stringify({ error: { code: "search_completed", message: PRIVATE } }), {
      status: 409,
      headers: { "content-type": "application/json" },
    });
  });
  assert.equal(
    (await client.journalStats({ expectedRoot: root(), query: query() })).value.records,
    2,
  );
  const source = periods(),
    original = structuredClone(source),
    prepared = client.prepareJournalPeriods(source);
  assert.ok(prepared instanceof PreparedJournalPeriods);
  source.widthUs = 99;
  source.query.writer = null;
  assert.equal((await prepared.send()).value[0].start_us, -10);
  await assert.rejects(
    client.prepareJournalPeriods(original).send(),
    safe("search_completed", "unknown"),
  );
  assert.equal(calls[1].body, calls[2].body);
  assert.ok(!inspect(prepared).includes(PRIVATE));
});

test("invalid requests, missing consent and encoded small-body caps fail before network", async () => {
  let count = 0;
  const client = sdk(async () => {
    count++;
    throw Error("must not send");
  });
  for (const mutate of [
    (x) => {
      delete x.consent;
    },
    (x) => {
      x.consent = "yes";
    },
    (x) => {
      x.expectedRoot.project = W;
    },
    (x) => {
      delete x.input.key;
    },
    (x) => {
      x.input.valid_at = Number.MAX_SAFE_INTEGER + 1;
    },
    (x) => {
      x.input.agent_id = "bad\nname";
    },
    (x) => {
      x.input.status = "success";
    },
    (x) => {
      x.input.attrs = { nested: { huge: "x".repeat(4097) } };
    },
    (x) => {
      x.input.attrs = { deep: Array.from({ length: 1025 }, () => 0) };
    },
    (x) => {
      x.input.text = "x".repeat(1024 * 1024);
    },
  ]) {
    const value = append();
    mutate(value);
    assert.throws(() => client.prepareJournalRecord(value), safe("invalid_request"));
  }
  assert.throws(
    () => sdk(async () => {}, { maxRequestBytes: 100 }).prepareJournalRecord(append()),
    safe("invalid_request"),
  );
  for (const mutate of [
    (x) => {
      x.query.generation = 0;
    },
    (x) => {
      x.query.mode.versions = "Current";
    },
    (x) => {
      x.query.mode = { mode: "History", key: "key" };
      x.query.writer = null;
    },
    (x) => {
      x.query.mode.filter.unknown = "x";
    },
    (x) => {
      x.maxRecords = 1001;
    },
    (x) => {
      x.maxBytes = 1023;
    },
    (x) => {
      x.cursor = "x".repeat(16385);
    },
  ]) {
    const value = { expectedRoot: root(), query: query() };
    mutate(value);
    await assert.rejects(client.journalPage(value), safe("invalid_request"));
  }
  assert.throws(
    () => client.prepareJournalPeriods({ ...periods(), widthUs: 0 }),
    safe("invalid_request"),
  );
  assert.throws(
    () => client.prepareJournalForget({ ...forget(), key: "" }),
    safe("invalid_request"),
  );
  assert.equal(count, 0);
});

test("native integer fields reject unsafe or non-integer source lexemes with private-safe errors", async () => {
  const invalid = [
    JSON.stringify({
      watermark: mark(),
      value: { watermark: mark(), records: [record()], next: null },
    }).replace('"recorded_at":100', '"recorded_at":100.000000000000001'),
    JSON.stringify({
      watermark: mark(),
      value: { watermark: mark(), records: [record()], next: null },
    }).replace('"id":9', '"id":9007199254740993'),
    JSON.stringify({
      watermark: mark(),
      value: { watermark: mark(), records: [record()], next: null },
    }).replace('"valid_at":-1', '"valid_at":-1e0'),
    JSON.stringify({
      watermark: mark(),
      value: { watermark: mark(), records: [record()], next: null },
    }).replace('"id":9', '"id":9.0'),
  ];
  for (const wire of invalid) {
    const client = sdk(async () => response(wire));
    await assert.rejects(
      client.journalPage({ expectedRoot: root(), query: query() }),
      safe("invalid_response"),
    );
  }
});

test("malformed ACK, roots, pages, stats and periods never escape validators", async () => {
  const badAck = outcome();
  badAck.record_ids = {};
  await assert.rejects(
    sdk(async () => response({ outcome: badAck }))
      .prepareJournalRecord(append())
      .send(),
    safe("invalid_response", "unknown"),
  );
  await assert.rejects(
    sdk(async () => envelope({ ...root(), project: W })).journalRoot(),
    safe("invalid_response"),
  );
  for (const mutate of [
    (p) => {
      p.watermark.generation = 8;
    },
    (p) => {
      p.records[0].record.writer = D;
    },
    (p) => {
      p.records[0].record.key = 3;
    },
    (p) => {
      p.records.push(record());
    },
    (p) => {
      p.records[0].record.unknown = PRIVATE;
    },
    (p) => {
      p.next = 4;
    },
  ]) {
    const page = { watermark: mark(), records: [record()], next: null };
    mutate(page);
    await assert.rejects(
      sdk(async () => envelope(page)).journalPage({ expectedRoot: root(), query: query() }),
      safe("invalid_response"),
    );
  }
  const empty = { records: 0, text_bytes: 0, kinds: {}, first_valid_at: null, last_valid_at: null };
  assert.deepEqual(
    (await sdk(async () => envelope(empty)).journalStats({ expectedRoot: root(), query: query() }))
      .value,
    empty,
  );
  await assert.rejects(
    sdk(async () => envelope({ ...empty, kinds: { explicit: 1 } })).journalStats({
      expectedRoot: root(),
      query: query(),
    }),
    safe("invalid_response"),
  );
  for (const values of [
    [{ start_us: 1, records: 1, text_bytes: 0 }],
    [{ start_us: 10, records: 0, text_bytes: 0 }],
    [
      { start_us: 10, records: 1, text_bytes: 0 },
      { start_us: 0, records: 1, text_bytes: 0 },
    ],
  ]) {
    await assert.rejects(
      sdk(async () => envelope(values))
        .prepareJournalPeriods(periods())
        .send(),
      safe("invalid_response", "unknown"),
    );
  }
});

test("response caps, wrong generation, abort and incomplete stream remain all-or-nothing", async () => {
  const data = JSON.stringify({ watermark: mark(), value: root() });
  await assert.rejects(
    sdk(async () => response(data), { maxResponseBytes: 10 }).journalRoot(),
    safe("response_too_large"),
  );
  await assert.rejects(sdk(async () => response(data, 6)).journalRoot(), safe("invalid_response"));
  await assert.rejects(
    sdk(async () => response(data, 7, { "x-oojuro-generation": "9007199254740993" })).journalRoot(),
    safe("invalid_response"),
  );
  const abort = new AbortController();
  abort.abort();
  await assert.rejects(
    sdk(async () => {
      throw Error("must not send");
    }).journalRoot({ signal: abort.signal }),
    safe("aborted"),
  );
  const partial = new ReadableStream({
    start(c) {
      c.enqueue(new TextEncoder().encode(data));
      c.error(Error(PRIVATE + TOKEN));
    },
  });
  await assert.rejects(
    sdk(
      async () =>
        new Response(partial, {
          headers: { "content-type": "application/json", "x-oojuro-generation": "7" },
        }),
    ).journalRoot(),
    safe("transport"),
  );
  await assert.rejects(
    sdk(async () =>
      response(data, 7, { "content-length": String(data.length + 10) }),
    ).journalRoot(),
    safe("truncated_response"),
  );
});

test("read-only page/stats refusals remain none; changed root/query cursors are never silently retried", async () => {
  let calls = 0;
  const client = sdk(async (_url, init) => {
    calls++;
    const wire = JSON.parse(init.body);
    assert.equal(wire.cursor, "original-cursor");
    assert.equal(wire.query.mode.mode, "History");
    assert.equal(wire.query.mode.key, "changed");
    return new Response(PRIVATE + TOKEN, { status: 409 });
  });
  const q = query();
  q.mode = { mode: "History", key: "changed" };
  await assert.rejects(
    client.journalPage({ expectedRoot: root(), query: q, cursor: "original-cursor" }),
    safe("http"),
  );
  assert.equal(calls, 1);
  await assert.rejects(
    sdk(async () => {
      throw Error(PRIVATE);
    }).journalStats({ expectedRoot: root(), query: query() }),
    safe("transport"),
  );
  await assert.rejects(
    sdk(async () => new Response(PRIVATE, { status: 503 })).journalStats({
      expectedRoot: root(),
      query: query(),
    }),
    safe("http"),
  );
});

test("page snapshot generation, output budget and ID/history selections are checked", async () => {
  const page = () => ({ watermark: mark(), records: [record()], next: null });
  const q = query();
  q.generation = 6;
  assert.equal(
    (await sdk(async () => envelope(page())).journalPage({ expectedRoot: root(), query: q })).value
      .watermark.generation,
    7,
  );
  q.generation = 8;
  await assert.rejects(
    sdk(async () => envelope(page())).journalPage({ expectedRoot: root(), query: q }),
    safe("invalid_response"),
  );
  for (const mode of [
    { mode: "Id", id: D },
    { mode: "History", key: "other" },
  ]) {
    const selected = query();
    selected.mode = mode;
    await assert.rejects(
      sdk(async () => envelope(page())).journalPage({ expectedRoot: root(), query: selected }),
      safe("invalid_response"),
    );
  }
  const large = page();
  large.records[0].record.text = "x".repeat(2048);
  await assert.rejects(
    sdk(async () => envelope(large)).journalPage({
      expectedRoot: root(),
      query: query(),
      maxBytes: 1024,
    }),
    safe("invalid_response"),
  );
});

test("own-data preflight rejects getters/toJSON and over-budget escapes before serialization or send", async () => {
  let getters = 0,
    serialized = 0,
    sent = 0;
  const client = sdk(async () => {
    sent++;
    throw Error("must not send");
  });
  const accessor = append();
  Object.defineProperty(accessor, "input", {
    enumerable: true,
    get() {
      getters++;
      return input();
    },
  });
  assert.throws(() => client.prepareJournalRecord(accessor), safe("invalid_request"));
  const nested = append();
  Object.defineProperty(nested.input.attrs, "getter", {
    enumerable: true,
    get() {
      getters++;
      return PRIVATE;
    },
  });
  assert.throws(() => client.prepareJournalRecord(nested), safe("invalid_request"));
  const custom = append();
  custom.input.attrs = {
    toJSON() {
      serialized++;
      return {};
    },
  };
  assert.throws(() => client.prepareJournalRecord(custom), safe("invalid_request"));
  const inherited = Object.create(append());
  assert.throws(() => client.prepareJournalRecord(inherited), safe("invalid_request"));
  const expanded = append();
  expanded.input.text = "\u0000".repeat(180_000);
  assert.throws(() => client.prepareJournalRecord(expanded), safe("invalid_request"));
  const deep = append();
  let value = deep.input.attrs;
  for (let i = 0; i < 50; i++) {
    value.next = {};
    value = value.next;
  }
  assert.throws(() => client.prepareJournalRecord(deep), safe("invalid_request"));
  const page = { expectedRoot: root(), query: query() };
  Object.defineProperty(page, "query", {
    enumerable: true,
    get() {
      getters++;
      return query();
    },
  });
  await assert.rejects(client.journalPage(page), safe("invalid_request"));
  await assert.rejects(client.journalStats(page), safe("invalid_request"));
  const billed = periods();
  Object.defineProperty(billed, "widthUs", {
    enumerable: true,
    get() {
      getters++;
      return 10;
    },
  });
  assert.throws(() => client.prepareJournalPeriods(billed), safe("invalid_request"));
  assert.equal(getters, 0);
  assert.equal(serialized, 0);
  assert.equal(sent, 0);
});

test("own prototype-looking JSON attribute names remain data, without prototype changes", async () => {
  const value = append();
  value.input.attrs = JSON.parse(
    '{"__proto__":{"reported":true},"constructor":"caller-label","toJSON":"ordinary-data"}',
  );
  const client = sdk(async (_url, init) => {
    const wire = JSON.parse(init.body);
    assert.deepEqual(wire.input.attrs, value.input.attrs);
    assert.ok(Object.hasOwn(wire.input.attrs, "__proto__"));
    return response({ outcome: outcome() });
  });
  assert.equal((await client.prepareJournalRecord(value).send()).record_ids["0"], I);
  assert.equal({}.reported, undefined);
});

test("actual Rust Bearer HTTP response fixtures pass the client without rewriting schema or headers", async () => {
  const fixture = JSON.parse(
    readFileSync(new URL("./support/journal-wire.json", import.meta.url), "utf8"),
  );
  assert.equal(fixture.schema, 1);
  const rootCase = fixture.cases.find((item) => item.operation === "root");
  const project = JSON.parse(rootCase.response_body).value.project;
  let verified = 0;
  for (const item of fixture.cases) {
    if (item.operation === "descriptors") continue; // Separate framed/discovery follow-up.
    let calls = 0;
    const client = new OojuroClient({
      baseUrl: "http://127.0.0.1",
      project,
      token: TOKEN,
      fetch: async (url, init) => {
        calls++;
        assert.equal(url, "http://127.0.0.1" + item.path);
        assert.equal(init.method, item.method);
        if (item.method === "POST") {
          const sent = JSON.parse(init.body),
            expected = structuredClone(item.request);
          if (item.operation === "page" && !Object.hasOwn(expected, "cursor"))
            expected.cursor = null;
          assert.deepEqual(sent, expected);
        } else assert.equal(init.body, undefined);
        assert.ok(item.response_headers["content-type"]);
        assert.ok(item.response_headers["x-oojuro-generation"]);
        return new Response(item.response_body, {
          status: item.status,
          headers: item.response_headers,
        });
      },
    });
    const request = item.request;
    let value;
    if (item.operation === "root") value = await client.journalRoot();
    else if (item.operation === "append")
      value = await client
        .prepareJournalRecord({
          requestId: request.request_id,
          consent: request.consent,
          expectedRoot: request.expected_root,
          writer: request.writer,
          adapter: request.adapter,
          input: request.input,
        })
        .send();
    else if (item.operation === "delete")
      value = await client
        .prepareJournalDelete({
          requestId: request.request_id,
          expectedRoot: request.expected_root,
          writer: request.writer,
          id: request.id,
        })
        .send();
    else if (item.operation === "forget")
      value = await client
        .prepareJournalForget({
          requestId: request.request_id,
          expectedRoot: request.expected_root,
          writer: request.writer,
          key: request.key,
        })
        .send();
    else if (item.operation === "page")
      value = await client.journalPage({
        expectedRoot: request.expected_root,
        query: request.query,
        ...(request.cursor === undefined ? {} : { cursor: request.cursor }),
        maxRecords: request.max_records,
        maxBytes: request.max_bytes,
      });
    else if (item.operation === "stats")
      value = await client.journalStats({
        expectedRoot: request.expected_root,
        query: request.query,
      });
    else if (item.operation === "periods")
      value = await client
        .prepareJournalPeriods({
          requestId: request.request_id,
          expectedRoot: request.expected_root,
          query: request.query,
          widthUs: request.width_us,
          maxPeriods: request.max_periods,
        })
        .send();
    else assert.fail("unexpected actual wire fixture operation");
    const expected = JSON.parse(item.response_body);
    assert.deepEqual(
      value,
      ["append", "delete", "forget"].includes(item.operation) ? expected.outcome : expected,
    );
    assert.equal(calls, 1);
    verified++;
  }
  assert.equal(verified, 7);
});
