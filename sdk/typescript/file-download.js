// Internal bounded download path shared by native files and project-linked attachments.
// No filesystem access, parser, cache or credential ownership.
import { createHash } from "node:crypto";
import { OojuroError } from "./index.js";
import { fail, hash, fileManifestName, manifestOf } from "./file-format.js";
function abort(signal) { if (signal?.aborted) fail("file_aborted"); }
function safe(error, outcome = "none") {
  try {
    const allowed = ["file_invalid_request", "file_source_mismatch", "file_integrity", "file_aborted", "file_incomplete", "file_sink"];
    const own = error instanceof OojuroError;
    const code = own ? error.code : null;
    const status = own ? error.status : undefined;
    return new OojuroError(allowed.includes(code) ? code : "file_transport", {
      outcome, status: Number.isInteger(status) && status >= 100 && status <= 599 ? status : undefined,
    });
  } catch { return new OojuroError("file_transport", { outcome }); }
}

export async function downloadVia(readBlob, input) {
  try {
    abort(input.signal);
    const manifestName = fileManifestName(input.name);
    const initial = await readBlob(manifestName, input);
    if (input.expectedRevision !== undefined && initial.revision !== input.expectedRevision) fail("file_integrity");
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
              const read = await readBlob(part.name, { minGeneration: generation, signal: readSignal });
              generation = Math.max(generation, read.generation);
              if (read.bytes.length !== part.bytes || hash(read.bytes) !== part.sha256 ||
                  (input.expectedRevision !== undefined && read.revision !== part.revision)) fail("file_integrity");
              full.update(read.bytes); total += read.bytes.length;
              yield read.bytes;
            }
            if (total !== manifest.total_bytes || full.digest("hex") !== manifest.sha256) fail("file_integrity");
            abort(readSignal);
            // Recheck the logical pointer and current authorization before declaring completion.
            const final = await readBlob(manifestName, { minGeneration: generation, signal: readSignal });
            if (final.bytes.length !== initial.bytes.length || hash(final.bytes) !== hash(initial.bytes) ||
                (input.expectedRevision !== undefined && final.revision !== input.expectedRevision)) fail("file_integrity");
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

export async function downloadToVia(read, configuration, sink) {
  let stream;
  try {
    if (!sink || ["write", "commit", "abort"].some((method) => typeof sink[method] !== "function")) fail();
    stream = await downloadVia(read, configuration);
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
