// Explicit selected bytes -> immutable file version -> small journal event -> project read.
// No file discovery, local file reads, extraction, OCR, preview or capture-byte grant.
import { OojuroClient, OojuroCaptureClient, OojuroProjectClient, OojuroError, captureSession } from "./index.js";
import { prepareFileUpload } from "./files.js";
import { downloadToVia } from "./file-download.js";
import { fields, opaque, label, size, fail, encoder, binding, partName, manifestOf, fileManifestName } from "./file-format.js";
export const ATTACHMENT_MAX_BYTES = 16 * 1024 * 1024;
const inspect = Symbol.for("nodejs.util.inspect.custom");
const wireName = (journal, upload) => `oojuro-attachment/v1/${journal}/${upload}`;
function displayName(name) {
  label(name);
  if (!name.trim() || encoder.encode(name).length > 256) fail();
}
function descriptor(input) {
  const names = ["uploadId", "eventId", "journal", "runId", "writer", "agentId", "displayName", "size", "sha256", "validAt", "consent",
    ...(input && Object.hasOwn(input, "session") ? ["session"] : [])];
  fields(input, names);
  const value = Object.fromEntries(names.map(key => [key, input[key]]));
  for (const name of ["uploadId", "eventId", "journal", "runId", "writer", "agentId", "sha256"]) opaque(value[name]);
  displayName(value.displayName); size(value.size, ATTACHMENT_MAX_BYTES);
  if (!Number.isSafeInteger(value.validAt) || value.consent !== "intentional-user-file") fail();
  if (Object.hasOwn(value, "session")) value.session = captureSession(value.session);
  return Object.freeze(value);
}
function reference(input, journal) {
  const names = ["format", "journal", "writer", "upload_id", "display_name", "manifest_name", "manifest_revision", "total_bytes", "sha256"];
  fields(input, names);
  const value = Object.fromEntries(names.map(key => [key, input[key]]));
  for (const name of ["journal", "writer", "upload_id", "manifest_revision", "sha256"]) opaque(value[name]);
  displayName(value.display_name); size(value.total_bytes, ATTACHMENT_MAX_BYTES);
  if (value.format !== "oojuro-attachment-1" || value.journal !== journal ||
      value.manifest_name !== fileManifestName(wireName(value.journal, value.upload_id))) fail();
  return Object.freeze(value);
}
function safe(error, attempted = false) {
  let status;
  try { if (error instanceof OojuroError && Number.isInteger(error.status) && error.status >= 100 && error.status <= 599) status = error.status; }
  catch { /* Never expose arbitrary transport properties. */ }
  return new OojuroError("attachment_failed", { status,
    outcome: attempted || (error instanceof OojuroError && error.outcome === "unknown") ? "unknown" : "none" });
}

export class PreparedJournalAttachment {
  #uploader;
  #capture;
  #input;
  #upload;
  #active = false;
  constructor(uploader, capture, input) {
    try {
      if (!(uploader instanceof OojuroClient) || !(capture instanceof OojuroCaptureClient) || uploader.origin !== capture.origin) fail();
      this.#input = descriptor(input); this.#uploader = uploader; this.#capture = capture;
      const value = this.#input;
      this.#upload = prepareFileUpload(uploader, { uploadId: value.uploadId, writer: value.writer,
        name: wireName(value.journal, value.uploadId), expected: null, size: value.size,
        sha256: value.sha256, consent: value.consent });
    } catch (error) { throw safe(error); }
  }
  toJSON() { return { type: "PreparedJournalAttachment" }; }
  [inspect]() { return "PreparedJournalAttachment {}"; }
  async send(source, options = {}) {
    if (this.#active) throw safe();
    this.#active = true;
    let published = false;
    try {
      if (!options || typeof options !== "object" || Object.keys(options).some(k => k !== "signal")) fail();
      const { signal } = options, input = this.#input;
      // Validate destination/producer identity before any selected file bytes are sent.
      const root = (await this.#uploader.journalRoot({ signal })).value;
      const capture = await this.#capture.discover({ signal });
      if (root.schema !== 3 || root.namespace !== capture.journal ||
          root.ownership?.agent !== input.agentId || capture.agent_id !== input.agentId ||
          capture.producer_id !== input.writer) fail();
      if (capture.journal_mode === "session") {
        const route = input.session;
        if (!route || route.source !== root.project || route.source !== this.#capture.source || route.agent_id !== input.agentId ||
            route.run_id !== input.runId || route.journal !== input.journal) fail();
      } else if (input.session) fail();
      const uploaded = await this.#upload.send(source, { signal });
      published = true;
      const file = Object.freeze({ format: "oojuro-attachment-1", journal: input.journal,
        writer: input.writer, upload_id: input.uploadId, display_name: input.displayName,
        manifest_name: uploaded.manifestName, manifest_revision: uploaded.revision,
        total_bytes: uploaded.size, sha256: uploaded.sha256 });
      const result = await this.#capture.prepareBatch({ requestId: `attachment_${input.uploadId}`,
        ...(input.session ? { session: input.session } : {}),
        events: [{ eventId: input.eventId, runId: input.runId, adapter: "oojuro-attachment-v1",
          input: { agent_id: input.agentId, parent_id: null, valid_at: input.validAt,
            kind: "output", tool: null, status: null, exit: null, key: null,
            text: `Attachment: ${input.displayName}`, indexed: true, attrs: { oojuro_attachment: file } } }] }).send({ signal });
      return Object.freeze({ journal: input.journal, eventId: input.eventId,
        recordId: result.records[0].record_id, file, generation: Math.max(uploaded.generation, result.generation) });
    } catch (error) { throw safe(error, published); }
    finally { this.#active = false; }
  }
}
export function prepareJournalAttachment(uploader, capture, descriptor) {
  return new PreparedJournalAttachment(uploader, capture, descriptor);
}

export async function downloadJournalAttachmentTo(client, supplied, sink, options = {}) {
  let input, file, configuration;
  try {
    if (!(client instanceof OojuroProjectClient)) fail();
    if (!supplied || typeof supplied !== "object" || Array.isArray(supplied) ||
        ["journal", "recordId", "file"].some(k => !Object.hasOwn(supplied, k)) ||
        Object.keys(supplied).some(k => !["journal", "recordId", "file", "eventId", "generation"].includes(k))) fail();
    if (Object.hasOwn(supplied, "eventId")) opaque(supplied.eventId);
    if (Object.hasOwn(supplied, "generation")) size(supplied.generation, Number.MAX_SAFE_INTEGER);
    input = { journal: supplied.journal, recordId: supplied.recordId };
    opaque(input.journal); opaque(input.recordId);
    file = reference(supplied.file, input.journal);
    if (!options || typeof options !== "object" || Object.keys(options).some(k => !["signal", "minGeneration"].includes(k))) fail();
    const minGeneration = options.minGeneration ?? 0;
    size(minGeneration, Number.MAX_SAFE_INTEGER);
    configuration = { writer: file.writer, name: wireName(file.journal, file.upload_id),
      minGeneration, expectedRevision: file.manifest_revision, signal: options.signal };
  } catch (error) {
    try { await sink?.abort(); } catch { /* No destination contents in errors. */ }
    throw safe(error);
  }
  const bound = binding({ writer: configuration.writer, name: configuration.name,
    uploadId: file.upload_id, expected: null, size: file.total_bytes, sha256: file.sha256 });
  const selected = new Map(Array.from({ length: Math.ceil(file.total_bytes / (256 * 1024)) }, (_, index) =>
    [partName(file.upload_id, bound, index), index]));
  const read = async (name, settings) => {
    const part = name === file.manifest_name ? undefined : selected.get(name);
    if (part === undefined && name !== file.manifest_name) fail("file_integrity");
    const result = await client.attachmentPart({ ...input, ...(part === undefined ? {} : { part }) }, settings);
    if (part === undefined) {
      const manifest = manifestOf(result.bytes, configuration);
      if (manifest.upload_id !== file.upload_id || manifest.expected !== null ||
          manifest.total_bytes !== file.total_bytes || manifest.sha256 !== file.sha256) fail("file_integrity");
    }
    return result;
  };
  return downloadToVia(read, configuration, sink);
}
