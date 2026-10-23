import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { inspect } from "node:util";
import { OojuroClient, OojuroCaptureClient, OojuroProjectClient } from "../index.js";
import { prepareJournalAttachment, downloadJournalAttachmentTo, ATTACHMENT_MAX_BYTES } from "../attachments.js";
import { store, clientOptions, P, W } from "./support/file-model.js";
const id = c => c.repeat(64), J = id("8"), A = id("3");
const sha = b => createHash("sha256").update(b).digest("hex");
const payload = n => Uint8Array.from({ length: n }, (_, i) => (i * 37 + 19) % 256);
async function* source(bytes) {
  for (let i = 0; i < bytes.length; i += 32768) yield bytes.subarray(i, i + 32768);
}
const descriptor = bytes => ({ uploadId: id("a"), eventId: id("b"), journal: J, runId: id("c"),
  writer: W, agentId: A, displayName: "selected-synthetic.bin", size: bytes.length,
  sha256: sha(bytes), validAt: 42, consent: "intentional-user-file" });
const json = (value, generation = 1) => new Response(JSON.stringify(value), {
  headers: { "content-type": "application/json", "x-oojuro-generation": String(generation) } });
function fixture() {
  const state = store(); state.generation = 1; state.visible = true; state.events = new Map(); state.captureRequests = new Map();
  state.root = { schema: 3, minimum_reader: 3, project: P, namespace: J, ownership: { account: "fixture_owner", agent: A } };
  const native = clientOptions(state);
  const uploader = new OojuroClient({ ...native, fetch: async (url, init) => {
    if (url.endsWith("journal/root")) return json({ watermark: { generation: state.generation, epoch: 1,
      transaction: { id: id("d"), digest: id("e") } }, value: state.root }, state.generation);
    return native.fetch(url, init);
  } });
  const capture = new OojuroCaptureClient({ baseUrl: native.baseUrl, token: `ojc_${P}_${"A".repeat(43)}`,
    fetch: async (_url, init) => {
      if (init.method === "GET") return json({ format: 1, journal: J, agent_id: A, key_id: id("f"), producer_id: W,
        label: "synthetic", expires_us: 999999999999999,
        limits: { events: 100, metadata_bytes: 65536, text_bytes: 16777216 } });
      const wire = Buffer.from(init.body), n = wire.readUInt32BE(0), meta = JSON.parse(wire.subarray(4, 4 + n));
      const old = state.captureRequests.get(meta.request_id);
      if (old && !old.wire.equals(wire)) return new Response(null, { status: 409 });
      let ack = old?.ack;
      if (!ack) {
        const event = meta.events[0], record = sha(Buffer.from(event.event_id));
        event.text = wire.subarray(4 + n).toString();
        state.events.set(record, event);
        ack = { request_id: meta.request_id, generation: ++state.generation, recorded_bytes: 0,
          records: [{ event_id: event.event_id, record_id: record, replayed: false }] };
        state.captureRequests.set(meta.request_id, { wire, ack });
      }
      if (state.loseEventAck) { state.loseEventAck = false; throw Error("private dropped reply"); }
      return json(ack);
    } });
  const project = new OojuroProjectClient({ baseUrl: native.baseUrl, project: id("9"), token: "ojp_" + "A".repeat(43),
    fetch: async (url, init) => {
      state.projectCalls = (state.projectCalls ?? 0) + 1;
      assert.equal(init.credentials, "omit"); assert.equal(init.redirect, "error");
      const path = new URL(url).pathname.split("/"), journal = path[5], record = path[7];
      if (!state.visible || journal !== J || !state.events.has(record)) return new Response(null, { status: 404 });
      const event = state.events.get(record), file = event.input.attrs.oojuro_attachment;
      const manifest = state.blobs.get(file.manifest_name);
      if (!manifest) return new Response(null, { status: 404 });
      const value = path[9] === "manifest" ? manifest : state.blobs.get(JSON.parse(Buffer.from(manifest.bytes)).parts[Number(path[10])].name);
      const headers = { "content-type": "application/octet-stream", "x-oojuro-generation": String(state.generation) };
      if (!state.omitRevision) headers["x-oojuro-blob-revision"] = value.revision;
      return new Response(value.bytes.slice(), { headers });
    } });
  return { state, uploader, capture, project };
}
function sink(extra = {}) {
  return { chunks: [], committed: 0, aborted: 0,
    async write(bytes) { this.chunks.push(bytes.slice()); },
    async commit() { this.committed++; }, async abort() { this.aborted++; this.chunks = []; }, ...extra };
}
test("selected bytes publish one immutable version and a small event; project download verifies exact bytes", async () => {
  const f = fixture(), bytes = payload(2 * 1024 * 1024 + 37), input = descriptor(bytes);
  const operation = prepareJournalAttachment(f.uploader, f.capture, input);
  assert.ok(!JSON.stringify(operation).includes(input.displayName)); assert.ok(!inspect(operation).includes(input.displayName));
  input.displayName = "changed after preparation";
  const receipt = await operation.send(source(bytes));
  const event = f.state.events.get(receipt.recordId);
  assert.equal(event.text, "Attachment: selected-synthetic.bin");
  assert.ok(JSON.stringify(event).length < 2048); assert.equal(event.input.text, "");
  assert.equal(f.state.blobs.size, 10); assert.equal(f.state.events.size, 1);
  const target = sink();
  const result = await downloadJournalAttachmentTo(f.project, receipt, target);
  assert.equal(result.sha256, sha(bytes)); assert.equal(target.committed, 1); assert.equal(target.aborted, 0);
  assert.deepEqual(Buffer.concat(target.chunks), Buffer.from(bytes));
});
test("same display name creates independent retained versions; lost event ACK replays without new originals", async () => {
  const f = fixture(), bytes = payload(262145), input = descriptor(bytes);
  f.state.loseEventAck = true;
  await assert.rejects(prepareJournalAttachment(f.uploader, f.capture, input).send(source(bytes)), e => e.outcome === "unknown");
  const count = f.state.blobs.size, generation = f.state.generation;
  const first = await prepareJournalAttachment(f.uploader, f.capture, input).send(source(bytes));
  assert.equal(f.state.blobs.size, count); assert.equal(f.state.events.size, 1); assert.equal(f.state.generation, generation);
  const next = payload(262146), second = await prepareJournalAttachment(f.uploader, f.capture,
    { ...descriptor(next), uploadId: id("d"), eventId: id("e") }).send(source(next));
  assert.notEqual(first.file.manifest_name, second.file.manifest_name);
  for (const [receipt, original] of [[first, bytes], [second, next]]) {
    const target = sink(); await downloadJournalAttachmentTo(f.project, receipt, target);
    assert.deepEqual(Buffer.concat(target.chunks), Buffer.from(original));
  }
});
test("revocation after tentative bytes or deleting manifest aborts destination; missing revision refuses", async () => {
  for (const kind of ["membership", "manifest", "revision"]) {
    const f = fixture(), bytes = payload(262145), receipt = await prepareJournalAttachment(f.uploader, f.capture, descriptor(bytes)).send(source(bytes));
    const target = sink({ async write(bytes) {
      this.chunks.push(bytes.slice());
      if (kind === "membership") f.state.visible = false;
      if (kind === "manifest") f.state.blobs.delete(receipt.file.manifest_name);
    } });
    if (kind === "revision") f.state.omitRevision = true;
    await assert.rejects(downloadJournalAttachmentTo(f.project, receipt, target));
    assert.equal(target.committed, 0); assert.equal(target.aborted, 1); assert.equal(target.chunks.length, 0);
    assert.ok(f.state.projectCalls > 0);
    if (kind !== "revision") assert.ok(f.state.projectCalls >= 3, "failure follows a real tentative part");
  }
});
test("consent, destination, journal identity and size limits are validated before writes", async () => {
  const f = fixture(), bytes = payload(1), input = descriptor(bytes);
  for (const extra of [{ consent: "capture-consent" }, { size: ATTACHMENT_MAX_BYTES + 1 }, { journal: "../" }, { displayName: "bad\nname" }])
    assert.throws(() => prepareJournalAttachment(f.uploader, f.capture, { ...input, ...extra }));
  assert.throws(() => prepareJournalAttachment(f.uploader,
    new OojuroCaptureClient({ baseUrl: "https://other.invalid", token: `ojc_${P}_${"A".repeat(43)}` }), input));
  await assert.rejects(prepareJournalAttachment(f.uploader, f.capture, { ...input, writer: id("7") }).send(source(bytes)));
  assert.equal(f.state.blobs.size, 0);
  const target = sink();
  await assert.rejects(downloadJournalAttachmentTo(f.project, { journal: J, recordId: id("f"), file: {} }, target));
  assert.equal(f.state.projectCalls ?? 0, 0); assert.equal(target.aborted, 1);
});
test("owned roots 2/3 validate exact ownership/minimum-reader while schema1 remains compatible", async () => {
  const f = fixture();
  for (const schema of [1, 2, 3]) {
    f.state.root = { ...f.state.root, schema, minimum_reader: schema };
    if (schema === 1) delete f.state.root.ownership;
    else f.state.root.ownership = { account: "fixture_owner", agent: A };
    assert.equal((await f.uploader.journalRoot()).value.schema, schema);
  }
  for (const change of [{ minimum_reader: 2 }, { ownership: { account: "bad account", agent: A } }, { ownership: { account: "fixture", agent: "bad" } }]) {
    const saved = f.state.root; f.state.root = { ...saved, ...change };
    await assert.rejects(f.uploader.journalRoot()); f.state.root = saved;
  }
});
