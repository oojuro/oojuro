import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { inspect } from "node:util";
import { test } from "node:test";
import { OojuroClient, OojuroError } from "../index.js";
import { FILE_PART_BYTES, prepareFileUpload, fileManifestName } from "../files.js";
import { planFileDeletion, prepareFileDeletion, OojuroFileDeletionError } from "../file-deletion.js";
const W = "2".repeat(64), P = "1".repeat(64), U = "5".repeat(64), D = "6".repeat(64);
const TOKEN = "ojv2_" + "A".repeat(43), PRIVATE = "synthetic-private-selected-file";
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const data = (n = FILE_PART_BYTES + 1) => Uint8Array.from({ length: n }, (_, i) => (i * 13 + 17) % 256);
async function* source(bytes) { for (let i = 0; i < bytes.length; i += FILE_PART_BYTES) yield bytes.subarray(i, i + FILE_PART_BYTES); }
function store() { return { blobs: new Map(), requests: new Map(), calls: [], generation: 0, read: true, write: true, after: null }; }
function client(state) {
  return new OojuroClient({ baseUrl: "https://synthetic.invalid", project: P, token: TOKEN,
    fetch: async (url, init) => {
      const path = new URL(url); state.calls.push({ path, body: init.body });
      if (init.method === "GET") {
        if (!state.read || path.searchParams.get("writer") !== W) return new Response(null, { status: 403 });
        const row = state.blobs.get(path.searchParams.get("name"));
        if (!row) return new Response(null, { status: 404 });
        return new Response(row.bytes.slice(), { headers: { "content-type": "application/octet-stream", "x-oojuro-generation": String(state.generation) } });
      }
      if (!state.write) return new Response(null, { status: 401 });
      const input = JSON.parse(init.body), item = input.mutations[0];
      if (input.scope !== W || item.writer !== W) return new Response(null, { status: 403 });
      const old = state.requests.get(input.request_id);
      let response;
      if (old) {
        if (old.body !== init.body) return new Response(null, { status: 409 });
        response = old.response;
      } else {
        const row = state.blobs.get(item.name);
        if ((row?.revision ?? null) !== item.expected) return new Response(null, { status: 409 });
        if (item.kind === "Blob") state.blobs.set(item.name, { revision: sha(init.body), bytes: Uint8Array.from(item.bytes) });
        else if (item.kind === "DeleteBlob" && row) state.blobs.delete(item.name);
        else return new Response(null, { status: 409 });
        const generation = ++state.generation;
        response = { outcome: { generation, record_ids: {}, blob_revisions: item.kind === "Blob" ? { 0: state.blobs.get(item.name).revision } : {}, writer: null, key: null, destruction: null, usage: { request_tag: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", intent_fingerprint: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", scope_tag: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc", committed_at_us: 1, recorded_bytes: 0, kept_bytes: 0 } } };
        state.requests.set(input.request_id, { body: init.body, response });
      }
      if (state.after) await state.after(input, state);
      return new Response(JSON.stringify(response), { headers: { "content-type": "application/json", "x-oojuro-generation": String(response.outcome.generation) } });
    } });
}
async function uploaded(n, state = store(), uploadId = U, expected = null) {
  const bytes = data(n), sdk = client(state);
  const ack = await prepareFileUpload(sdk, { uploadId, writer: W, name: PRIVATE, expected, size: bytes.length, sha256: sha(bytes), consent: "intentional-user-file" }).send(source(bytes));
  return { state, sdk, ack };
}
async function planned(fixture) {
  return planFileDeletion(fixture.sdk, { deleteId: D, writer: W, name: PRIVATE, manifestRevision: fixture.ack.revision, minGeneration: fixture.ack.generation, consent: "delete-selected-user-file" });
}
function safe(error, outcome = "none") {
  assert.ok(error instanceof OojuroError); assert.equal(error.outcome, outcome);
  for (const text of [error.message, error.stack, inspect(error), JSON.stringify(error)]) { assert.ok(!text.includes(PRIVATE)); assert.ok(!text.includes(TOKEN)); }
  return true;
}
const mutations = (state) => state.calls.filter((c) => c.body).map((c) => JSON.parse(c.body));

test("explicit selection of >2MiB file separates manifest ACK from exact selected-part cleanup", async () => {
  const f = await uploaded(2 * 1024 * 1024 + 31), descriptor = await planned(f);
  const count = f.state.blobs.size; f.state.calls = [];
  const operation = prepareFileDeletion(f.sdk, descriptor);
  assert.ok(!inspect(operation).includes(PRIVATE)); assert.ok(!JSON.stringify(operation).includes(PRIVATE));
  const logical = await operation.removeManifest();
  assert.equal(logical.logicalFileDeleted, true); assert.equal(logical.cleanupComplete, false);
  assert.equal(logical.remainingParts, count - 1); assert.equal(f.state.blobs.size, count - 1);
  assert.equal(f.state.calls.length, 1);
  const done = await operation.cleanupParts();
  assert.equal(done.logicalFileDeleted, true); assert.equal(done.cleanupComplete, true); assert.equal(done.remainingParts, 0);
  assert.equal(f.state.blobs.size, 0);
  assert.ok(mutations(f.state).every((r) => r.mutations[0].kind === "DeleteBlob" && typeof r.mutations[0].expected === "string"));
});

test("planner is read-only and cleanup cannot run before an original manifest-delete ACK", async () => {
  const f = await uploaded(3); f.state.calls = [];
  const descriptor = await planned(f); assert.equal(f.state.calls.length, 1); assert.equal(mutations(f.state).length, 0);
  f.state.calls = [];
  const operation = prepareFileDeletion(f.sdk, descriptor);
  await assert.rejects(operation.cleanupParts(), (e) => { safe(e); assert.equal(e.code, "file_delete_manifest_unconfirmed"); assert.equal(e.progress.logicalFileDeleted, false); return true; });
  assert.equal(f.state.calls.length, 0); assert.equal(f.state.blobs.size, 2);
});

test("lost manifest ACK stops before cleanup; restart uses durable original descriptor without rereading missing manifest", async () => {
  const f = await uploaded(FILE_PART_BYTES + 1), descriptor = await planned(f); f.state.calls = [];
  let lost = false;
  f.state.after = async () => { if (!lost) { lost = true; throw new Error(TOKEN + PRIVATE); } };
  const operation = prepareFileDeletion(f.sdk, descriptor);
  await assert.rejects(operation.removeManifest(), (e) => { safe(e, "unknown"); assert.equal(e.progress.logicalFileDeleted, false); return true; });
  assert.equal(f.state.calls.length, 1); assert.ok(!f.state.blobs.has(fileManifestName(PRIVATE)));
  await assert.rejects(operation.cleanupParts()); assert.equal(f.state.calls.length, 1);
  const originalBody = f.state.calls[0].body; f.state.after = null;
  const reopened = prepareFileDeletion(client(f.state), JSON.parse(JSON.stringify(descriptor)));
  const logical = await reopened.removeManifest(); assert.equal(logical.logicalFileDeleted, true);
  assert.equal(f.state.calls[1].body, originalBody);
  const done = await reopened.cleanupParts(); assert.equal(done.cleanupComplete, true); assert.equal(f.state.blobs.size, 0);
});

test("lost cleanup ACK reports remaining and restart replays exact delete IDs, preserving a replacement part", async () => {
  const f = await uploaded(FILE_PART_BYTES + 1), descriptor = await planned(f), operation = prepareFileDeletion(f.sdk, descriptor);
  await operation.removeManifest(); f.state.calls = [];
  const manifest = JSON.parse(descriptor.manifest), name = manifest.parts[0].name;
  let lost = false;
  f.state.after = async () => { if (!lost) { lost = true; throw new Error(PRIVATE); } };
  await assert.rejects(operation.cleanupParts(), (e) => { safe(e, "unknown"); assert.equal(e.progress.logicalFileDeleted, true); assert.equal(e.progress.cleanupComplete, false); assert.deepEqual(e.progress.remainingPartIndices, [0, 1]); return true; });
  assert.equal(f.state.calls.length, 1);
  const first = f.state.calls[0].body;
  f.state.blobs.set(name, { revision: "f".repeat(64), bytes: new Uint8Array([99]) });
  f.state.after = null;
  const reopened = prepareFileDeletion(client(f.state), descriptor);
  await reopened.removeManifest(); await reopened.cleanupParts();
  assert.equal(f.state.calls[2].body, first);
  assert.equal(reopened.progress.cleanupComplete, true); assert.equal(f.state.blobs.size, 1);
  assert.deepEqual(f.state.blobs.get(name).bytes, new Uint8Array([99]));
});

test("raced manifest revision blocks removal and every cleanup; raced part revision is preserved", async () => {
  for (const mode of ["manifest", "part"]) {
    const f = await uploaded(FILE_PART_BYTES + 1), descriptor = await planned(f), operation = prepareFileDeletion(f.sdk, descriptor);
    const manifest = JSON.parse(descriptor.manifest), name = mode === "manifest" ? fileManifestName(PRIVATE) : manifest.parts[0].name;
    if (mode === "part") await operation.removeManifest();
    f.state.blobs.set(name, { revision: "a".repeat(64), bytes: new Uint8Array([55]) }); f.state.calls = [];
    await assert.rejects(mode === "manifest" ? operation.removeManifest() : operation.cleanupParts(), (e) => { safe(e, "unknown"); assert.equal(e.status, 409); return true; });
    assert.equal(f.state.calls.length, 1); assert.deepEqual(f.state.blobs.get(name).bytes, new Uint8Array([55]));
    if (mode === "manifest") { await assert.rejects(operation.cleanupParts()); assert.equal(f.state.calls.length, 1); }
  }
});

test("fresh-namespace replacement after logical deletion survives selected old-part cleanup", async () => {
  const f = await uploaded(FILE_PART_BYTES + 1), descriptor = await planned(f), operation = prepareFileDeletion(f.sdk, descriptor);
  await operation.removeManifest();
  await uploaded(15, f.state, "7".repeat(64), null);
  const replacement = new Map([...f.state.blobs].filter(([name]) => name.includes("/manifests/") || name.includes("/7" + "7".repeat(63) + "/")));
  assert.equal(replacement.size, 2);
  await operation.cleanupParts();
  assert.equal(f.state.blobs.size, 2);
  for (const [name, value] of replacement) assert.deepEqual(f.state.blobs.get(name), value);
});

test("404 absence and missing retry receipt never free an unknown manifest hold", async () => {
  const f = await uploaded(1), descriptor = await planned(f), operation = prepareFileDeletion(f.sdk, descriptor);
  f.state.after = async () => { throw new Error(PRIVATE); };
  await assert.rejects(operation.removeManifest());
  f.state.after = null; f.state.requests.clear();
  await assert.rejects(operation.removeManifest(), (e) => { safe(e, "unknown"); assert.equal(e.progress.logicalFileDeleted, false); return true; });
  const count = f.state.calls.length;
  await assert.rejects(operation.cleanupParts()); assert.equal(f.state.calls.length, count);
  assert.equal(f.state.blobs.size, 1);
});

test("current authorization is checked for manifest and every selected part; partial progress stays explicit", async () => {
  const f = await uploaded(2 * FILE_PART_BYTES + 1), descriptor = await planned(f), operation = prepareFileDeletion(f.sdk, descriptor);
  f.state.write = false;
  await assert.rejects(operation.removeManifest(), (e) => { safe(e, "unknown"); assert.equal(e.progress.logicalFileDeleted, false); return true; });
  f.state.write = true; await operation.removeManifest(); f.state.calls = [];
  f.state.after = async (_request, current) => { current.write = false; };
  await assert.rejects(operation.cleanupParts(), (e) => { safe(e, "unknown"); assert.equal(e.progress.logicalFileDeleted, true); assert.equal(e.progress.acknowledgedParts, 1); assert.deepEqual(e.progress.remainingPartIndices, [1, 2]); return true; });
  assert.equal(f.state.calls.length, 2); assert.equal(f.state.blobs.size, 2);
  f.state.write = true; f.state.after = null;
  const done = await operation.cleanupParts(); assert.equal(done.cleanupComplete, true); assert.equal(f.state.blobs.size, 0);
});

test("selected descriptor snapshot rejects malformed schemas, altered manifest digests and foreign/path part references", async () => {
  const f = await uploaded(1), descriptor = await planned(f); f.state.calls = [];
  for (const edit of [
    (v) => { v.extra = true; }, (v) => { v.manifestSha256 = "f".repeat(64); },
    (v) => { v.consent = "trace-capture"; }, (v) => { v.manifestRevision = null; },
    (v) => { const m = JSON.parse(v.manifest); m.parts[0].name = "../foreign"; v.manifest = JSON.stringify(m); v.manifestSha256 = sha(v.manifest); },
    (v) => { const m = JSON.parse(v.manifest); m.writer = "9".repeat(64); v.manifest = JSON.stringify(m); v.manifestSha256 = sha(v.manifest); },
  ]) {
    const chosen = JSON.parse(JSON.stringify(descriptor)); edit(chosen);
    assert.throws(() => prepareFileDeletion(f.sdk, chosen), (e) => safe(e));
  }
  assert.equal(f.state.calls.length, 0);
  const mutable = { ...descriptor }, operation = prepareFileDeletion(f.sdk, mutable);
  mutable.manifestRevision = "0".repeat(64); mutable.name = "another logical file";
  await operation.removeManifest(); assert.ok(!f.state.blobs.has(fileManifestName(PRIVATE)));
});

test("explicit receipt and deletion consent are required; planning unauthorized reads never mutates", async () => {
  const f = await uploaded(1); f.state.calls = [];
  const input = { deleteId: D, writer: W, name: PRIVATE, manifestRevision: f.ack.revision, consent: "delete-selected-user-file" };
  await assert.rejects(planFileDeletion(f.sdk, { ...input, manifestRevision: undefined }), (e) => safe(e));
  await assert.rejects(planFileDeletion(f.sdk, { ...input, consent: "intentional-user-file" }), (e) => safe(e));
  assert.equal(f.state.calls.length, 0);
  f.state.read = false; await assert.rejects(planFileDeletion(f.sdk, input), (e) => safe(e));
  assert.equal(mutations(f.state).length, 0);
});

test("empty file requires no cleanup writes and still separates explicit cleanup stage", async () => {
  const f = await uploaded(0), operation = prepareFileDeletion(f.sdk, await planned(f));
  const before = f.state.calls.length;
  const logical = await operation.removeManifest(); assert.equal(logical.cleanupComplete, false); assert.equal(logical.remainingParts, 0);
  const done = await operation.cleanupParts(); assert.equal(done.cleanupComplete, true); assert.equal(f.state.calls.length - before, 1);
});

test("abort/hostile getters and source errors keep error progress bounded and content free", async () => {
  const f = await uploaded(1), descriptor = await planned(f), operation = prepareFileDeletion(f.sdk, descriptor);
  const bad = { ...descriptor }; Object.defineProperty(bad, "name", { enumerable: true, get() { throw new Error(PRIVATE + TOKEN); } });
  assert.throws(() => prepareFileDeletion(f.sdk, bad), (e) => safe(e));
  await assert.rejects(operation.removeManifest({ get signal() { throw new Error(PRIVATE); } }), (e) => { safe(e); assert.ok(e instanceof OojuroFileDeletionError); return true; });
  await assert.rejects(operation.removeManifest({ signal: AbortSignal.abort(new Error(TOKEN)) }), (e) => safe(e));
  assert.equal(operation.progress.logicalFileDeleted, false);
  await operation.removeManifest();
  const error = new OojuroError("bad"); Object.defineProperty(error, "status", { get() { throw new Error(PRIVATE); } });
  f.state.after = async () => { throw error; };
  await assert.rejects(operation.cleanupParts(), (e) => { safe(e, "unknown"); assert.equal(e.progress.remainingParts, 1); assert.ok(Object.isFrozen(e.progress.remainingPartIndices)); return true; });
});


test("replanned replacement selection cannot inherit historical delete ACK from an old manifest revision", async () => {
  const f = await uploaded(1), original = await planned(f), first = prepareFileDeletion(f.sdk, original);
  f.state.after = async () => { throw new Error(PRIVATE); };
  await assert.rejects(first.removeManifest());
  f.state.after = null;
  await uploaded(17, f.state, "7".repeat(64), null);
  const retained = new Map(f.state.blobs);
  const changed = await planned(f); // Same deleteId and old receipt R1, but now reads replacement M2.
  assert.notEqual(changed.manifestSha256, original.manifestSha256);
  const operation = prepareFileDeletion(client(f.state), changed);
  await assert.rejects(operation.removeManifest(), (e) => { safe(e, "unknown"); assert.equal(e.progress.logicalFileDeleted, false); return true; });
  await assert.rejects(operation.cleanupParts());
  assert.deepEqual(f.state.blobs, retained);
});
