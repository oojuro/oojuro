import assert from "node:assert/strict";
import { inspect } from "node:util";
import { test } from "node:test";
import { OojuroClient, OojuroError, reportedTrace } from "../index.js";
const P = "1".repeat(64),
  W = "2".repeat(64),
  R = "3".repeat(64),
  B = "4".repeat(64);
const TOKEN = "ojv2_" + "A".repeat(43),
  PRIVATE = "synthetic-private-content";
const utf8 = new TextEncoder();
const watermark = (generation) => ({ generation, epoch: 2, transaction: { id: R, digest: B } });
const row = () => ({
  id: R,
  writer: W,
  writer_name: "writer life 1",
  memory_name: null,
  trace: reportedTrace("hello 🦉 한글", {
    agent_id: "reported-agent",
    parent_id: "reported-parent",
    source: "synthetic",
    session: "reported-run",
    parent_session: "reported-parent-run",
    tool: "reported-tool",
    indexed: false,
    raw: { source_generation: "local-source-generation", start: 2, end: 7, emission: 0 },
  }),
});
function response(value, generation = 7, type = "application/json") {
  return new Response(typeof value === "string" ? value : JSON.stringify(value), {
    headers: { "content-type": type, "x-oojuro-generation": String(generation) },
  });
}
const envelope = (value, generation = 7) =>
  response({ watermark: watermark(generation), value }, generation);
const usage = () => ({
  request_tag: R, intent_fingerprint: B, scope_tag: W,
  committed_at_us: 1767323045001000, recorded_bytes: 3, kept_bytes: 700,
});
const ack = () => ({
  outcome: {
    generation: 7,
    record_ids: { 0: R, 1: B },
    blob_revisions: { 2: B },
    writer: null,
    key: null,
    destruction: null,
    usage: usage(),
  },
});
const input = () => ({
  requestId: "caller exact id 🦉\n1",
  scope: W,
  mutations: [
    { kind: "Record", writer: W, trace: row().trace },
    { kind: "Memory", writer: W, name: "explicit note", trace: reportedTrace("memory") },
    { kind: "Blob", writer: W, name: "attachment", expected: null, bytes: [0, 255, 1] },
  ],
});
const client = (fetch, extra = {}) =>
  new OojuroClient({
    baseUrl: "https://synthetic.invalid",
    project: P,
    token: TOKEN,
    fetch,
    ...extra,
  });
function safe(error, code, outcome = "none") {
  assert.ok(error instanceof OojuroError);
  assert.equal(error.code, code);
  assert.equal(error.outcome, outcome);
  for (const text of [error.message, error.stack, inspect(error), JSON.stringify(error)]) {
    assert.ok(!text.includes(TOKEN));
    assert.ok(!text.includes(PRIVATE));
  }
  assert.equal(error.cause, undefined);
  return true;
}
function stream(parts, end = "close") {
  let i = 0;
  return new ReadableStream(
    {
      pull(controller) {
        if (i < parts.length) controller.enqueue(parts[i++]);
        else if (end === "close") controller.close();
        else controller.error(new Error(TOKEN + PRIVATE));
      },
    },
    { highWaterMark: 0 },
  );
}
function streaming(body, type = "application/x-ndjson", headers = {}) {
  return new Response(body, {
    headers: { "content-type": type, "x-oojuro-generation": "7", ...headers },
  });
}

test("ambiguous original commit reuses exact frozen caller input, with no automatic retry or new identity", async () => {
  const sent = [],
    request = input();
  let committed = false;
  const sdk = client(async (url, init) => {
    sent.push({ url, init });
    if (!committed) {
      committed = true;
      throw new Error(TOKEN + PRIVATE);
    }
    if (sent.length === 2) return new Response(PRIVATE + TOKEN, { status: 503 });
    return response(ack());
  });
  const prepared = sdk.prepareCommit(request);
  request.requestId = "changed";
  request.mutations[0].trace.text = "changed";
  request.mutations[2].bytes[0] = 9;
  await assert.rejects(prepared.send(), (error) => safe(error, "transport", "unknown"));
  assert.equal(sent.length, 1);
  await assert.rejects(prepared.send(), (error) => safe(error, "http", "unknown"));
  assert.equal(sent.length, 2);
  assert.equal((await prepared.send()).record_ids[0], R);
  assert.equal(sent.length, 3);
  assert.equal(new Set(sent.map((item) => item.init.body)).size, 1);
  const original = JSON.parse(sent[0].init.body);
  assert.equal(original.request_id, "caller exact id 🦉\n1");
  assert.equal(original.mutations[0].trace.text, "hello 🦉 한글");
  assert.deepEqual(original.mutations[0].trace.raw, row().trace.raw);
  assert.equal(original.mutations[0].trace.indexed, false);
  assert.deepEqual(original.mutations[2].bytes, [0, 255, 1]);
  for (const { url, init } of sent) {
    assert.equal(url, `https://synthetic.invalid/v2/projects/${P}/commits`);
    assert.equal(init.headers.authorization, `Bearer ${TOKEN}`);
    assert.ok(!init.body.includes(TOKEN));
    assert.equal(init.redirect, "error");
    assert.equal(init.credentials, "omit");
  }
  assert.ok(!inspect(sdk).includes(TOKEN));
  assert.ok(!JSON.stringify(sdk).includes(TOKEN));
  assert.ok(!inspect(prepared).includes("hello"));
  assert.ok(!JSON.stringify(prepared).includes("hello"));
});

test("4xx rejects this attempt but cannot resolve a prior ambiguous commit; bodies are never parsed", async () => {
  for (const status of [401, 403, 409, 413]) {
    let cancelled = false,
      pulled = false;
    const sdk = client(
      async () =>
        new Response(
          new ReadableStream(
            {
              pull() {
                pulled = true;
                throw new Error(TOKEN);
              },
              cancel() {
                cancelled = true;
              },
            },
            { highWaterMark: 0 },
          ),
          { status },
        ),
    );
    await assert.rejects(sdk.prepareCommit(input()).send(), (error) =>
      safe(error, "http", "unknown"),
    );
    assert.equal(cancelled, true);
    assert.equal(pulled, false);
  }
});

test("commit body/ACK failure is unknown and retries the identical prepared body", async () => {
  const sent = [];
  let turn = 0;
  const sdk = client(async (_url, init) => {
    sent.push(init.body);
    return turn++ === 0
      ? streaming(stream([utf8.encode('{"outcome":')], "error"), "application/json")
      : response(ack());
  });
  const prepared = sdk.prepareCommit(input());
  await assert.rejects(prepared.send(), (error) => safe(error, "transport", "unknown"));
  await prepared.send();
  assert.equal(sent.length, 2);
  assert.equal(sent[0], sent[1]);
  for (const bad of [
    { outcome: { ...ack().outcome, record_ids: {} } },
    { outcome: { ...ack().outcome, generation: 6 } },
    {},
  ]) {
    await assert.rejects(
      client(async () => response(bad))
        .prepareCommit(input())
        .send(),
      (error) => safe(error, "invalid_response", "unknown"),
    );
  }
});

test("secret-bearing errors, redirects and malformed configuration never leak credentials or causes", async () => {
  for (const baseUrl of [
    `https://user:${TOKEN}@synthetic.invalid/`,
    `https://synthetic.invalid/?token=${TOKEN}`,
    "https://synthetic.invalid/path",
    "http://remote.invalid/",
  ]) {
    assert.throws(
      () => client(() => {}, { baseUrl }),
      (error) => safe(error, "invalid_configuration"),
    );
  }
  const configuration = {
    get token() {
      throw new Error(TOKEN + PRIVATE);
    },
  };
  assert.throws(
    () => new OojuroClient(configuration),
    (error) => safe(error, "invalid_configuration"),
  );
  await assert.rejects(
    client(async () => {
      throw new Error(`redirect ${TOKEN} ${PRIVATE}`);
    }).records(),
    (error) => safe(error, "transport"),
  );
});

test("rejects unsafe integer input, malformed scopes, implicit context, malformed Unicode and invalid raw ranges before transport", () => {
  let calls = 0;
  const sdk = client(() => {
    calls++;
    throw new Error("unexpected");
  });
  const changes = [
    (value) => {
      value.scope = "../other";
    },
    (value) => {
      value.mutations[0].trace.tokens_in = Number.MAX_SAFE_INTEGER + 1;
    },
    (value) => {
      value.mutations[0].trace.raw.end = Number.MAX_SAFE_INTEGER + 1;
    },
    (value) => {
      value.mutations[0].trace.sequence = 0x100000000;
    },
    (value) => {
      value.mutations[0].trace.raw.start = 8;
    },
    (value) => {
      value.mutations[0].trace.raw_context = "never upload";
    },
    (value) => {
      value.mutations[0].trace.text = "\ud800";
    },
    (value) => {
      value.mutations[0].trace.arguments = { invalid: Infinity };
    },
    (value) => {
      value.mutations[2].bytes = [256];
    },
  ];
  for (const change of changes) {
    const value = input();
    change(value);
    assert.throws(
      () => sdk.prepareCommit(value),
      (error) => safe(error, "invalid_request"),
    );
  }
  assert.throws(
    () => reportedTrace("x", { parent: Number.MAX_SAFE_INTEGER + 1 }),
    (error) => safe(error, "invalid_request"),
  );
  assert.equal(calls, 0);
});

test("request sizes are byte bounds and circular/non-JSON arguments fail safely", () => {
  const sdk = client(() => {}, { maxRequestBytes: 100 });
  assert.throws(
    () => sdk.prepareCommit(input()),
    (error) => safe(error, "invalid_request"),
  );
  const cyclic = {};
  cyclic.self = cyclic;
  const value = input();
  value.mutations[0].trace.arguments = cyclic;
  assert.throws(
    () => client(() => {}).prepareCommit(value),
    (error) => safe(error, "invalid_request"),
  );
  assert.throws(
    () => reportedTrace("🦉".repeat(262145)),
    (error) => safe(error, "invalid_request"),
  );
});

test("read minimum is sent, and a stale/missing/mismatched watermark cannot be returned", async () => {
  const sdk = client(async (url, init) => {
    assert.equal(init.headers["x-oojuro-min-generation"], "7");
    assert.equal(url, `https://synthetic.invalid/v2/projects/${P}/records`);
    return envelope([row()]);
  });
  assert.equal((await sdk.records({ minGeneration: 7 })).value[0].trace.agent_id, "reported-agent");
  await assert.rejects(
    client(async () => envelope([row()], 6)).records({ minGeneration: 7 }),
    (error) => safe(error, "behind"),
  );
  await assert.rejects(
    client(async () => response({ watermark: watermark(6), value: [] })).records(),
    (error) => safe(error, "invalid_response"),
  );
  await assert.rejects(
    client(
      async () => new Response("{}", { headers: { "content-type": "application/json" } }),
    ).records(),
    (error) => safe(error, "invalid_response"),
  );
  await assert.rejects(sdk.records({ minGeneration: Number.MAX_SAFE_INTEGER + 1 }), (error) =>
    safe(error, "invalid_request"),
  );
});

test("wire u64/i64/decimal integer precision loss is refused rather than silently rounded", async () => {
  const base = JSON.stringify({ watermark: watermark(7), value: [row()] });
  for (const number of ["9007199254740993", "-9007199254740993", "1.0000000000000001", "1e0"]) {
    const bad = base.replace('"valid_us":0', `"valid_us":${number}`);
    await assert.rejects(client(async () => response(bad)).records(), (error) =>
      safe(error, "invalid_response"),
    );
  }
  const sdk = client(async () =>
    response(base.replace('"generation":7', '"generation":9007199254740993'), "9007199254740993"),
  );
  await assert.rejects(sdk.records(), (error) => safe(error, "invalid_response"));
});

test("record detail validates identity; search and authorization preserve the actual v2 shapes", async () => {
  const sdk = client(async (url, init) => {
    if (url.endsWith("/access"))
      return envelope({ admin: false, read: [W], write: [], retry: [W] });
    if (url.endsWith("/search")) {
      assert.deepEqual(JSON.parse(init.body), { request_id: "search-original", query: "hello", k: 2 });
      return envelope([{ ...row(), score_bits: 2.75, continued: false, excerpt_start: 0, excerpt_end: 5, excerpt: "hello" }]);
    }
    return envelope(row());
  });
  assert.equal((await sdk.access()).value.admin, false);
  assert.equal((await sdk.record(R)).value.id, R);
  assert.equal((await sdk.search({ requestId: "search-original", query: "hello", k: 2 })).value[0].score_bits, 2.75);
  await assert.rejects(sdk.record(B), (error) => safe(error, "invalid_response"));
  await assert.rejects(sdk.search({ requestId: "search-original", query: "", k: 2 }), (error) => safe(error, "invalid_request"));
});

test("blob names are encoded, writer lifetimes are opaque, and binary bytes are returned exactly", async () => {
  const sdk = client(async (url, init) => {
    const parsed = new URL(url);
    assert.equal(parsed.searchParams.get("name"), "folder/a?b&🦉");
    assert.equal(parsed.searchParams.get("writer"), W);
    assert.ok(!url.includes(TOKEN));
    assert.equal(init.headers["x-oojuro-min-generation"], "7");
    return streaming(stream([new Uint8Array([0, 255, 128])]), "application/octet-stream");
  });
  assert.deepEqual(
    (await sdk.blob(W, "folder/a?b&🦉", { minGeneration: 7 })).bytes,
    new Uint8Array([0, 255, 128]),
  );
  await assert.rejects(sdk.blob(W, "invalid\nname"), (error) => safe(error, "invalid_request"));
});

test("JSONL export survives arbitrary UTF8/chunk boundaries and publishes only after clean EOF", async () => {
  const encoded = utf8.encode(
    JSON.stringify(row()) + "\n" + JSON.stringify({ ...row(), id: B }) + "\n",
  );
  const parts = Array.from(encoded, (byte) => new Uint8Array([byte]));
  const result = await client(async () => streaming(stream(parts))).exportRecords({
    minGeneration: 7,
  });
  assert.equal(result.generation, 7);
  assert.equal(result.records.length, 2);
  assert.equal(result.records[0].trace.text, "hello 🦉 한글");
});

test("revocation after one valid JSONL row never returns a partial completed export", async () => {
  let returned = false;
  const promise = client(async () =>
    streaming(stream([utf8.encode(JSON.stringify(row()) + "\n")], "error")),
  )
    .exportRecords()
    .then(() => {
      returned = true;
    });
  await assert.rejects(promise, (error) => safe(error, "transport"));
  assert.equal(returned, false);
});

test("truncated JSONL, invalid UTF8, invalid line shapes and length mismatch fail closed", async () => {
  const cases = [
    [utf8.encode(JSON.stringify(row()))],
    [utf8.encode(JSON.stringify(row()) + "\n{")],
    [new Uint8Array([0xf0, 0x9f])],
    [utf8.encode("{}\n")],
    [utf8.encode("\n")],
  ];
  for (const parts of cases)
    await assert.rejects(client(async () => streaming(stream(parts))).exportRecords(), (error) => {
      assert.ok(error instanceof OojuroError);
      return safe(error, error.code);
    });
  await assert.rejects(
    client(async () =>
      streaming(stream([utf8.encode(JSON.stringify(row()) + "\n")]), "application/x-ndjson", {
        "content-length": "9999",
      }),
    ).exportRecords(),
    (error) => safe(error, "truncated_response"),
  );
});

test("body/line bounds and content type are enforced before any completed result", async () => {
  let cancelled = false;
  const sdk = client(
    async () =>
      streaming(
        new ReadableStream({
          start(c) {
            c.enqueue(utf8.encode("x".repeat(101)));
          },
          cancel() {
            cancelled = true;
          },
        }),
      ),
    { maxResponseBytes: 100 },
  );
  await assert.rejects(sdk.exportRecords(), (error) => safe(error, "response_too_large"));
  assert.equal(cancelled, true);
  await assert.rejects(
    client(
      async () => streaming(stream([]), "application/x-ndjson", { "content-length": "99999999" }),
      { maxResponseBytes: 100 },
    ).exportRecords(),
    (error) => safe(error, "response_too_large"),
  );
  await assert.rejects(
    client(async () => response("not JSON", 7, "text/html")).records(),
    (error) => safe(error, "invalid_response"),
  );
});

test("abort propagates during body consumption, including a pending read, without returning partial data", async () => {
  const controller = new AbortController();
  let entered;
  const reading = new Promise((resolve) => {
    entered = resolve;
  });
  const sdk = client(async () =>
    streaming(
      new ReadableStream(
        {
          pull() {
            entered();
          },
          cancel() {},
        },
        { highWaterMark: 0 },
      ),
    ),
  );
  const promise = sdk.exportRecords({ signal: controller.signal });
  await reading;
  controller.abort(TOKEN + PRIVATE);
  await assert.rejects(promise, (error) => safe(error, "aborted"));
  const before = new AbortController();
  before.abort();
  let called = false;
  await assert.rejects(
    client(async () => {
      called = true;
    }).records({ signal: before.signal }),
    (error) => safe(error, "aborted"),
  );
  assert.equal(called, false);
});

test("all supported delete mutations preserve opaque identities and conditional revisions", async () => {
  const mutations = [
    { kind: "DeleteRecord", id: R },
    { kind: "DeleteMemory", writer: W, name: "old note" },
    { kind: "DeleteBlob", writer: W, name: "old file", expected: B },
  ];
  const sdk = client(async (_url, init) => {
    assert.deepEqual(JSON.parse(init.body).mutations, mutations);
    return response({ outcome: { ...ack().outcome, record_ids: {}, blob_revisions: {} } });
  });
  assert.equal(
    (await sdk.prepareCommit({ requestId: "delete-exact", scope: W, mutations }).send()).generation,
    7,
  );
});

test("maximum exact signed/unsigned JS wire integers round-trip without normalization", async () => {
  const value = row();
  value.trace.parent = Number.MAX_SAFE_INTEGER;
  value.trace.valid_us = -Number.MAX_SAFE_INTEGER;
  value.trace.turn = 0xffffffff;
  const result = await client(async () => envelope([value])).records();
  assert.equal(result.value[0].trace.parent, Number.MAX_SAFE_INTEGER);
  assert.equal(result.value[0].trace.valid_us, -Number.MAX_SAFE_INTEGER);
});

test("empty authorized export is complete; a single oversized line cannot accumulate", async () => {
  assert.deepEqual((await client(async () => streaming(stream([]))).exportRecords()).records, []);
  await assert.rejects(
    client(async () =>
      streaming(stream([utf8.encode("x".repeat(2 * 1024 * 1024 + 16385))])),
    ).exportRecords(),
    (error) => safe(error, "response_too_large"),
  );
});

test("untrusted transport error objects and caller getters cannot smuggle raw strings into errors", async () => {
  await assert.rejects(
    client(async () => {
      throw new OojuroError(TOKEN + PRIVATE, { status: PRIVATE });
    }).records(),
    (error) => safe(error, "transport"),
  );
  await assert.rejects(
    client(() => {})
      .prepareCommit(input())
      .send({
        get signal() {
          throw new Error(TOKEN + PRIVATE);
        },
      }),
    (error) => safe(error, "invalid_request"),
  );
});

test("prepared metered search freezes ID, query and minimum and exposes safe status-only outcomes", async () => {
  const sent=[];let calls=0;
  const sdk=client(async (_url,init)=>{sent.push(init);calls++;
    if(calls===1)throw new Error(TOKEN+PRIVATE);
    return new Response(JSON.stringify({error:{code:"search_completed",message:TOKEN+PRIVATE}}),{status:409,headers:{"content-type":"application/json"}});
  });
  const intent={requestId:"original-metered-search",query:"hello",k:2,minGeneration:6};
  const prepared=sdk.prepareSearch(intent);intent.requestId="changed";intent.query="changed";intent.minGeneration=9;
  await assert.rejects(prepared.send(),e=>safe(e,"transport","unknown"));assert.equal(calls,1);
  await assert.rejects(prepared.send(),e=>safe(e,"search_completed","unknown"));assert.equal(calls,2);
  assert.equal(sent[0].body,sent[1].body);assert.equal(sent[1].headers["x-oojuro-min-generation"],"6");
  assert.equal(JSON.parse(sent[0].body).request_id,"original-metered-search");
  assert.ok(!inspect(prepared).includes("hello"));assert.ok(!JSON.stringify(prepared).includes("hello"));
});
test("metered search validates native continuation and exact UTF8 excerpt ranges", async()=>{
  const hit={...row(),score_bits:3.25,continued:true,excerpt_start:6,excerpt_end:10,excerpt:"🦉"};
  const intent={requestId:"range-search",query:"hello",k:1};
  const valid=await client(async()=>envelope([hit])).search(intent);assert.equal(valid.value[0].continued,true);
  for(const change of [{excerpt_start:7},{excerpt_end:9},{continued:1},{excerpt:"wrong"}])
    await assert.rejects(client(async()=>envelope([{...hit,...change}])).search(intent),e=>safe(e,"invalid_response","unknown"));
  assert.throws(()=>client(async()=>envelope([])).prepareSearch({query:"hello",k:1}),e=>safe(e,"invalid_request"));
});
test("blob current CAS revision is validated without inventing one for old replies",async()=>{
  const sdk=client(async()=>streaming(stream([new Uint8Array([0,255])]),"application/octet-stream",{"x-oojuro-blob-revision":R}));
  assert.equal((await sdk.blob(W,"file")).revision,R);
  await assert.rejects(client(async()=>streaming(stream([new Uint8Array([0])]),"application/octet-stream",{"x-oojuro-blob-revision":"invalid"})).blob(W,"file"),e=>safe(e,"invalid_response"));
});


test("current cloud usage receipt is required and returned unchanged for data commits and deletes", async () => {
  const expected = ack();
  const sent = [];
  const sdk = client(async (_url, init) => { sent.push(JSON.parse(init.body)); return response(expected); });
  const request = { ...input(), usage: { request_tag: PRIVATE + TOKEN } };
  const result = await sdk.prepareCommit(request).send();
  assert.deepEqual(result.usage, expected.outcome.usage);
  assert.deepEqual(Object.keys(sent[0]).sort(), ["mutations", "request_id", "scope"]);
  assert.equal(Object.hasOwn(sent[0], "usage"), false);
  const deletionAck = { outcome: { ...ack().outcome, record_ids: {}, blob_revisions: {},
    usage: { ...usage(), committed_at_us: -1, recorded_bytes: 0, kept_bytes: 500 } } };
  const deleted = await client(async () => response(deletionAck)).prepareCommit({
    requestId: "selected-delete", scope: W,
    mutations: [{ kind: "DeleteBlob", writer: W, name: "selected-file", expected: B }],
  }).send();
  assert.deepEqual(deleted.usage, deletionAck.outcome.usage);
});

test("usage receipt refuses absent fields, control-only null tags, malformed tags and extra schema", async () => {
  const edits = [
    (v) => { delete v.usage; }, (v) => { v.usage = null; }, (v) => { v.usage = []; },
    (v) => { v.usage.extra = PRIVATE + TOKEN; },
    ...["request_tag", "intent_fingerprint", "scope_tag", "committed_at_us", "recorded_bytes", "kept_bytes"]
      .map((key) => (v) => { delete v.usage[key]; }),
    ...["request_tag", "intent_fingerprint", "scope_tag"].flatMap((key) => [
      (v) => { v.usage[key] = null; }, (v) => { v.usage[key] = PRIVATE + TOKEN; },
      (v) => { v.usage[key] = "A".repeat(64); },
    ]),
    (v) => { v.usage.predecessor = B; },
    (v) => { v.usage.recorded_bytes = -1; }, (v) => { v.usage.kept_bytes = -1; },
  ];
  for (const edit of edits) {
    const value = ack(); edit(value.outcome);
    await assert.rejects(client(async () => response(value)).prepareCommit(input()).send(),
      (error) => safe(error, "invalid_response", "unknown"));
  }
});

test("usage receipt numeric source lexemes retain exact signed and unsigned safe-integer boundaries", async () => {
  for (const key of ["committed_at_us", "recorded_bytes", "kept_bytes"]) {
    for (const lexeme of ["1.0", "1e0", "1.0000000000000001", "9007199254740992", "-9007199254740992"]) {
      const value = ack(); value.outcome.usage[key] = 123;
      const wire = JSON.stringify(value).replace('"' + key + '":123', '"' + key + '":' + lexeme);
      await assert.rejects(client(async () => response(wire)).prepareCommit(input()).send(),
        (error) => safe(error, "invalid_response", "unknown"));
    }
  }
  for (const sign of [-1, 1]) {
    const value = ack();
    value.outcome.usage.committed_at_us = sign * Number.MAX_SAFE_INTEGER;
    value.outcome.usage.recorded_bytes = 0;
    value.outcome.usage.kept_bytes = Number.MAX_SAFE_INTEGER;
    const result = await client(async () => response(value)).prepareCommit(input()).send();
    assert.deepEqual(result.usage, value.outcome.usage);
  }
});
