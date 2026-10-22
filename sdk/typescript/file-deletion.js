// Explicit deletion of caller-selected file revisions; never retention or orphan GC.
import { OojuroClient, OojuroError } from "./index.js";
import { encoder, fields, opaque, label, size, hash, manifestOf, fileManifestName, PREFIX, MANIFEST_MAX_BYTES } from "./file-format.js";
const FORMAT = "oojuro-user-file-delete-1";
const inspect = Symbol.for("nodejs.util.inspect.custom");
const fail = (code = "file_delete_invalid_request") => { throw new OojuroError(code); };
function safe(error) {
  try {
    const own = error instanceof OojuroError;
    const code = own ? error.code : undefined;
    const status = own ? error.status : undefined;
    return { code: ["file_delete_invalid_request", "file_delete_manifest_unconfirmed", "file_delete_aborted"].includes(code) ? code : "file_delete_transport",
      status: Number.isInteger(status) && status >= 100 && status <= 599 ? status : undefined };
  } catch { return { code: "file_delete_transport" }; }
}
function signalOf(input = {}) {
  if (!input || typeof input !== "object" || Array.isArray(input) || Object.keys(input).some((k) => k !== "signal")) fail();
  const signal = input.signal;
  if (signal?.aborted) fail("file_delete_aborted");
  return signal;
}
function selected(input) {
  try {
    const names = ["format", "deleteId", "writer", "name", "manifestRevision", "manifestSha256", "manifest", "consent"];
    fields(input, names);
    const value = Object.fromEntries(names.map((key) => [key, input[key]]));
    if (value.format !== FORMAT || value.consent !== "delete-selected-user-file") fail();
    opaque(value.deleteId); opaque(value.writer); opaque(value.manifestRevision); opaque(value.manifestSha256); label(value.name);
    if (typeof value.manifest !== "string" || value.manifest.length > MANIFEST_MAX_BYTES || !value.manifest.isWellFormed()) fail();
    const bytes = encoder.encode(value.manifest);
    if (hash(bytes) !== value.manifestSha256) fail();
    const manifest = manifestOf(bytes, value);
    return { descriptor: Object.freeze(value), manifest };
  } catch { fail(); }
}
export async function planFileDeletion(client, supplied) {
  try {
    if (!(client instanceof OojuroClient) || !supplied || typeof supplied !== "object" || Array.isArray(supplied) ||
        Object.keys(supplied).some((k) => !["deleteId", "writer", "name", "manifestRevision", "minGeneration", "signal", "consent"].includes(k))) fail();
    const input = { deleteId: supplied.deleteId, writer: supplied.writer, name: supplied.name,
      manifestRevision: supplied.manifestRevision, minGeneration: supplied.minGeneration ?? 0,
      signal: supplied.signal, consent: supplied.consent };
    opaque(input.deleteId); opaque(input.writer); opaque(input.manifestRevision); label(input.name); size(input.minGeneration, Number.MAX_SAFE_INTEGER);
    if (input.consent !== "delete-selected-user-file") fail();
    if (input.signal?.aborted) fail("file_delete_aborted");
    const read = await client.blob(input.writer, fileManifestName(input.name), input);
    manifestOf(read.bytes, input);
    const value = { format: FORMAT, deleteId: input.deleteId, writer: input.writer, name: input.name,
      manifestRevision: input.manifestRevision, manifestSha256: hash(read.bytes),
      manifest: new TextDecoder("utf-8", { fatal: true }).decode(read.bytes), consent: input.consent };
    return selected(value).descriptor;
  } catch (error) {
    const { code, status } = safe(error);
    throw new OojuroError(code, { status });
  }
}
export class OojuroFileDeletionError extends OojuroError {
  constructor(code, { status, outcome = "none", progress }) {
    super(code, { status, outcome });
    this.name = "OojuroFileDeletionError";
    this.progress = progress;
  }
}
export class PreparedFileDeletion {
  #client;
  #descriptor;
  #manifest;
  #manifestOperation;
  #manifestAck = null;
  #partAcks = new Map();
  #cleanupComplete = false;
  #active = false;
  constructor(client, input) {
    try {
      if (!(client instanceof OojuroClient)) fail();
      const chosen = selected(input);
      this.#client = client; this.#descriptor = chosen.descriptor; this.#manifest = chosen.manifest;
      this.#manifestOperation = client.prepareCommit({ requestId: `${PREFIX}deletes/${chosen.descriptor.deleteId}/${chosen.descriptor.manifestSha256}/manifest`, scope: chosen.descriptor.writer,
        mutations: [{ kind: "DeleteBlob", writer: chosen.descriptor.writer, name: fileManifestName(chosen.descriptor.name), expected: chosen.descriptor.manifestRevision }] });
    } catch { fail(); }
  }
  toJSON() { return { type: "PreparedFileDeletion" }; }
  [inspect]() { return "PreparedFileDeletion {}"; }
  get progress() {
    const remaining = this.#manifest.parts.map((_, index) => index).filter((index) => !this.#partAcks.has(index));
    const generations = [this.#manifestAck?.generation ?? 0, ...[...this.#partAcks.values()].map((ack) => ack.generation)];
    return Object.freeze({ logicalFileDeleted: this.#manifestAck !== null, cleanupComplete: this.#cleanupComplete,
      partCount: this.#manifest.parts.length, acknowledgedParts: this.#partAcks.size,
      remainingParts: remaining.length, remainingPartIndices: Object.freeze(remaining), generation: Math.max(...generations) });
  }
  #failure(error, attempted) {
    const { code, status } = safe(error);
    return new OojuroFileDeletionError(code, { status, outcome: attempted ? "unknown" : "none", progress: this.progress });
  }
  async removeManifest(options) {
    if (this.#active) throw this.#failure(new OojuroError("file_delete_invalid_request"), false);
    this.#active = true;
    let attempted = false;
    try {
      const signal = signalOf(options);
      attempted = true;
      this.#manifestAck = await this.#manifestOperation.send({ signal });
      return this.progress;
    } catch (error) { throw this.#failure(error, attempted); }
    finally { this.#active = false; }
  }
  async cleanupParts(options) {
    if (this.#active) throw this.#failure(new OojuroError("file_delete_invalid_request"), false);
    this.#active = true;
    let attempted = false;
    try {
      const signal = signalOf(options);
      if (!this.#manifestAck) fail("file_delete_manifest_unconfirmed");
      // An explicit new cleanup pass replays the same exact requests from zero.
      for (const part of this.#manifest.parts) {
        if (signal?.aborted) fail("file_delete_aborted");
        const operation = this.#client.prepareCommit({ requestId: `${PREFIX}deletes/${this.#descriptor.deleteId}/${this.#descriptor.manifestSha256}/parts/${part.index}`, scope: this.#descriptor.writer,
          mutations: [{ kind: "DeleteBlob", writer: this.#descriptor.writer, name: part.name, expected: part.revision }] });
        attempted = true;
        const ack = await operation.send({ signal });
        this.#partAcks.set(part.index, ack);
      }
      this.#cleanupComplete = true;
      return this.progress;
    } catch (error) { throw this.#failure(error, attempted); }
    finally { this.#active = false; }
  }
}
export function prepareFileDeletion(client, descriptor) { return new PreparedFileDeletion(client, descriptor); }
