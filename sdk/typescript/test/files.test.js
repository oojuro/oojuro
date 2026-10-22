import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { inspect } from "node:util";
import {store,clientOptions} from "./support/file-model.js";
import { test } from "node:test";
import { OojuroClient, OojuroError } from "../index.js";
import { FILE_PART_BYTES, FILE_MAX_BYTES, prepareFileUpload, downloadFile, downloadFileTo, fileManifestName } from "../files.js";

const W = "2".repeat(64), P = "1".repeat(64), U = "5".repeat(64), TOKEN = "ojv2_" + "A".repeat(43);
const PRIVATE = "synthetic-private-file-name";
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const data = (n = 2 * 1024 * 1024 + 123) => Uint8Array.from({ length: n }, (_, i) => (i * 37 + 19) % 256);
async function* source(bytes, step = 17011) {
  for (let i = 0; i < bytes.length; i += step) yield bytes.subarray(i, i + step);
}
const descriptor = (bytes, extra = {}) => ({ uploadId: U, writer: W, name: PRIVATE, expected: null,
  size: bytes.length, sha256: sha(bytes), consent: "intentional-user-file", ...extra });
function client(state) { return new OojuroClient(clientOptions(state)); }
function sink(extra = {}) {
  const out = { parts: [], committed: 0, aborted: 0,
    async write(bytes) { this.parts.push(bytes.slice()); },
    async commit(receipt) { this.committed++; this.receipt = receipt; },
    async abort() { this.aborted++; this.parts = []; }, ...extra };
  return out;
}
function assertSafe(error, outcome = "none") {
  assert.ok(error instanceof OojuroError);
  assert.equal(error.outcome, outcome);
  for (const text of [error.message, error.stack, inspect(error), JSON.stringify(error)]) {
    assert.ok(!text.includes(TOKEN)); assert.ok(!text.includes(PRIVATE));
  }
  assert.equal(error.cause, undefined);
  return true;
}
const joined = (parts) => Buffer.concat(parts.map((part) => Buffer.from(part)));

// This transport models exact-request persistence and current auth, not a real server/provider.
test("file larger than 2MiB composes bounded parts, reopens model and commits exact transactional download", async () => {
  const state = store(), bytes = data(), input = descriptor(bytes), sdk = client(state);
  const operation = prepareFileUpload(sdk, input);
  input.name = "caller mutation must not alter frozen descriptor";
  assert.ok(!inspect(operation).includes(PRIVATE)); assert.ok(!JSON.stringify(operation).includes(PRIVATE));
  const ack = await operation.send(source(bytes));
  assert.equal(state.blobs.size, 10);
  for (const row of state.calls) {
    const item = JSON.parse(row.body).mutations[0];
    assert.ok(item.bytes.length <= FILE_PART_BYTES || item.name.includes("/manifests/"));
    assert.equal(item.expected, null);
  }
  const target = sink();
  const read = await downloadFileTo(client(state), { writer: W, name: PRIVATE, minGeneration: ack.generation }, target);
  assert.equal(target.committed, 1); assert.equal(target.aborted, 0);
  assert.deepEqual(joined(target.parts), Buffer.from(bytes));
  assert.equal(read.sha256, sha(bytes)); assert.equal(read.size, bytes.length);
});

test("lost part ACK stops immediately and recreated operation replays exact IDs and original bytes", async () => {
  const state = store(), bytes = data(); let lost = false;
  state.after = async (input) => { if (!lost) { lost = true; throw new Error(TOKEN + PRIVATE); } };
  await assert.rejects(prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes)), (e) => assertSafe(e, "unknown"));
  assert.equal(state.calls.length, 1); assert.equal(state.blobs.size, 1);
  const first = state.calls[0].body;
  state.after = null;
  await prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes));
  assert.equal(state.calls[1].body, first);
  assert.equal(state.blobs.size, 10); assert.equal(state.requests.size, 10);
});

test("lost manifest ACK retains original identity and exact retry never republishes", async () => {
  const state = store(), bytes = data(2 * FILE_PART_BYTES + 1); let lost = false;
  state.after = async (input) => { if (input.request_id.endsWith("/manifest") && !lost) { lost = true; throw new Error(PRIVATE); } };
  await assert.rejects(prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes)), (e) => assertSafe(e, "unknown"));
  const generation = state.generation;
  assert.ok(state.blobs.has(fileManifestName(PRIVATE)));
  state.after = null;
  const ack = await prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes));
  assert.equal(ack.generation, generation); assert.equal(state.generation, generation);
});

test("changed replay conflicts and never replaces original part or publishes manifest", async () => {
  const state = store(), bytes = data(FILE_PART_BYTES + 1);
  state.after = async () => { throw new Error(PRIVATE); };
  await assert.rejects(prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes)));
  const retained = state.blobs.values().next().value.bytes.slice();
  const changed = bytes.slice(); changed[0] ^= 255; state.after = null;
  await assert.rejects(prepareFileUpload(client(state), descriptor(bytes)).send(source(changed)), (e) => assertSafe(e, "unknown"));
  assert.deepEqual(state.blobs.values().next().value.bytes, retained);
  assert.equal(state.blobs.size, 1);
});

test("source length/full digest mismatch never writes manifest and does not clean acknowledged parts", async () => {
  for (const adjust of [{ size: FILE_PART_BYTES + 2 }, { sha256: "0".repeat(64) }]) {
    const state = store(), bytes = data(FILE_PART_BYTES + 1);
    await assert.rejects(prepareFileUpload(client(state), descriptor(bytes, adjust)).send(source(bytes)), (e) => assertSafe(e, "unknown"));
    assert.equal(state.blobs.size, 1);
    assert.ok(!state.blobs.has(fileManifestName(PRIVATE)));
  }
});

test("intentional versions require original manifest CAS; stale/create-only CAS cannot overwrite pointer", async () => {
  const state = store(), bytes = data(15);
  const first = await prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes));
  const pointer = state.blobs.get(fileManifestName(PRIVATE)).bytes.slice();
  const next = data(16);
  await assert.rejects(prepareFileUpload(client(state), descriptor(next, { uploadId: "6".repeat(64) })).send(source(next)), (e) => assertSafe(e, "unknown"));
  assert.deepEqual(state.blobs.get(fileManifestName(PRIVATE)).bytes, pointer);
  const ack = await prepareFileUpload(client(state), descriptor(next, { uploadId: "7".repeat(64), expected: first.revision })).send(source(next));
  assert.notEqual(ack.revision, first.revision);
});

test("current auth revocation between uploads denies the next part and stops before manifest", async () => {
  const state = store(), bytes = data(FILE_PART_BYTES + 1);
  state.after = async (_input, current) => { current.writable = false; };
  await assert.rejects(prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes)), (e) => assertSafe(e, "unknown"));
  assert.equal(state.blobs.size, 1); assert.equal(state.calls.length, 2);
  assert.ok(!state.blobs.has(fileManifestName(PRIVATE)));
});

test("read revocation after tentative part aborts destination and never commits", async () => {
  const state = store(), bytes = data(FILE_PART_BYTES + 1);
  await prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes));
  const target = sink({ async write(part) { this.parts.push(part.slice()); state.readable = false; } });
  await assert.rejects(downloadFileTo(client(state), { writer: W, name: PRIVATE }, target), (e) => assertSafe(e));
  assert.equal(target.committed, 0); assert.equal(target.aborted, 1); assert.equal(target.parts.length, 0);
});

test("revocation after last tentative part still fails final manifest/current auth check", async () => {
  const state = store(), bytes = data(1);
  await prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes));
  const target = sink({ async write() { state.readable = false; } });
  await assert.rejects(downloadFileTo(client(state), { writer: W, name: PRIVATE }, target));
  assert.equal(target.committed, 0); assert.equal(target.aborted, 1);
});

test("part tampering fails before any yielded bytes; wrong full digest aborts before commit", async () => {
  for (const mode of ["part", "full"]) {
    const state = store(), bytes = data(80);
    await prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes));
    if (mode === "part") state.blobs.values().next().value.bytes[0] ^= 255;
    else {
      const pointer = state.blobs.get(fileManifestName(PRIVATE));
      const manifest = JSON.parse(new TextDecoder().decode(pointer.bytes)); manifest.sha256 = "f".repeat(64);
      pointer.bytes = new TextEncoder().encode(JSON.stringify(manifest));
    }
    const target = sink();
    await assert.rejects(downloadFileTo(client(state), { writer: W, name: PRIVATE }, target), (e) => { assertSafe(e); assert.equal(e.code, "file_integrity"); return true; });
    assert.equal(target.committed, 0); assert.equal(target.aborted, 1);
  }
});

test("strict manifests reject foreign writer, duplicate/order/path references, original request changes and unknown fields before part reads", async () => {
  const edits = [
    (m) => { m.writer = "9".repeat(64); }, (m) => { m.parts[1].name = m.parts[0].name; },
    (m) => { m.parts[0].name = "../private-file"; }, (m) => { m.parts[0].index = 1; },
    (m) => { m.parts[0].bytes--; }, (m) => { m.parts[0].extra = true; },
    (m) => { m.request_id += "other"; }, (m) => { m.total_bytes = 1; }, (m) => { m.extra = "unknown"; },
  ];
  for (const edit of edits) {
    const state = store(), bytes = data(FILE_PART_BYTES + 1);
    await prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes));
    const pointer = state.blobs.get(fileManifestName(PRIVATE));
    const manifest = JSON.parse(new TextDecoder().decode(pointer.bytes)); edit(manifest);
    pointer.bytes = new TextEncoder().encode(JSON.stringify(manifest)); state.calls = [];
    const target = sink();
    await assert.rejects(downloadFileTo(client(state), { writer: W, name: PRIVATE }, target));
    assert.equal(state.calls.length, 1); assert.equal(target.committed, 0); assert.equal(target.aborted, 1);
  }
});

test("duplicate JSON properties and decimal integer lexemes are refused", async () => {
  for (const change of [(s) => s.replace('"format":', '"format":"discarded","format":'), (s) => s.replace('"total_bytes":1', '"total_bytes":1.0')]) {
    const state = store(), bytes = data(1);
    await prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes));
    const pointer = state.blobs.get(fileManifestName(PRIVATE));
    pointer.bytes = new TextEncoder().encode(change(new TextDecoder().decode(pointer.bytes)));
    await assert.rejects(downloadFileTo(client(state), { writer: W, name: PRIVATE }, sink()));
  }
});

test("pointer replacement during read prevents destination commit", async () => {
  const state = store(), bytes = data(1);
  await prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes));
  const target = sink({ async write() { state.blobs.get(fileManifestName(PRIVATE)).bytes = new TextEncoder().encode("{}"); } });
  await assert.rejects(downloadFileTo(client(state), { writer: W, name: PRIVATE }, target));
  assert.equal(target.committed, 0); assert.equal(target.aborted, 1);
});

test("advanced iterator early return rejects completion and cannot report a complete file", async () => {
  const state = store(), bytes = data(FILE_PART_BYTES + 1);
  await prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes));
  const read = await downloadFile(client(state), { writer: W, name: PRIVATE });
  for await (const _part of read.chunks) break;
  await assert.rejects(read.completion, (e) => { assertSafe(e); assert.equal(e.code, "file_incomplete"); return true; });
});

test("empty file has one manifest and verifies; explicit consent/limits and bounded chunks fail closed", async () => {
  const state = store(), empty = new Uint8Array();
  await prepareFileUpload(client(state), descriptor(empty)).send(source(empty));
  assert.equal(state.blobs.size, 1);
  const target = sink(); await downloadFileTo(client(state), { writer: W, name: PRIVATE }, target);
  assert.equal(target.committed, 1); assert.equal(target.parts.length, 0);
  for (const extra of [{ consent: false }, { expected: undefined }, { size: FILE_MAX_BYTES + 1 }, { uploadId: "../other" }]) {
    assert.throws(() => prepareFileUpload(client(store()), descriptor(empty, extra)), (e) => assertSafe(e));
  }
  const other = store(), big = data(FILE_PART_BYTES + 1);
  await assert.rejects(prepareFileUpload(client(other), descriptor(big)).send(source(big, big.length)), (e) => assertSafe(e));
  assert.equal(other.calls.length, 0);
});

test("wrong writer, abort, source and sink errors retain fixed private-content-free errors", async () => {
  const state = store(), bytes = data(1);
  await assert.rejects(prepareFileUpload(client(state), descriptor(bytes, { writer: "9".repeat(64) })).send(source(bytes)), (e) => assertSafe(e, "unknown"));
  const aborted = AbortSignal.abort(new Error(PRIVATE));
  await assert.rejects(prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes), { signal: aborted }), (e) => assertSafe(e));
  async function* broken() { throw new Error(TOKEN + PRIVATE); }
  await assert.rejects(prepareFileUpload(client(state), descriptor(bytes)).send(broken()), (e) => assertSafe(e));
  await prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes));
  const target = sink({ async write() { throw new Error(PRIVATE); }, async abort() { this.aborted++; throw new Error(TOKEN); } });
  await assert.rejects(downloadFileTo(client(state), { writer: W, name: PRIVATE }, target), (e) => assertSafe(e));
  assert.equal(target.committed, 0); assert.equal(target.aborted, 1);
});


test("changed original descriptor after lost part ACK conflicts even when all source bytes match", async () => {
  for (const changed of [{ name: "different logical file" }, { expected: "a".repeat(64) }, { size: FILE_PART_BYTES + 2 }, { sha256: "c".repeat(64) }]) {
    const state = store(), bytes = data(FILE_PART_BYTES + 1);
    state.after = async () => { throw new Error(PRIVATE); };
    await assert.rejects(prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes)));
    const old = [...state.blobs.keys()]; state.after = null;
    await assert.rejects(prepareFileUpload(client(state), descriptor(bytes, changed)).send(source(bytes)), (e) => assertSafe(e, "unknown"));
    assert.deepEqual([...state.blobs.keys()], old);
    assert.equal(state.calls.length, 2);
  }
});

test("hostile descriptor/options/error getters never bypass fixed errors and descriptor reads once", async () => {
  const bytes = data(1), state = store();
  const input = descriptor(bytes); let reads = 0;
  Object.defineProperty(input, "name", { enumerable: true, get() { reads++; return PRIVATE; } });
  const operation = prepareFileUpload(client(state), input);
  await operation.send(source(bytes)); assert.equal(reads, 1);
  const bad = descriptor(bytes); Object.defineProperty(bad, "name", { enumerable: true, get() { throw new Error(PRIVATE + TOKEN); } });
  assert.throws(() => prepareFileUpload(client(store()), bad), (e) => assertSafe(e));
  await assert.rejects(operation.send(source(bytes), { get signal() { throw new Error(PRIVATE); } }), (e) => assertSafe(e));
  const error = new OojuroError("synthetic"); Object.defineProperty(error, "code", { get() { throw new Error(PRIVATE); } });
  async function* broken() { throw error; }
  await assert.rejects(operation.send(broken()), (e) => assertSafe(e));
});


test("Node Buffer chunks are owned before boundary awaits so mutation cannot publish an inconsistent file", async () => {
  const state = store(), bytes = data(FILE_PART_BYTES + 1);
  const first = Buffer.from(bytes.subarray(0, FILE_PART_BYTES - 1));
  const second = Buffer.from(bytes.subarray(FILE_PART_BYTES - 1));
  let mutated = false;
  state.after = async () => { if (!mutated) { mutated = true; second[1] ^= 255; } };
  async function* buffers() { yield first; yield second; }
  await prepareFileUpload(client(state), descriptor(bytes)).send(buffers());
  const target = sink(); await downloadFileTo(client(state), { writer: W, name: PRIVATE }, target);
  assert.deepEqual(joined(target.parts), Buffer.from(bytes));
  assert.equal(target.committed, 1);
});

test("advanced iterator cancellation before first next settles completion and makes no part reads", async () => {
  const state = store(), bytes = data(1);
  await prepareFileUpload(client(state), descriptor(bytes)).send(source(bytes));
  state.calls = [];
  const read = await downloadFile(client(state), { writer: W, name: PRIVATE });
  const iterator = read.chunks[Symbol.asyncIterator]();
  await iterator.return();
  const settled = await Promise.race([read.completion.then(() => "success", () => "rejected"), new Promise((yes) => setTimeout(() => yes("pending"), 25))]);
  assert.equal(settled, "rejected"); assert.equal(state.calls.length, 1);
});


test("strict expected revision pins manifest and each part including same-byte replacement", async () => {
  for (const mode of ["correct","manifest","part","missing","final"]) {
    const state=store(),bytes=data(FILE_PART_BYTES+1),sdk=client(state);
    const ack=await prepareFileUpload(sdk,descriptor(bytes)).send(source(bytes));
    const manifestName=fileManifestName(PRIVATE);
    if(mode==="manifest")state.blobs.get(manifestName).revision="e".repeat(64);
    if(mode==="part")state.blobs.values().next().value.revision="f".repeat(64);
    if(mode==="missing")state.omitRevision=true;
    if(mode==="final")state.afterRead=(name,current)=>{
      if(name!==manifestName)current.blobs.get(manifestName).revision="d".repeat(64);
    };
    const target=sink(),read=downloadFileTo(sdk,{writer:W,name:PRIVATE,expectedRevision:ack.revision},target);
    if(mode==="correct"){await read;assert.equal(target.committed,1);assert.deepEqual(joined(target.parts),Buffer.from(bytes));}
    else {await assert.rejects(read,e=>e.code==="file_integrity");assert.equal(target.committed,0);assert.equal(target.aborted,1);assert.equal(target.parts.length,0);}
  }
});
