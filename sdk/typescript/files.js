// Explicit Node24 user-file protocol over the existing encrypted v2 Blob API.
import { createHash } from "node:crypto";
import { OojuroClient, OojuroError } from "./index.js";
import { downloadVia, downloadToVia } from "./file-download.js";

import {
  FILE_PART_BYTES, FILE_MAX_PARTS, MANIFEST_MAX_BYTES, FORMAT,
  encoder, fail, hash, binding, partName, requestId, fileManifestName,
  opaque, label, fields, size,
} from "./file-format.js";
export { FILE_PART_BYTES, FILE_MAX_PARTS, FILE_MAX_BYTES, fileManifestName } from "./file-format.js";
const inspect = Symbol.for("nodejs.util.inspect.custom");
function descriptor(input) {
  try {
    const names = ["uploadId", "writer", "name", "expected", "size", "sha256", "consent"];
    fields(input, names);
    // Read caller properties exactly once, then validate the captured primitives.
    const value = Object.fromEntries(names.map((key) => [key, input[key]]));
    opaque(value.uploadId); opaque(value.writer); opaque(value.sha256); label(value.name);
    if (value.expected !== null) opaque(value.expected);
    size(value.size);
    if (value.consent !== "intentional-user-file") fail();
    return Object.freeze(value);
  } catch {
    fail();
  }
}
function abort(signal) {
  if (signal?.aborted) fail("file_aborted");
}
function safe(error, outcome = "none") {
  try {
    const allowed = ["file_invalid_request", "file_source_mismatch", "file_integrity", "file_aborted", "file_incomplete", "file_sink"];
    const own = error instanceof OojuroError;
    const code = own ? error.code : null;
    const status = own ? error.status : undefined;
    return new OojuroError(allowed.includes(code) ? code : "file_transport", {
      outcome, status: Number.isInteger(status) && status >= 100 && status <= 599 ? status : undefined,
    });
  } catch {
    return new OojuroError("file_transport", { outcome });
  }
}

export class PreparedFileUpload {
  #client;
  #input;
  #active = false;
  constructor(client, input) {
    if (!(client instanceof OojuroClient)) fail();
    this.#client = client;
    this.#input = descriptor(input);
  }
  toJSON() { return { type: "PreparedFileUpload" }; }
  [inspect]() { return "PreparedFileUpload {}"; }
  async send(source, supplied = {}) {
    if (this.#active) fail();
    this.#active = true;
    let attempted = false;
    try {
      if (!supplied || typeof supplied !== "object" || Object.keys(supplied).some((k) => k !== "signal")) fail();
      const signal = supplied.signal;
      abort(signal);
      if (!source || typeof source[Symbol.asyncIterator] !== "function") fail();
      const input = this.#input;
      const bound = binding(input);
      const full = createHash("sha256");
      const parts = [];
      let total = 0;
      let buffer = new Uint8Array(FILE_PART_BYTES);
      let used = 0;
      let generation = 0;
      const sendPart = async () => {
        const bytes = buffer.slice(0, used);
        const index = parts.length;
        if (index >= FILE_MAX_PARTS) fail("file_source_mismatch");
        const name = partName(input.uploadId, bound, index);
        const sha256 = hash(bytes);
        const operation = this.#client.prepareCommit({
          requestId: requestId(input.uploadId, `part/${index}`), scope: input.writer,
          mutations: [{ kind: "Blob", writer: input.writer, name, expected: null, bytes: Array.from(bytes) }],
        });
        abort(signal);
        attempted = true;
        const ack = await operation.send({ signal });
        generation = Math.max(generation, ack.generation);
        parts.push({ index, name, bytes: used, sha256, revision: ack.blob_revisions["0"] });
        used = 0;
      };
      // Every input chunk is bounded; copy into an owned part before any await.
      for await (const incoming of source) {
        abort(signal);
        if (!(incoming instanceof Uint8Array) || incoming.byteLength > FILE_PART_BYTES || total + incoming.byteLength > input.size) fail("file_source_mismatch");
        const owned = new Uint8Array(incoming.byteLength);
        owned.set(incoming);
        full.update(owned);
        total += owned.length;
        let at = 0;
        while (at < owned.length) {
          const take = Math.min(owned.length - at, FILE_PART_BYTES - used);
          buffer.set(owned.subarray(at, at + take), used);
          at += take; used += take;
          if (used === FILE_PART_BYTES) await sendPart();
        }
      }
      if (total !== input.size || full.digest("hex") !== input.sha256) fail("file_source_mismatch");
      if (used) await sendPart();
      // Complete length and digest are validated before the only visible-file pointer write.
      const manifest = {
        format: FORMAT, writer: input.writer, name: input.name, upload_id: input.uploadId,
        request_id: requestId(input.uploadId, "manifest"), expected: input.expected,
        total_bytes: total, sha256: input.sha256, part_bytes: FILE_PART_BYTES, parts,
      };
      const bytes = encoder.encode(JSON.stringify(manifest));
      if (bytes.length > MANIFEST_MAX_BYTES) fail("file_source_mismatch");
      const name = fileManifestName(input.name);
      const operation = this.#client.prepareCommit({ requestId: manifest.request_id, scope: input.writer,
        mutations: [{ kind: "Blob", writer: input.writer, name, expected: input.expected, bytes: Array.from(bytes) }] });
      abort(signal);
      attempted = true;
      const ack = await operation.send({ signal });
      return Object.freeze({ writer: input.writer, name: input.name, uploadId: input.uploadId,
        size: total, sha256: input.sha256, generation: Math.max(generation, ack.generation),
        revision: ack.blob_revisions["0"], manifestName: name });
    } catch (error) {
      // An unacknowledged attempt holds the original identity; never advance or invent an ID.
      throw safe(error, attempted ? "unknown" : "none");
    } finally {
      this.#active = false;
    }
  }
}
export function prepareFileUpload(client, input) { return new PreparedFileUpload(client, input); }

function readOptions(input) {
  if (!input || typeof input !== "object" || Array.isArray(input) ||
      Object.keys(input).some((k) => !["writer", "name", "minGeneration", "signal", "expectedRevision"].includes(k))) fail();
  const value = { writer: input.writer, name: input.name, minGeneration: input.minGeneration ?? 0, signal: input.signal, expectedRevision: input.expectedRevision };
  opaque(value.writer); label(value.name);
  size(value.minGeneration, Number.MAX_SAFE_INTEGER);
  if (value.expectedRevision !== undefined) opaque(value.expectedRevision);
  return value;
}

export async function downloadFile(client, supplied) {
  try {
    if (!(client instanceof OojuroClient)) fail();
    const input = readOptions(supplied);
    return await downloadVia((name, options) => client.blob(input.writer, name, options), input);
  } catch (error) { throw safe(error); }
}
export async function downloadFileTo(client, supplied, sink) {
  let input;
  try {
    if (!(client instanceof OojuroClient)) fail();
    input = readOptions(supplied);
  } catch (error) {
    try { await sink?.abort(); } catch { /* Keep destination errors content-free. */ }
    throw safe(error);
  }
  return downloadToVia((name, options) => client.blob(input.writer, name, options), input, sink);
}
