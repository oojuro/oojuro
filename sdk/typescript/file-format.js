// Internal shared bounded user-file format validation; no I/O or public package subpath.
import { createHash } from "node:crypto";
import { OojuroError } from "./index.js";

export const FILE_PART_BYTES = 256 * 1024;
export const FILE_MAX_PARTS = 1024;
export const FILE_MAX_BYTES = FILE_PART_BYTES * FILE_MAX_PARTS;
export const MANIFEST_MAX_BYTES = 512 * 1024;
export const FORMAT = "oojuro-user-file-1";
export const PREFIX = "oojuro-file/v1/";
const hex = /^[0-9a-f]{64}$/;
export const encoder = new TextEncoder();
export const fail = (code = "file_invalid_request", outcome = "none") => {
  throw new OojuroError(code, { outcome });
};
export const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
export const binding = (input) => hash(encoder.encode(JSON.stringify([input.writer, input.name, input.uploadId, input.expected, input.size, input.sha256, "intentional-user-file"])));
export const partName = (upload, bound, index) => `${PREFIX}parts/${upload}/${bound}/${index}`;
export const requestId = (upload, step) => `${PREFIX}requests/${upload}/${step}`;
export function fileManifestName(name) {
  label(name);
  return `${PREFIX}manifests/${hash(encoder.encode(name))}`;
}
export function opaque(value) {
  if (typeof value !== "string" || !hex.test(value)) fail();
}
export function label(value) {
  if (typeof value !== "string" || !value.length || !value.isWellFormed() ||
      encoder.encode(value).length > 1024 || /[\u0000-\u001f\u007f-\u009f]/u.test(value)) fail();
}
export function fields(value, names) {
  if (!value || typeof value !== "object" || Array.isArray(value) ||
      Object.keys(value).length !== names.length || names.some((k) => !Object.hasOwn(value, k))) fail();
}
export function size(value, maximum = FILE_MAX_BYTES) {
  if (!Number.isSafeInteger(value) || value < 0 || value > maximum) fail();
}
export function manifestOf(bytes, input) {
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
