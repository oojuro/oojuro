// Explicit Node24 user-file protocol over the existing encrypted v2 Blob API.
import { createHash } from "node:crypto";
import { OojuroClient, OojuroError } from "./index.js";

export const FILE_PART_BYTES = 256 * 1024;
export const FILE_MAX_PARTS = 1024;
export const FILE_MAX_BYTES = FILE_PART_BYTES * FILE_MAX_PARTS;
const MANIFEST_MAX_BYTES = 512 * 1024;
const FORMAT = "oojuro-user-file-1";
const PREFIX = "oojuro-file/v1/";
const hex = /^[0-9a-f]{64}$/;
const encoder = new TextEncoder();
const inspect = Symbol.for("nodejs.util.inspect.custom");
const fail = (code = "file_invalid_request", outcome = "none") => {
  throw new OojuroError(code, { outcome });
};
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
const binding = (input) => hash(encoder.encode(JSON.stringify([input.writer, input.name, input.uploadId, input.expected, input.size, input.sha256, "intentional-user-file"])));
const partName = (upload, bound, index) => `${PREFIX}parts/${upload}/${bound}/${index}`;
const requestId = (upload, step) => `${PREFIX}requests/${upload}/${step}`;
export function fileManifestName(name) {
  label(name);
  return `${PREFIX}manifests/${hash(encoder.encode(name))}`;
}
function opaque(value) {
  if (typeof value !== "string" || !hex.test(value)) fail();
}
function label(value) {
  if (typeof value !== "string" || !value.length || !value.isWellFormed() ||
      encoder.encode(value).length > 1024 || /[\u0000-\u001f\u007f-\u009f]/u.test(value)) fail();
}
function fields(value, names) {
  if (!value || typeof value !== "object" || Array.isArray(value) ||
      Object.keys(value).length !== names.length || names.some((k) => !Object.hasOwn(value, k))) fail();
}
function size(value, maximum = FILE_MAX_BYTES) {
  if (!Number.isSafeInteger(value) || value < 0 || value > maximum) fail();
}
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
      Object.keys(input).some((k) => !["writer", "name", "minGeneration", "signal"].includes(k))) fail();
  const value = { writer: input.writer, name: input.name, minGeneration: input.minGeneration ?? 0, signal: input.signal };
  opaque(value.writer); label(value.name);
  size(value.minGeneration, Number.MAX_SAFE_INTEGER);
  return value;
}
function manifestOf(bytes, input) {
  try {
    if (bytes.length > MANIFEST_MAX_BYTES) fail();
    const value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes), (_key, item, context) => {
      if (typeof item === "number" && (!Number.isSafeInteger(item) || !context || !/^(0|[1-9][0-9]*)$/.test(context.source))) fail();
      return item;
    });
    fields(value, ["format", "writer", "name", "upload_id", "request_id", "expected", "total_bytes", "sha256", "part_bytes", "parts"]);
    if (value.format !== FORMAT || value.writer !== input.writer || value.name !== input.name || value.part_bytes !== FILE_PART_BYTES) fail();
    opaque(value.writer); opaque(value.upload_id); opaque(value.sha256);
    if (value.expected !== null) opaque(value.expected);
    if (value.request_id !== requestId(value.upload_id, "manifest")) fail();
    size(value.total_bytes);
    if (!Array.isArray(value.parts) || value.parts.length !== Math.ceil(value.total_bytes / FILE_PART_BYTES) || value.parts.length > FILE_MAX_PARTS) fail();
    const bound = binding({ writer: value.writer, name: value.name, uploadId: value.upload_id, expected: value.expected, size: value.total_bytes, sha256: value.sha256 });
    let total = 0;
    value.parts.forEach((part, index) => {
      fields(part, ["index", "name", "bytes", "sha256", "revision"]);
      opaque(part.sha256); opaque(part.revision);
      const expectedBytes = Math.min(FILE_PART_BYTES, value.total_bytes - index * FILE_PART_BYTES);
      if (part.index !== index || part.name !== partName(value.upload_id, bound, index) || part.bytes !== expectedBytes || part.bytes <= 0) fail();
      total += part.bytes;
    });
    if (total !== value.total_bytes || hash(encoder.encode(JSON.stringify(value))) !== hash(bytes)) fail();
    return value;
  } catch {
    fail("file_integrity");
  }
}

export async function downloadFile(client, supplied) {
  try {
    if (!(client instanceof OojuroClient)) fail();
    const input = readOptions(supplied);
    abort(input.signal);
    const manifestName = fileManifestName(input.name);
    const initial = await client.blob(input.writer, manifestName, input);
    const manifest = manifestOf(initial.bytes, input);
    let resolve, reject;
    const completion = new Promise((yes, no) => { resolve = yes; reject = no; });
    // Advanced callers may handle iterator failure before awaiting completion.
    void completion.catch(() => {});
    let started = false;
    const controller = new AbortController();
    const readSignal = input.signal ? AbortSignal.any([input.signal, controller.signal]) : controller.signal;
    const chunks = {
      [Symbol.asyncIterator]() {
        if (started) fail("file_incomplete");
        started = true;
        const iterator = (async function* () {
          let complete = false;
          try {
            const full = createHash("sha256");
            let total = 0, generation = Math.max(input.minGeneration, initial.generation);
            for (const part of manifest.parts) {
              abort(readSignal);
              const read = await client.blob(input.writer, part.name, { minGeneration: generation, signal: readSignal });
              generation = Math.max(generation, read.generation);
              if (read.bytes.length !== part.bytes || hash(read.bytes) !== part.sha256) fail("file_integrity");
              full.update(read.bytes); total += read.bytes.length;
              yield read.bytes;
            }
            if (total !== manifest.total_bytes || full.digest("hex") !== manifest.sha256) fail("file_integrity");
            abort(readSignal);
            // Recheck the logical pointer and current authorization before declaring completion.
            const final = await client.blob(input.writer, manifestName, { minGeneration: generation, signal: readSignal });
            if (final.bytes.length !== initial.bytes.length || hash(final.bytes) !== hash(initial.bytes)) fail("file_integrity");
            const result = Object.freeze({ writer: input.writer, name: input.name, uploadId: manifest.upload_id,
              size: total, sha256: manifest.sha256, generation: Math.max(generation, final.generation), manifestName });
            complete = true;
            resolve(result);
          } catch (error) {
            const failure = safe(error);
            reject(failure);
            throw failure;
          } finally {
            if (!complete) reject(new OojuroError("file_incomplete"));
          }
        })();
        return {
          next: (...args) => iterator.next(...args),
          return(value) {
            // Async-generator finally does not run if return precedes the first next.
            reject(new OojuroError("file_incomplete"));
            controller.abort();
            return iterator.return(value);
          },
          async throw(error) {
            const failure = safe(error);
            reject(failure);
            controller.abort();
            try { await iterator.return(); } catch { /* Keep caller content out of failures. */ }
            throw failure;
          },
          [Symbol.asyncIterator]() { return this; },
        };
      },
    };
    return Object.freeze({ chunks, completion });
  } catch (error) {
    throw safe(error);
  }
}

export async function downloadFileTo(client, input, sink) {
  let stream;
  try {
    if (!sink || ["write", "commit", "abort"].some((method) => typeof sink[method] !== "function")) fail();
    const configuration = readOptions(input);
    stream = await downloadFile(client, configuration);
    for await (const part of stream.chunks) {
      try { await sink.write(part); } catch { fail("file_sink"); }
    }
    const result = await stream.completion;
    abort(configuration.signal);
    try { await sink.commit(result); } catch { fail("file_sink"); }
    return result;
  } catch (error) {
    if (stream) void stream.completion.catch(() => {});
    try { await sink?.abort(); } catch { /* Do not expose sink content or replace the original failure. */ }
    throw safe(error);
  }
}
