import assert from "node:assert/strict";
import { test } from "node:test";
import { inspect } from "node:util";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { OojuroClient, OojuroError } from "../index.js";
const P = "1".repeat(64),
  W = "2".repeat(64),
  I = "3".repeat(64),
  D = "4".repeat(64);
const TOKEN = "ojv2_" + "A".repeat(43),
  PRIVATE = "private-framed-fixture",
  MEDIA = "application/vnd.oojuro.record.v1";
const MiB = 1024 * 1024,
  WIRE = 16 * MiB + 65536 + 4,
  encoder = new TextEncoder();
const mark = (generation = 7) => ({ generation, epoch: 2, transaction: { id: I, digest: D } });
const root = () => ({ schema: 1, minimum_reader: 1, project: P, namespace: D });
const input = (text = PRIVATE) => ({
  agent_id: "reported",
  parent_id: null,
  valid_at: 42,
  kind: "explicit",
  tool: null,
  status: null,
  exit: null,
  key: null,
  text,
  attrs: { reported: true },
  indexed: false,
});
const append = (text = PRIVATE) => ({
  requestId: "original-framed",
  consent: "intentional-user-file",
  expectedRoot: root(),
  writer: W,
  adapter: "reported",
  input: input(text),
});
const query = () => ({
  as_of: { from: -100, valid_at: 1000, known_at: 1000 },
  generation: null,
  writer: W,
  mode: { mode: "Range", versions: "current", filter: {} },
});
const metadata = () => {
  const { text, ...fields } = input();
  return { schema: 1, id: 9, writer: W, adapter: "reported", recorded_at: 100, ...fields };
};
const ack = () => ({
  outcome: {
    generation: 7,
    record_ids: { 0: I },
    blob_revisions: {},
    writer: null,
    key: null,
    destruction: null,
    usage: {
      request_tag: I,
      intent_fingerprint: D,
      scope_tag: W,
      committed_at_us: 100,
      recorded_bytes: 10,
      kept_bytes: 10,
    },
  },
});
const sdk = (fetch, extra = {}) =>
  new OojuroClient({
    baseUrl: "https://fixture.invalid",
    project: P,
    token: TOKEN,
    fetch,
    ...extra,
  });
const response = (body, type = "application/json", generation = 7, headers = {}) =>
  new Response(body, {
    headers: { "content-type": type, "x-oojuro-generation": String(generation), ...headers },
  });
const json = (value) => response(JSON.stringify(value));
function frame(meta, text) {
  const raw = typeof text === "string" ? encoder.encode(text) : text;
  const encoded =
    typeof meta === "string" ? encoder.encode(meta) : encoder.encode(JSON.stringify(meta));
  const body = new Uint8Array(4 + encoded.length + raw.length);
  new DataView(body.buffer).setUint32(0, encoded.length, false);
  body.set(encoded, 4);
  body.set(raw, 4 + encoded.length);
  return body;
}
const readFrame = (text) =>
  frame({ schema: 1, text_length: encoder.encode(text).length, id: I, record: metadata() }, text);
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
    return true;
  };
test("framed prepare freezes exact UTF8 and original identity, including mutation by transport", async () => {
  const bodies = [];
  let count = 0;
  const source = append("\ufeffUnicode 한글 🦉\0\n" + PRIVATE),
    original = structuredClone(source);
  const client = sdk(async (url, init) => {
    count++;
    bodies.push(init.body.slice());
    assert.equal(url, "https://fixture.invalid/v2/projects/" + P + "/journal/records/framed");
    assert.equal(init.headers["content-type"], MEDIA);
    assert.equal(init.headers["content-length"], String(init.body.length));
    assert.equal(init.headers["content-encoding"], "identity");
    assert.equal(init.headers.authorization, "Bearer " + TOKEN);
    assert.equal(init.redirect, "error");
    assert.equal(init.credentials, "omit");
    if (count === 1) {
      init.body.fill(0);
      throw Error(PRIVATE + TOKEN);
    }
    return json(ack());
  });
  const prepared = client.prepareJournalRecordFramed(source);
  source.input.text = "changed";
  source.expectedRoot.namespace = W;
  source.requestId = "changed";
  await assert.rejects(prepared.send(), safe("transport", "unknown"));
  assert.equal(count, 1);
  assert.deepEqual(await prepared.send(), ack().outcome);
  assert.deepEqual(await client.prepareJournalRecordFramed(original).send(), ack().outcome);
  assert.deepEqual(bodies[0], bodies[1]);
  assert.deepEqual(bodies[1], bodies[2]);
  const n = new DataView(bodies[0].buffer).getUint32(0, false);
  const m = JSON.parse(new TextDecoder().decode(bodies[0].subarray(4, 4 + n)));
  assert.equal(m.request_id, original.requestId);
  assert.equal(m.text_length, encoder.encode(original.input.text).length);
  assert.ok(!Object.hasOwn(m.input, "text"));
  assert.equal(
    new TextDecoder("utf-8", { ignoreBOM: true }).decode(bodies[0].subarray(4 + n)),
    original.input.text,
  );
  for (const value of [client, prepared])
    for (const text of [inspect(value), JSON.stringify(value)])
      assert.ok(!text.includes(PRIVATE) && !text.includes(TOKEN));
});
test("full16MiB NUL text uses compact frame and JSON limits remain unchanged", async () => {
  const text = "\0".repeat(16 * MiB);
  let sent = 0;
  const client = sdk(async (_url, init) => {
    sent++;
    const n = new DataView(init.body.buffer, init.body.byteOffset).getUint32(0, false);
    assert.equal(init.body.length, 4 + n + 16 * MiB);
    assert.ok(init.body.length < WIRE);
    assert.equal(init.body.at(-1), 0);
    return json(ack());
  });
  const prepared = client.prepareJournalRecordFramed(append(text));
  await prepared.send();
  assert.equal(sent, 1);
  assert.throws(() => client.prepareJournalRecord(append(text)), safe("invalid_request"));
  const decoded = await sdk(async () => response(readFrame(text), MEDIA)).journalRecordFramed({
    expectedRoot: root(),
    id: I,
  });
  assert.equal(decoded.value.record.text.length, text.length);
  assert.equal(decoded.value.record.text, text);
});
test("framed reads bind root/id/minimum and preserve BOM/Unicode/empty text exactly", async () => {
  for (const text of ["\ufefftext 한글 🦉\0", ""]) {
    const client = sdk(async (url, init) => {
      assert.equal(
        url,
        "https://fixture.invalid/v2/projects/" + P + "/journal/records/" + I + "/framed",
      );
      assert.equal(init.headers.accept, MEDIA);
      assert.equal(init.headers["x-oojuro-min-generation"], "6");
      assert.deepEqual(
        JSON.parse(
          Buffer.from(init.headers["x-oojuro-journal-root"], "base64url").toString("utf8"),
        ),
        root(),
      );
      assert.ok(!init.headers["x-oojuro-journal-root"].includes("="));
      assert.equal(init.body, undefined);
      return response(readFrame(text), MEDIA);
    });
    const value = await client.journalRecordFramed(
      { expectedRoot: root(), id: I },
      { minGeneration: 6 },
    );
    assert.equal(value.generation, 7);
    assert.equal(value.value.record.text, text);
    assert.equal(value.value.record.indexed, false);
  }
});
test("framed preflight rejects getters, invalid Unicode, metadata, text and separate budgets before send", async () => {
  let getters = 0,
    sent = 0;
  const client = sdk(async () => {
    sent++;
    throw Error(PRIVATE);
  });
  const getter = append();
  Object.defineProperty(getter.input, "text", {
    enumerable: true,
    get() {
      getters++;
      return PRIVATE;
    },
  });
  assert.throws(() => client.prepareJournalRecordFramed(getter), safe("invalid_request"));
  for (const change of [
    (v) => {
      v.input.text = "x".repeat(16 * MiB + 1);
    },
    (v) => {
      v.input.text = "한".repeat(Math.floor((16 * MiB) / 3) + 1);
    },
    (v) => {
      v.input.text = "\ud800";
    },
    (v) => {
      delete v.input.key;
    },
    (v) => {
      v.consent = "capture";
    },
    (v) => {
      v.input.attrs = {
        toJSON() {
          getters++;
          return {};
        },
      };
    },
    (v) => {
      v.expectedRoot.project = W;
    },
  ]) {
    const value = append();
    change(value);
    assert.throws(() => client.prepareJournalRecordFramed(value), safe("invalid_request"));
  }
  assert.throws(
    () => sdk(async () => {}, { maxFramedRequestBytes: 100 }).prepareJournalRecordFramed(append()),
    safe("invalid_request"),
  );
  for (const extra of [
    { maxFramedRequestBytes: WIRE + 1 },
    { maxFramedResponseBytes: WIRE + 1 },
    { maxFramedResponseBytes: 0 },
  ])
    assert.throws(() => sdk(async () => {}, extra), safe("invalid_configuration"));
  await assert.rejects(
    client.journalRecordFramed({ expectedRoot: { ...root(), project: W }, id: I }),
    safe("invalid_request"),
  );
  assert.equal(getters, 0);
  assert.equal(sent, 0);
});
test("malformed/truncated/misidentified frames, unsafe integers and duplicate fields refuse", async () => {
  const good = () => ({ schema: 1, text_length: 1, id: I, record: metadata() });
  const cases = [
    new Uint8Array(),
    new Uint8Array([0, 0, 0, 0]),
    new Uint8Array([0, 1, 0, 1]),
    frame({ ...good(), text_length: 2 }, "x"),
    frame({ ...good(), id: D }, "x"),
    frame({ ...good(), record: { ...metadata(), text: PRIVATE } }, "x"),
    frame(good(), new Uint8Array([255])),
    frame({ ...good(), extra: PRIVATE }, "x"),
    frame(JSON.stringify(good()).replace('"text_length":1', '"text_length":1.0'), "x"),
    frame(JSON.stringify(good()).replace('"id":9', '"id":9007199254740993'), "x"),
    frame(JSON.stringify(good()).replace('"schema":1', '"schema":1,"schema":1'), "x"),
    frame(JSON.stringify(good()).replace('"schema":1', '"schema":1,"\\u0073chema":1'), "x"),
  ];
  for (const body of cases)
    await assert.rejects(
      sdk(async () => response(body, MEDIA)).journalRecordFramed({ expectedRoot: root(), id: I }),
      safe("invalid_response"),
    );
  await assert.rejects(
    sdk(async () => response(readFrame("x"), MEDIA + "; charset=utf-8")).journalRecordFramed({
      expectedRoot: root(),
      id: I,
    }),
    safe("invalid_response"),
  );
  await assert.rejects(
    sdk(async () =>
      response(readFrame("x"), MEDIA, 7, { "content-encoding": "gzip" }),
    ).journalRecordFramed({ expectedRoot: root(), id: I }),
    safe("invalid_response"),
  );
});
test("framed clean EOF, lower budgets, abort and private-safe unknown writes remain bounded", async () => {
  const data = readFrame(PRIVATE);
  await assert.rejects(
    sdk(async () => response(data, MEDIA), { maxFramedResponseBytes: 100 }).journalRecordFramed({
      expectedRoot: root(),
      id: I,
    }),
    safe("response_too_large"),
  );
  await assert.rejects(
    sdk(async () =>
      response(data, MEDIA, 7, { "content-length": String(data.length + 1) }),
    ).journalRecordFramed({ expectedRoot: root(), id: I }),
    safe("truncated_response"),
  );
  const stream = new ReadableStream({
    start(c) {
      c.enqueue(data);
      c.error(Error(PRIVATE + TOKEN));
    },
  });
  await assert.rejects(
    sdk(async () => response(stream, MEDIA)).journalRecordFramed({ expectedRoot: root(), id: I }),
    safe("transport"),
  );
  const abort = new AbortController();
  abort.abort();
  await assert.rejects(
    sdk(async () => {
      throw Error("must not send");
    })
      .prepareJournalRecordFramed(append())
      .send({ signal: abort.signal }),
    safe("aborted", "unknown"),
  );
  await assert.rejects(
    sdk(async () => new Response(PRIVATE, { status: 409 }))
      .prepareJournalRecordFramed(append())
      .send(),
    safe("http", "unknown"),
  );
});
test("descriptor pages omit full text and keep projection-bound cursors and selection cutoff", async () => {
  const captured = JSON.parse(
    readFileSync(new URL("./support/journal-wire.json", import.meta.url), "utf8"),
  );
  const item = captured.cases.find((x) => x.operation === "descriptors"),
    request = item.request;
  const project = JSON.parse(captured.cases.find((x) => x.operation === "root").response_body).value
    .project;
  const client = new OojuroClient({
    baseUrl: "http://127.0.0.1",
    project,
    token: TOKEN,
    fetch: async (url, init) => {
      assert.equal(url, "http://127.0.0.1" + item.path);
      const sent = JSON.parse(init.body);
      assert.deepEqual(sent, { ...request, cursor: null });
      return new Response(item.response_body, {
        status: item.status,
        headers: item.response_headers,
      });
    },
  });
  const value = await client.journalDescriptorsPage({
    expectedRoot: request.expected_root,
    query: request.query,
    maxRecords: request.max_records,
    maxBytes: request.max_bytes,
  });
  assert.deepEqual(value, JSON.parse(item.response_body));
  assert.ok(!Object.hasOwn(value.value.records[0].record, "text"));
  for (const mutate of [
    (d) => {
      d.record.text = PRIVATE;
    },
    (d) => {
      d.text_bytes = 16 * MiB + 1;
    },
    (d) => {
      d.generation = 8;
    },
  ]) {
    const d = { id: I, record: metadata(), text_bytes: 16 * MiB, generation: 6 };
    mutate(d);
    await assert.rejects(
      sdk(async () =>
        json({ watermark: mark(), value: { watermark: mark(), records: [d], next: null } }),
      ).journalDescriptorsPage({ expectedRoot: root(), query: query() }),
      safe("invalid_response"),
    );
  }
  let calls = 0;
  const changed = sdk(async () => {
    calls++;
    return new Response(PRIVATE, { status: 409 });
  });
  await assert.rejects(
    changed.journalDescriptorsPage({
      expectedRoot: root(),
      query: query(),
      cursor: "full-page-cursor",
    }),
    safe("http"),
  );
  assert.equal(calls, 1);
});

test("actual Rust framed HTTP bodies and headers pass without converting text into JSON", async () => {
  const fixture = JSON.parse(
    readFileSync(new URL("./support/journal-framed-wire.json", import.meta.url), "utf8"),
  );
  assert.equal(fixture.schema, 1);
  const originalText = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(
    Buffer.from(fixture.text_base64, "base64"),
  );
  const m = fixture.metadata,
    original = {
      requestId: m.request_id,
      consent: m.consent,
      expectedRoot: m.expected_root,
      writer: m.writer,
      adapter: m.adapter,
      input: { ...m.input, text: originalText },
    };
  let verified = 0,
    appendAck;
  for (const item of fixture.cases) {
    const client = new OojuroClient({
      baseUrl: "http://127.0.0.1",
      project: fixture.root.project,
      token: TOKEN,
      fetch: async (url, init) => {
        assert.equal(url, "http://127.0.0.1" + item.path);
        assert.equal(init.method, item.method);
        const expected = Buffer.from(item.request_body_base64, "base64");
        if (item.method === "POST") {
          assert.equal(init.headers["content-type"], item.request_headers["content-type"]);
          assert.equal(init.headers["content-length"], String(expected.length));
          const actual = init.body,
            n = new DataView(actual.buffer, actual.byteOffset).getUint32(0, false);
          const en = expected.readUInt32BE(0);
          assert.deepEqual(
            JSON.parse(new TextDecoder().decode(actual.subarray(4, 4 + n))),
            JSON.parse(expected.subarray(4, 4 + en).toString("utf8")),
          );
          assert.deepEqual(Buffer.from(actual.subarray(4 + n)), expected.subarray(4 + en));
        } else {
          assert.equal(init.headers.accept, item.request_headers.accept);
          assert.deepEqual(
            JSON.parse(
              Buffer.from(init.headers["x-oojuro-journal-root"], "base64url").toString("utf8"),
            ),
            fixture.root,
          );
        }
        return new Response(Buffer.from(item.response_body_base64, "base64"), {
          status: item.status,
          headers: item.response_headers,
        });
      },
    });
    if (item.operation === "appendFramed") {
      appendAck = await client.prepareJournalRecordFramed(original).send();
      assert.deepEqual(
        appendAck,
        JSON.parse(Buffer.from(item.response_body_base64, "base64").toString("utf8")).outcome,
      );
    } else if (item.operation === "getFramed") {
      const body = Buffer.from(item.response_body_base64, "base64"),
        n = body.readUInt32BE(0),
        metadata = JSON.parse(body.subarray(4, 4 + n).toString("utf8"));
      const result = await client.journalRecordFramed({
        expectedRoot: fixture.root,
        id: metadata.id,
      });
      assert.equal(result.value.record.text, originalText);
      assert.deepEqual(
        { ...result.value.record, text: undefined },
        { ...metadata.record, text: undefined },
      );
      assert.equal(result.generation, Number(item.response_headers["x-oojuro-generation"]));
    } else assert.fail("unexpected actual framed operation");
    verified++;
  }
  assert.equal(verified, 2);
  assert.deepEqual(Object.keys(fixture.controls).sort(), ["json_conflict", "json_retry"]);
  if (fixture.controls) {
    for (const [name, item] of Object.entries(fixture.controls)) {
      let calls = 0;
      const client = new OojuroClient({
        baseUrl: "http://127.0.0.1",
        project: fixture.root.project,
        token: TOKEN,
        fetch: async (url, init) => {
          calls++;
          assert.equal(url, "http://127.0.0.1" + item.path);
          assert.deepEqual(
            JSON.parse(init.body),
            JSON.parse(Buffer.from(item.request_body_base64, "base64").toString("utf8")),
          );
          const value = Buffer.from(item.response_body_base64, "base64");
          return item.status === 200
            ? response(
                value,
                "application/json",
                JSON.parse(value.toString("utf8")).outcome.generation,
              )
            : new Response(value, { status: item.status });
        },
      });
      const wire = JSON.parse(Buffer.from(item.request_body_base64, "base64").toString("utf8"));
      const prepared = client.prepareJournalRecord({
        requestId: wire.request_id,
        consent: wire.consent,
        expectedRoot: wire.expected_root,
        writer: wire.writer,
        adapter: wire.adapter,
        input: wire.input,
      });
      if (name === "json_retry") assert.deepEqual(await prepared.send(), appendAck);
      else {
        assert.equal(name, "json_conflict");
        await assert.rejects(prepared.send(), safe("http", "unknown"));
      }
      assert.equal(calls, 1);
    }
  }
});

test("framed collector owns each delivered chunk before next pull and never retains borrowed parts", async () => {
  const expected = readFrame(PRIVATE),
    source = expected.slice();
  let offset = 0,
    prior;
  const body = new ReadableStream(
    {
      pull(controller) {
        if (prior) prior.fill(0);
        if (offset === source.length) {
          controller.close();
          return;
        }
        // Small view over an unrelated large backing buffer must be copied immediately.
        const backing = new Uint8Array(64 * 1024);
        const view = backing.subarray(100, 101);
        view[0] = source[offset++];
        prior = view;
        controller.enqueue(view);
      },
    },
    { highWaterMark: 0 },
  );
  const result = await sdk(async () => response(body, MEDIA)).journalRecordFramed({
    expectedRoot: root(),
    id: I,
  });
  assert.equal(result.value.record.text, PRIVATE);
});
test("framed collector refuses empty and SharedArrayBuffer chunks before a completed result", async () => {
  for (const chunk of [new Uint8Array(), new Uint8Array(new SharedArrayBuffer(1))]) {
    let calls = 0;
    const body = new ReadableStream(
      {
        pull(controller) {
          calls++;
          controller.enqueue(chunk);
        },
      },
      { highWaterMark: 0 },
    );
    await assert.rejects(
      sdk(async () => response(body, MEDIA)).journalRecordFramed({ expectedRoot: root(), id: I }),
      safe("invalid_response"),
    );
    assert.equal(calls, 1);
  }
});

test("framed chunk intrinsic brands refuse cross-realm shared buffers without invoking spoofed getters", async () => {
  const encoded = readFrame(PRIVATE),
    shared = runInNewContext("new SharedArrayBuffer(" + encoded.length + ")");
  const view = new Uint8Array(shared);
  view.set(encoded);
  assert.equal(shared instanceof SharedArrayBuffer, false);
  const body = new ReadableStream({
    start(c) {
      c.enqueue(view);
      c.close();
    },
  });
  await assert.rejects(
    sdk(async () => response(body, MEDIA)).journalRecordFramed({ expectedRoot: root(), id: I }),
    safe("invalid_response"),
  );
  let getters = 0;
  const ordinary = encoded.slice();
  for (const name of ["buffer", "length", "byteOffset"])
    Object.defineProperty(ordinary, name, {
      get() {
        getters++;
        throw Error(PRIVATE);
      },
    });
  const ownedBody = new ReadableStream({
    start(c) {
      c.enqueue(ordinary);
      c.close();
    },
  });
  assert.equal(
    (
      await sdk(async () => response(ownedBody, MEDIA)).journalRecordFramed({
        expectedRoot: root(),
        id: I,
      })
    ).value.record.text,
    PRIVATE,
  );
  assert.equal(getters, 0);
});

test("hostile SDK-error code/status getters cannot escape private-safe read/write boundaries", async () => {
  for (const field of ["code", "status"]) {
    const hostile = new OojuroError("transport", { status: 503 });
    Object.defineProperty(hostile, field, {
      get() {
        throw Error(PRIVATE + TOKEN);
      },
    });
    const client = sdk(async () => {
      throw hostile;
    });
    await assert.rejects(
      client.journalRecordFramed({ expectedRoot: root(), id: I }),
      safe("transport"),
    );
    await assert.rejects(
      client.prepareJournalRecordFramed(append()).send(),
      safe("transport", "unknown"),
    );
  }
});

test("stateful SDK-error accessors cannot substitute private code or status after validation", async () => {
  for (const field of ["code", "status"]) {
    let reads = 0;
    const hostile = new OojuroError("http", { status: 503 });
    Object.defineProperty(hostile, field, {
      get() {
        reads++;
        return field === "code" ? (reads === 1 ? "http" : PRIVATE) : reads < 4 ? 503 : PRIVATE;
      },
    });
    const client = sdk(async () => {
      throw hostile;
    });
    await assert.rejects(client.journalRecordFramed({ expectedRoot: root(), id: I }), (error) => {
      assert.ok(error instanceof OojuroError);
      for (const text of [error.message, inspect(error), JSON.stringify(error)])
        assert.ok(!text.includes(PRIVATE));
      return true;
    });
    assert.equal(
      reads,
      0,
      "normalization must inspect data descriptors without invoking accessors",
    );
  }
});
