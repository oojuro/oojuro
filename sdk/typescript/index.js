// Experimental versioned native, hosted capture and shared-project read adapters.
const encoder = new TextEncoder();
const opaquePattern = /^[0-9a-f]{64}$/;
const sources = new WeakMap();
const inspect = Symbol.for("nodejs.util.inspect.custom");
const MiB = 1024 * 1024;
const recordMedia = "application/vnd.oojuro.record.v1";
const recordMetadataLimit = 64 * 1024;
const recordTextLimit = 16 * MiB;
const recordWireLimit = 4 + recordMetadataLimit + recordTextLimit;
const typedArrayPrototype = Object.getPrototypeOf(Uint8Array.prototype);
const typedArrayBuffer = Object.getOwnPropertyDescriptor(typedArrayPrototype, "buffer").get;
const typedArrayLength = Object.getOwnPropertyDescriptor(typedArrayPrototype, "length").get;
const typedArrayOffset = Object.getOwnPropertyDescriptor(typedArrayPrototype, "byteOffset").get;
const arrayBufferLength = Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, "byteLength").get;

export class OojuroError extends Error {
  constructor(code, { status, outcome = "none" } = {}) {
    super(`Oojuro request failed: ${code}`);
    this.name = "OojuroError";
    this.code = code;
    this.status = status;
    this.outcome = outcome;
  }
}
const fail = (code = "invalid_response") => {
  throw new OojuroError(code);
};
const object = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
function keys(value, names) {
  if (
    !object(value) ||
    Object.keys(value).length !== names.length ||
    names.some((name) => !Object.hasOwn(value, name))
  )
    fail();
}
function string(value, max = 2 * MiB, nonempty = false) {
  if (
    typeof value !== "string" ||
    !value.isWellFormed() ||
    encoder.encode(value).length > max ||
    (nonempty && !value.length)
  )
    fail();
}
function id(value) {
  if (typeof value !== "string" || !opaquePattern.test(value)) fail();
}
function integer(value, key, min = 0, max = Number.MAX_SAFE_INTEGER) {
  const number = value[key];
  if (!Number.isSafeInteger(number) || number < min || number > max || sources.get(value)?.has(key))
    fail();
}
function parse(text) {
  // Source lexemes prevent an integer field such as 1.0000000000000001 silently becoming 1.
  return JSON.parse(text, function (key, value, context) {
    if (typeof value === "number") {
      if (!Number.isFinite(value) || Math.abs(value) > Number.MAX_SAFE_INTEGER) fail();
      if (!context || !/^-?(0|[1-9][0-9]*)$/.test(context.source)) {
        const set = sources.get(this) ?? new Set();
        set.add(key);
        sources.set(this, set);
      }
    }
    if (typeof value === "string" && !value.isWellFormed()) fail();
    return value;
  });
}
function encode(value) {
  return JSON.stringify(value, (_key, item) => {
    if (
      item === undefined ||
      ["bigint", "function", "symbol"].includes(typeof item) ||
      (typeof item === "number" &&
        (!Number.isFinite(item) || Math.abs(item) > Number.MAX_SAFE_INTEGER))
    )
      fail();
    return item;
  });
}
const traceFields = [
  "format",
  "text",
  "kind",
  "agent_id",
  "parent_id",
  "source",
  "session",
  "parent_session",
  "turn",
  "sequence",
  "parent",
  "revises",
  "valid_us",
  "recorded_us",
  "duration_us",
  "tool",
  "arguments",
  "status",
  "exit_code",
  "tokens_in",
  "tokens_out",
  "indexed",
  "metadata",
  "raw",
];
function trace(value) {
  keys(value, traceFields);
  if (
    value.format !== 2 ||
    ![
      "system",
      "user_input",
      "assistant",
      "thinking",
      "tool_call",
      "tool_result",
      "explicit",
    ].includes(value.kind)
  )
    fail();
  for (const field of [
    "text",
    "agent_id",
    "parent_id",
    "source",
    "session",
    "parent_session",
    "tool",
    "status",
  ])
    string(value[field], field === "text" ? MiB : 2 * MiB);
  for (const field of ["format", "turn", "sequence"]) integer(value, field, 0, 0xffffffff);
  for (const field of ["parent", "revises", "duration_us", "tokens_in", "tokens_out"])
    integer(value, field);
  for (const field of ["valid_us", "recorded_us"]) integer(value, field, -Number.MAX_SAFE_INTEGER);
  if (value.exit_code !== null) integer(value, "exit_code", -0x80000000, 0x7fffffff);
  if (typeof value.indexed !== "boolean" || !object(value.metadata)) fail();
  for (const [key, item] of Object.entries(value.metadata)) {
    string(key);
    string(item);
  }
  if (value.raw !== null) {
    keys(value.raw, ["source_generation", "start", "end", "emission"]);
    string(value.raw.source_generation);
    integer(value.raw, "start");
    integer(value.raw, "end");
    integer(value.raw, "emission", 0, 0xffffffff);
    if (value.raw.start > value.raw.end) fail();
  }
  if (encoder.encode(JSON.stringify(value)).length > 2 * MiB) fail();
}
function record(value) {
  keys(value, ["id", "writer", "writer_name", "memory_name", "trace"]);
  id(value.id);
  id(value.writer);
  string(value.writer_name, 256);
  if (value.memory_name !== null) string(value.memory_name, 1024, true);
  trace(value.trace);
}
function mark(value, generation) {
  keys(value, ["generation", "epoch", "transaction"]);
  integer(value, "generation");
  integer(value, "epoch");
  keys(value.transaction, ["id", "digest"]);
  id(value.transaction.id);
  id(value.transaction.digest);
  if (value.generation !== generation) fail();
}
function grant(value) {
  keys(value, ["admin", "read", "write", "retry"]);
  if (typeof value.admin !== "boolean") fail();
  for (const field of ["read", "write", "retry"]) {
    if (!Array.isArray(value[field]) || value[field].length > 10000) fail();
    value[field].forEach(id);
  }
}
function mutation(value) {
  const shapes = {
    Record: ["kind", "writer", "trace"],
    Memory: ["kind", "writer", "name", "trace"],
    Blob: ["kind", "writer", "name", "expected", "bytes"],
    DeleteRecord: ["kind", "id"],
    DeleteMemory: ["kind", "writer", "name"],
    DeleteBlob: ["kind", "writer", "name", "expected"],
  };
  const shape = shapes[value?.kind];
  if (!shape) fail();
  keys(value, shape);
  if ("writer" in value) id(value.writer);
  if ("id" in value) id(value.id);
  if ("name" in value) string(value.name, 1024, true);
  if ("expected" in value && value.expected !== null) id(value.expected);
  if (value.kind === "DeleteBlob" && value.expected === null) fail();
  if ("trace" in value) trace(value.trace);
  if (
    value.kind === "Blob" &&
    (!Array.isArray(value.bytes) ||
      value.bytes.length > 2 * MiB ||
      value.bytes.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255))
  )
    fail();
}
function outcome(value, generation, mutations) {
  keys(value, ["generation", "record_ids", "blob_revisions", "writer", "key", "destruction", "usage"]);
  integer(value, "generation");
  if (
    value.generation !== generation ||
    value.writer !== null ||
    value.key !== null ||
    value.destruction !== null
  )
    fail();
  // Public commits contain only data writes/deletes. Current cloud control-only
  // null tags cannot appear here; these opaque receipts grant no caller authority.
  keys(value.usage, ["request_tag", "intent_fingerprint", "scope_tag",
    "committed_at_us", "recorded_bytes", "kept_bytes"]);
  for (const field of ["request_tag", "intent_fingerprint", "scope_tag"]) id(value.usage[field]);
  integer(value.usage, "committed_at_us", -Number.MAX_SAFE_INTEGER);
  integer(value.usage, "recorded_bytes");
  integer(value.usage, "kept_bytes");
  for (const [field, kinds] of [
    ["record_ids", ["Record", "Memory"]],
    ["blob_revisions", ["Blob"]],
  ]) {
    if (!object(value[field])) fail();
    const expected = mutations.flatMap((item, index) =>
      kinds.includes(item.kind) ? [String(index)] : [],
    );
    if (
      Object.keys(value[field]).length !== expected.length ||
      expected.some((index) => !Object.hasOwn(value[field], index))
    )
      fail();
    Object.values(value[field]).forEach(id);
  }
}

// Journal accepts bounded own-data JSON only. Inspect descriptors before reading values;
// count JSON escape/UTF8 bytes incrementally before allocating the encoded body.
function journalSnapshot(input, maximum) {
  let size = 0;
  let nodes = 0;
  const add = (count) => {
    size += count;
    if (size > maximum) fail();
  };
  function text(value) {
    add(2);
    for (let i = 0; i < value.length; i++) {
      const c = value.charCodeAt(i);
      if (c === 34 || c === 92 || [8, 9, 10, 12, 13].includes(c)) add(2);
      else if (c < 32) add(6);
      else if (c < 128) add(1);
      else if (c < 2048) add(2);
      else if (c >= 0xd800 && c <= 0xdbff) {
        const next = value.charCodeAt(++i);
        if (!(next >= 0xdc00 && next <= 0xdfff)) fail();
        add(4);
      } else if (c >= 0xdc00 && c <= 0xdfff) fail();
      else add(3);
    }
    return value;
  }
  function visit(value, depth) {
    if (++nodes > 8192 || depth > 40) fail();
    if (value === null) {
      add(4);
      return null;
    }
    if (typeof value === "string") return text(value);
    if (typeof value === "boolean") {
      add(value ? 4 : 5);
      return value;
    }
    if (typeof value === "number") {
      if (!Number.isFinite(value) || Math.abs(value) > Number.MAX_SAFE_INTEGER) fail();
      add(String(value).length);
      return value;
    }
    if (!object(value) && !Array.isArray(value)) fail();
    const array = Array.isArray(value);
    const prototype = Object.getPrototypeOf(value);
    if (
      array ? prototype !== Array.prototype : prototype !== Object.prototype && prototype !== null
    )
      fail();
    const names = Reflect.ownKeys(value);
    if (names.length > 1025 || names.some((key) => typeof key !== "string")) fail();
    const result = array ? [] : Object.create(null);
    add(2);
    const entries = array ? names.filter((key) => key !== "length") : names;
    if (array) {
      const length = Object.getOwnPropertyDescriptor(value, "length");
      if (
        !length ||
        !Object.hasOwn(length, "value") ||
        length.value > 1024 ||
        entries.length !== length.value ||
        entries.some((key, index) => key !== String(index))
      )
        fail();
    } else if (entries.length > 1024) fail();
    for (let i = 0; i < entries.length; i++) {
      const key = entries[i],
        descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor?.enumerable || !Object.hasOwn(descriptor, "value")) fail();
      if (i) add(1);
      if (!array) {
        text(key);
        add(1);
      }
      const child = visit(descriptor.value, depth + 1);
      Object.defineProperty(result, key, {
        value: child,
        enumerable: true,
        writable: true,
        configurable: true,
      });
    }
    return result;
  }
  return visit(input, 0);
}
const journalKinds = [
  "input",
  "output",
  "thinking",
  "tool_call",
  "tool_result",
  "system",
  "explicit",
  "raw",
];
const nativeFields = [
  "agent_id",
  "parent_id",
  "valid_at",
  "kind",
  "tool",
  "status",
  "exit",
  "key",
  "text",
  "attrs",
  "indexed",
];
function label(value, max) {
  string(value, max, true);
  if (/\p{Cc}/u.test(value)) fail();
}
function journalRoot(value, project) {
  const owned = value?.schema === 2 || value?.schema === 3;
  keys(value, ["schema", "minimum_reader", "project", "namespace", ...(owned ? ["ownership"] : [])]);
  integer(value, "schema", 1, 3);
  integer(value, "minimum_reader", value.schema, value.schema);
  id(value.project); id(value.namespace);
  if (value.project !== project) fail();
  if (owned) {
    keys(value.ownership, ["account", "agent"]);
    string(value.ownership.account, 128, true);
    if (!/^[A-Za-z0-9_-]+$/.test(value.ownership.account)) fail();
    id(value.ownership.agent);
  }
}
function nativeInput(value) {
  keys(value, nativeFields);
  label(value.agent_id, 256);
  for (const [field, max] of [
    ["parent_id", 256],
    ["tool", 1024],
    ["key", 1024],
  ])
    if (value[field] !== null) label(value[field], max);
  integer(value, "valid_at", -Number.MAX_SAFE_INTEGER);
  if (
    !journalKinds.includes(value.kind) ||
    ![null, "ok", "error"].includes(value.status) ||
    typeof value.indexed !== "boolean"
  )
    fail();
  if (value.exit !== null) integer(value, "exit", -0x80000000, 0x7fffffff);
  string(value.text, 16 * MiB);
  if (!object(value.attrs) || Object.keys(value.attrs).length > 1024) fail();
  const stack = Object.entries(value.attrs).map(([key, item]) => {
    string(key, 4096);
    return [item, 0];
  });
  let nodes = 0;
  while (stack.length) {
    const [item, depth] = stack.pop();
    if (++nodes > 4096 || depth > 32) fail();
    if (typeof item === "string") string(item, 4096);
    else if (Array.isArray(item) || object(item)) {
      if (Object.keys(item).length > 1024) fail();
      for (const [key, child] of Object.entries(item)) {
        string(key, 4096);
        stack.push([child, depth + 1]);
      }
    }
  }
  if (encoder.encode(encode(value.attrs)).length > 4096) fail();
}
function journalQuery(value) {
  keys(value, ["as_of", "generation", "writer", "mode"]);
  keys(value.as_of, ["from", "valid_at", "known_at"]);
  for (const field of ["from", "valid_at", "known_at"])
    integer(value.as_of, field, -Number.MAX_SAFE_INTEGER);
  if (value.generation !== null) integer(value, "generation", 1);
  if (value.writer !== null) id(value.writer);
  const mode = value.mode;
  if (mode?.mode === "Range") {
    keys(mode, ["mode", "versions", "filter"]);
    if (
      !["current", "all"].includes(mode.versions) ||
      !object(mode.filter) ||
      Object.keys(mode.filter).some((key) => !["kind", "tool", "status", "agent_id"].includes(key))
    )
      fail();
    const f = mode.filter;
    if (f.kind != null && !journalKinds.includes(f.kind)) fail();
    if (f.status != null && !["ok", "error"].includes(f.status)) fail();
    for (const [field, max] of [
      ["tool", 1024],
      ["agent_id", 256],
    ])
      if (f[field] != null) label(f[field], max);
  } else if (mode?.mode === "History") {
    keys(mode, ["mode", "key"]);
    label(mode.key, 1024);
    if (value.writer === null) fail();
  } else if (mode?.mode === "Id") {
    keys(mode, ["mode", "id"]);
    id(mode.id);
  } else fail();
}
function journalRecord(value) {
  keys(value, ["id", "record"]);
  id(value.id);
  const row = value.record;
  keys(row, ["schema", "id", "writer", "adapter", "recorded_at", ...nativeFields]);
  integer(row, "schema", 1, 1);
  integer(row, "id", 1);
  id(row.writer);
  label(row.adapter, 256);
  integer(row, "recorded_at", -Number.MAX_SAFE_INTEGER);
  const input = Object.fromEntries(nativeFields.map((key) => [key, row[key]]));
  // Retain the original numeric lexeme metadata for native integer fields.
  sources.set(input, sources.get(row));
  nativeInput(input);
}

function nativeRecordMetadata(row) {
  const fields = nativeFields.filter((key) => key !== "text");
  keys(row, ["schema", "id", "writer", "adapter", "recorded_at", ...fields]);
  integer(row, "schema", 1, 1);
  integer(row, "id", 1);
  id(row.writer);
  label(row.adapter, 256);
  integer(row, "recorded_at", -Number.MAX_SAFE_INTEGER);
  const input = Object.fromEntries(fields.map((key) => [key, row[key]]));
  input.text = "";
  sources.set(input, sources.get(row));
  nativeInput(input);
}
function journalDescriptor(value) {
  keys(value, ["id", "record", "text_bytes", "generation"]);
  id(value.id);
  nativeRecordMetadata(value.record);
  integer(value, "text_bytes", 0, recordTextLimit);
  integer(value, "generation", 1);
}
function ownFields(value, fields) {
  if (!object(value) || ![null, Object.prototype].includes(Object.getPrototypeOf(value))) fail();
  const names = Reflect.ownKeys(value);
  if (names.length !== fields.length || names.some((key) => !fields.includes(key))) fail();
  const result = Object.create(null);
  for (const key of fields) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (!descriptor?.enumerable || !Object.hasOwn(descriptor, "value")) fail();
    Object.defineProperty(result, key, {
      value: descriptor.value,
      enumerable: true,
      writable: true,
      configurable: true,
    });
  }
  return result;
}
function utf8Length(text, maximum) {
  if (typeof text !== "string" || text.length > maximum) fail();
  let length = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if (c < 128) length++;
    else if (c < 2048) length += 2;
    else if (c >= 0xd800 && c <= 0xdbff) {
      const next = text.charCodeAt(++i);
      if (!(next >= 0xdc00 && next <= 0xdfff)) fail();
      length += 4;
    } else if (c >= 0xdc00 && c <= 0xdfff) fail();
    else length += 3;
    if (length > maximum) fail();
  }
  return length;
}
function frameMetadata(text) {
  // JSON.parse alone discards duplicate object keys. Bound and reject them before
  // the existing source-lexeme parser, including escaped aliases such as "\u0069d".
  const stack = [];
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === "{" || c === "[") {
      if (stack.length >= 40) fail();
      stack.push(c === "{" ? new Set() : null);
    } else if (c === "}" || c === "]") stack.pop();
    else if (c === '"') {
      const start = i++;
      for (; i < text.length; i++) {
        if (text[i] === "\\") i++;
        else if (text[i] === '"') break;
      }
      if (i >= text.length) fail();
      let next = i + 1;
      while (/\s/u.test(text[next] ?? "") && next < text.length) next++;
      if (text[next] === ":") {
        const keys = stack.at(-1);
        const key = JSON.parse(text.slice(start, i + 1));
        if (!keys || keys.has(key)) fail();
        keys.add(key);
      }
    }
  }
  return parse(text);
}
function base64url(bytes) {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
function journalStats(value) {
  keys(value, ["records", "text_bytes", "kinds", "first_valid_at", "last_valid_at"]);
  integer(value, "records");
  integer(value, "text_bytes");
  if (!object(value.kinds) || Object.keys(value.kinds).some((key) => !journalKinds.includes(key)))
    fail();
  let total = 0;
  for (const key of Object.keys(value.kinds)) {
    integer(value.kinds, key, 1);
    total += value.kinds[key];
  }
  if (!Number.isSafeInteger(total) || total !== value.records) fail();
  for (const field of ["first_valid_at", "last_valid_at"])
    if (value[field] !== null) integer(value, field, -Number.MAX_SAFE_INTEGER);
  if (
    value.records === 0
      ? value.first_valid_at !== null || value.last_valid_at !== null
      : value.first_valid_at === null ||
        value.last_valid_at === null ||
        value.first_valid_at > value.last_valid_at
  )
    fail();
}
function safeError(error, committing) {
  try {
    const known = [
      "invalid_response",
      "invalid_request",
      "invalid_configuration",
      "transport",
      "http",
      "aborted",
      "behind",
      "response_too_large",
      "truncated_response",
      "truncated_export",
      "search_pending",
      "search_completed",
      "search_failed",
    ];
    let code = "transport",
      status;
    if (error instanceof OojuroError) {
      const codeDescriptor = Object.getOwnPropertyDescriptor(error, "code");
      const statusDescriptor = Object.getOwnPropertyDescriptor(error, "status");
      const rawCode =
        codeDescriptor && Object.hasOwn(codeDescriptor, "value") ? codeDescriptor.value : undefined;
      const rawStatus =
        statusDescriptor && Object.hasOwn(statusDescriptor, "value")
          ? statusDescriptor.value
          : undefined;
      if (typeof rawCode === "string" && known.includes(rawCode)) code = rawCode;
      if (
        typeof rawStatus === "number" &&
        Number.isInteger(rawStatus) &&
        rawStatus >= 100 &&
        rawStatus <= 599
      )
        status = rawStatus;
    }
    return new OojuroError(code, { status, outcome: committing ? "unknown" : "none" });
  } catch {
    return new OojuroError("transport", { outcome: committing ? "unknown" : "none" });
  }
}
function options(input = {}) {
  const min = input.minGeneration ?? 0;
  if (!Number.isSafeInteger(min) || min < 0) fail("invalid_request");
  return { min, signal: input.signal };
}
function abort(signal) {
  if (signal?.aborted) fail("aborted");
}
async function cancel(body) {
  try {
    await body?.cancel();
  } catch {
    /* Never surface transport-provided content. */
  }
}
function generation(response, minimum) {
  const header = response.headers.get("x-oojuro-generation");
  if (
    !header ||
    header.length > 16 ||
    !/^(0|[1-9][0-9]*)$/.test(header) ||
    BigInt(header) > BigInt(Number.MAX_SAFE_INTEGER)
  )
    fail();
  const value = Number(header);
  if (value < minimum) fail("behind");
  return value;
}
function contentType(response, expected) {
  if (response.headers.get("content-type")?.split(";", 1)[0].trim().toLowerCase() !== expected) {
    void cancel(response.body);
    fail();
  }
}
async function* chunks(response, limit, signal, ownedTarget) {
  const declared = response.headers.get("content-length");
  if (
    declared &&
    (declared.length > 16 || !/^[0-9]+$/.test(declared) || BigInt(declared) > BigInt(limit))
  ) {
    await cancel(response.body);
    fail("response_too_large");
  }
  if (!response.body) fail();
  const reader = response.body.getReader();
  let bytes = 0;
  let ended = false;
  const stop = () => {
    void reader.cancel().catch(() => {});
  };
  signal?.addEventListener("abort", stop, { once: true });
  try {
    while (true) {
      abort(signal);
      let part = await reader.read();
      abort(signal);
      if (part.done) {
        if (declared !== null && bytes !== Number(declared)) fail("truncated_response");
        ended = true;
        return;
      }
      if (ownedTarget) {
        let length;
        try {
          if (!(part.value instanceof Uint8Array)) fail();
          const buffer = typedArrayBuffer.call(part.value);
          // Intrinsic ArrayBuffer branding rejects SharedArrayBuffer across realms.
          arrayBufferLength.call(buffer);
          length = typedArrayLength.call(part.value);
          if (!length) fail();
          if (length > limit - bytes) fail("response_too_large");
          ownedTarget.set(new Uint8Array(buffer, typedArrayOffset.call(part.value), length), bytes);
        } catch (error) {
          if (error instanceof OojuroError) throw error;
          fail();
        }
        bytes += length;
        // Do not retain the borrowed view/backing buffer across the generator yield.
        part = null;
        yield bytes;
      } else {
        if (!(part.value instanceof Uint8Array) || (bytes += part.value.length) > limit)
          fail("response_too_large");
        yield part.value;
      }
    }
  } finally {
    signal?.removeEventListener("abort", stop);
    if (!ended) void reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
async function bytes(response, limit, signal, ownImmediately = false) {
  if (ownImmediately) {
    const answer = new Uint8Array(limit);
    let total = 0;
    for await (const length of chunks(response, limit, signal, answer)) total = length;
    return answer.subarray(0, total);
  }
  const parts = [];
  let total = 0;
  for await (const part of chunks(response, limit, signal)) {
    parts.push(part);
    total += part.length;
  }
  const answer = new Uint8Array(total);
  let at = 0;
  for (const part of parts) {
    answer.set(part, at);
    at += part.length;
  }
  return answer;
}

export function reportedTrace(text, fields = {}) {
  try {
    const value = parse(
      encode({
        format: 2,
        text,
        kind: "assistant",
        agent_id: "",
        parent_id: "",
        source: "sdk",
        session: "",
        parent_session: "",
        turn: 0,
        sequence: 0,
        parent: 0,
        revises: 0,
        valid_us: 0,
        recorded_us: 0,
        duration_us: 0,
        tool: "",
        arguments: null,
        status: "",
        exit_code: null,
        tokens_in: 0,
        tokens_out: 0,
        indexed: true,
        metadata: {},
        raw: null,
        ...fields,
      }),
    );
    trace(value);
    return value;
  } catch {
    fail("invalid_request");
  }
}
export class PreparedCommit {
  #send;
  constructor(send) {
    this.#send = send;
  }
  send(input = {}) {
    try {
      return this.#send(input.signal);
    } catch {
      return Promise.reject(new OojuroError("invalid_request"));
    }
  }
  toJSON() {
    return { type: "PreparedCommit" };
  }
  [inspect]() {
    return "PreparedCommit {}";
  }
}
export class PreparedSearch extends PreparedCommit {
  toJSON() { return { type: "PreparedSearch" }; }
  [inspect]() { return "PreparedSearch {}"; }
}
export class PreparedJournalWrite extends PreparedCommit {
  toJSON() { return { type: "PreparedJournalWrite" }; }
  [inspect]() { return "PreparedJournalWrite {}"; }
}
export class PreparedJournalPeriods extends PreparedSearch {
  toJSON() { return { type: "PreparedJournalPeriods" }; }
  [inspect]() { return "PreparedJournalPeriods {}"; }
}
export class OojuroClient {
  #base;
  #project;
  #token;
  #fetch;
  #requestLimit;
  #responseLimit;
  #framedRequestLimit;
  #framedResponseLimit;
  constructor(configuration) {
    try {
      const {
        baseUrl,
        project,
        token,
        fetch: transport = globalThis.fetch,
        maxRequestBytes = 8 * MiB,
        maxResponseBytes = 10 * MiB,
        maxFramedRequestBytes = recordWireLimit,
        maxFramedResponseBytes = recordWireLimit,
      } = configuration;
      const url = new URL(baseUrl);
      const local =
        url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
      if (
        (!local && url.protocol !== "https:") ||
        url.username ||
        url.password ||
        url.search ||
        url.hash ||
        url.pathname !== "/" ||
        typeof transport !== "function"
      )
        fail();
      id(project);
      if (
        typeof token !== "string" ||
        !/^ojv2_[A-Za-z0-9_-]{43}$/.test(token) ||
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_".indexOf(token.at(-1)) %
          4 !==
          0
      )
        fail();
      for (const [value, max] of [
        [maxRequestBytes, 8 * MiB],
        [maxResponseBytes, 10 * MiB],
        [maxFramedRequestBytes, recordWireLimit],
        [maxFramedResponseBytes, recordWireLimit],
      ])
        if (!Number.isSafeInteger(value) || value < 1 || value > max) fail();
      this.#project = project;
      this.#base = `${url.origin}/v2/projects/${project}/`;
      this.#token = token;
      this.#fetch = transport;
      this.#requestLimit = maxRequestBytes;
      this.#responseLimit = maxResponseBytes;
      this.#framedRequestLimit = maxFramedRequestBytes;
      this.#framedResponseLimit = maxFramedResponseBytes;
    } catch {
      fail("invalid_configuration");
    }
  }
  /** Public destination metadata; never the bearer token. */
  get origin() { return new URL(this.#base).origin; }
  toJSON() {
    return { type: "OojuroClient" };
  }
  [inspect]() {
    return "OojuroClient {}";
  }
  async #request(method, path, body, { min = 0, signal, searchStatus = false, frame } = {}) {
    abort(signal);
    if (frame && !((method === "POST" && path === "journal/records/framed") ||
        (method === "GET" && /^journal\/records\/[0-9a-f]{64}\/framed$/.test(path)))) fail("invalid_request");
    const response = await this.#fetch(this.#base + path, {
      method,
      headers: {
        authorization: `Bearer ${this.#token}`,
        "x-oojuro-min-generation": String(min),
        ...(frame ? (method === "POST" ? {
          "content-type": recordMedia, "content-length": String(body.byteLength), "content-encoding": "identity",
        } : {accept: recordMedia, "x-oojuro-journal-root": frame.root}) :
          body === undefined ? {} : { "content-type": "application/json" }),
      },
      ...(body === undefined ? {} : { body }),
      signal,
      redirect: "error",
      cache: "no-store",
      credentials: "omit",
      referrerPolicy: "no-referrer",
    });
    if (!Number.isInteger(response.status) || response.status < 100 || response.status > 599)
      fail();
    if (response.status !== 200) {
      if (searchStatus && response.status === 409) {
        try {
          contentType(response, "application/json");
          const value = parse(new TextDecoder("utf-8", { fatal: true }).decode(await bytes(response, 4096, signal)));
          const code = value?.error?.code;
          if (["search_pending", "search_completed", "search_failed"].includes(code))
            throw new OojuroError(code, { status: 409, outcome: "unknown" });
        } catch (error) {
          if (error instanceof OojuroError && ["search_pending", "search_completed", "search_failed"].includes(error.code)) throw error;
        }
      }
      await cancel(response.body);
      throw new OojuroError("http", {
        status: response.status,
        outcome: response.status >= 500 ? "unknown" : "rejected",
      });
    }
    try {
      return { response, generation: generation(response, min) };
    } catch (error) {
      await cancel(response.body);
      throw error;
    }
  }
  prepareCommit(input) {
    try {
      const body = encode({
        request_id: input.requestId,
        scope: input.scope,
        mutations: input.mutations,
      });
      if (encoder.encode(body).length > this.#requestLimit) fail();
      const value = parse(body);
      keys(value, ["request_id", "scope", "mutations"]);
      string(value.request_id, 1024, true);
      id(value.scope);
      if (
        !Array.isArray(value.mutations) ||
        !value.mutations.length ||
        value.mutations.length > 100
      )
        fail();
      value.mutations.forEach(mutation);
      return new PreparedCommit(async (signal) => {
        try {
          const result = await this.#request("POST", "commits", body, { signal });
          contentType(result.response, "application/json");
          const json = parse(
            new TextDecoder("utf-8", { fatal: true }).decode(
              await bytes(result.response, this.#responseLimit, signal),
            ),
          );
          keys(json, ["outcome"]);
          outcome(json.outcome, result.generation, value.mutations);
          return json.outcome;
        } catch (error) {
          throw safeError(error, true);
        }
      });
    } catch {
      fail("invalid_request");
    }
  }
  async #read(path, validate, input, body, flags = {}) {
    try {
      const config = options(input);
      const result = await this.#request(body === undefined ? "GET" : "POST", path, body, { ...config, searchStatus: flags.searchStatus });
      contentType(result.response, "application/json");
      const json = parse(
        new TextDecoder("utf-8", { fatal: true }).decode(
          await bytes(result.response, this.#responseLimit, config.signal),
        ),
      );
      keys(json, ["watermark", "value"]);
      mark(json.watermark, result.generation);
      validate(json.value, result.generation);
      return json;
    } catch (error) {
      throw safeError(error, flags.metered === true);
    }
  }

  journalRoot(input) {
    return this.#read("journal/root", (value) => journalRoot(value, this.#project), input);
  }
  #journalBody(wire) {
    const body = encode(journalSnapshot(wire, Math.min(MiB, this.#requestLimit)));
    // Journal currently uses the small POST ceiling, even if /commits is configured larger.
    if (encoder.encode(body).length > Math.min(MiB, this.#requestLimit)) fail();
    return { body, value: parse(body) };
  }
  #prepareJournalWrite(path, wire, appended) {
    try {
      const { body, value } = this.#journalBody(wire);
      string(value.request_id, 1024, true);
      journalRoot(value.expected_root, this.#project);
      id(value.writer);
      if (appended) {
        if (value.consent !== "intentional-user-file") fail();
        label(value.adapter, 256);
        nativeInput(value.input);
      } else if (path.endsWith("/forget")) label(value.key, 1024);
      else id(value.id);
      return new PreparedJournalWrite(async (signal) => {
        try {
          const result = await this.#request("POST", path, body, { signal });
          contentType(result.response, "application/json");
          const json = parse(
            new TextDecoder("utf-8", { fatal: true }).decode(
              await bytes(result.response, this.#responseLimit, signal),
            ),
          );
          keys(json, ["outcome"]);
          outcome(json.outcome, result.generation, appended ? [{ kind: "Record" }] : []);
          return json.outcome;
        } catch (error) {
          throw safeError(error, true);
        }
      });
    } catch {
      fail("invalid_request");
    }
  }

  prepareJournalRecordFramed(input) {
    try {
      const original = ownFields(input, [
        "requestId",
        "consent",
        "expectedRoot",
        "writer",
        "adapter",
        "input",
      ]);
      const fields = ownFields(original.input, nativeFields);
      const text = fields.text;
      const textLength = utf8Length(text, recordTextLimit);
      delete fields.text;
      const metadata = journalSnapshot(
        {
          request_id: original.requestId,
          consent: original.consent,
          expected_root: original.expectedRoot,
          writer: original.writer,
          adapter: original.adapter,
          input: fields,
          text_length: textLength,
        },
        recordMetadataLimit,
      );
      string(metadata.request_id, 1024, true);
      journalRoot(metadata.expected_root, this.#project);
      id(metadata.writer);
      label(metadata.adapter, 256);
      if (metadata.consent !== "intentional-user-file") fail();
      nativeInput({ ...metadata.input, text: "" });
      const meta = encoder.encode(encode(metadata));
      if (
        meta.length > recordMetadataLimit ||
        4 + meta.length + textLength > this.#framedRequestLimit
      )
        fail();
      const frame = new Uint8Array(4 + meta.length + textLength);
      new DataView(frame.buffer).setUint32(0, meta.length, false);
      frame.set(meta, 4);
      const written = encoder.encodeInto(text, frame.subarray(4 + meta.length));
      if (written.read !== text.length || written.written !== textLength) fail();
      return new PreparedJournalWrite(async (signal) => {
        try {
          abort(signal);
          // Give each transport attempt its own bytes; mutation cannot alter the original retry.
          const result = await this.#request("POST", "journal/records/framed", frame.slice(), {
            signal,
            frame: {},
          });
          contentType(result.response, "application/json");
          const json = parse(
            new TextDecoder("utf-8", { fatal: true }).decode(
              await bytes(result.response, this.#responseLimit, signal),
            ),
          );
          keys(json, ["outcome"]);
          outcome(json.outcome, result.generation, [{ kind: "Record" }]);
          return json.outcome;
        } catch (error) {
          throw safeError(error, true);
        }
      });
    } catch {
      fail("invalid_request");
    }
  }
  async journalRecordFramed(input, readOptions) {
    let root, config;
    try {
      input = journalSnapshot(input, 2048);
      keys(input, ["expectedRoot", "id"]);
      journalRoot(input.expectedRoot, this.#project);
      id(input.id);
      root = encoder.encode(encode(input.expectedRoot));
      if (root.length > 1024) fail();
      config = options(readOptions);
    } catch {
      fail("invalid_request");
    }
    try {
      const result = await this.#request(
        "GET",
        "journal/records/" + input.id + "/framed",
        undefined,
        { ...config, frame: { root: base64url(root) } },
      );
      if (
        result.response.headers.get("content-type") !== recordMedia ||
        ![null, "identity"].includes(result.response.headers.get("content-encoding"))
      ) {
        await cancel(result.response.body);
        fail();
      }
      const wire = await bytes(result.response, this.#framedResponseLimit, config.signal, true);
      let value;
      try {
        if (wire.length < 4) fail();
        const n = new DataView(wire.buffer, wire.byteOffset, wire.byteLength).getUint32(0, false);
        if (n === 0 || n > recordMetadataLimit || n > wire.length - 4) fail();
        const metadata = frameMetadata(
          new TextDecoder("utf-8", { fatal: true }).decode(wire.subarray(4, 4 + n)),
        );
        keys(metadata, ["schema", "text_length", "id", "record"]);
        integer(metadata, "schema", 1, 1);
        integer(metadata, "text_length", 0, recordTextLimit);
        id(metadata.id);
        nativeRecordMetadata(metadata.record);
        if (metadata.id !== input.id || metadata.text_length !== wire.length - 4 - n) fail();
        const text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(
          wire.subarray(4 + n),
        );
        value = { id: metadata.id, record: { ...metadata.record, text } };
      } catch {
        fail();
      }
      return { generation: result.generation, value };
    } catch (error) {
      throw safeError(error, false);
    }
  }
  prepareJournalRecord(input) {
    try {
      input = journalSnapshot(input, Math.min(MiB, this.#requestLimit));
      return this.#prepareJournalWrite(
        "journal/records",
        {
          request_id: input.requestId,
          consent: input.consent,
          expected_root: input.expectedRoot,
          writer: input.writer,
          adapter: input.adapter,
          input: input.input,
        },
        true,
      );
    } catch {
      fail("invalid_request");
    }
  }
  prepareJournalDelete(input) {
    try {
      input = journalSnapshot(input, Math.min(MiB, this.#requestLimit));
      return this.#prepareJournalWrite(
        "journal/delete",
        {
          request_id: input.requestId,
          expected_root: input.expectedRoot,
          writer: input.writer,
          id: input.id,
        },
        false,
      );
    } catch {
      fail("invalid_request");
    }
  }
  prepareJournalForget(input) {
    try {
      input = journalSnapshot(input, Math.min(MiB, this.#requestLimit));
      return this.#prepareJournalWrite(
        "journal/forget",
        {
          request_id: input.requestId,
          expected_root: input.expectedRoot,
          writer: input.writer,
          key: input.key,
        },
        false,
      );
    } catch {
      fail("invalid_request");
    }
  }
  journalPage(input, readOptions) {
    return this.#journalPage(input, readOptions, false);
  }
  journalDescriptorsPage(input, readOptions) {
    return this.#journalPage(input, readOptions, true);
  }
  #journalPage(input, readOptions, descriptors) {
    try {
      input = journalSnapshot(input, Math.min(MiB, this.#requestLimit));
      const { body, value } = this.#journalBody({
        expected_root: input.expectedRoot,
        query: input.query,
        cursor: input.cursor ?? null,
        max_records: input.maxRecords ?? 100,
        max_bytes: input.maxBytes ?? MiB,
      });
      journalRoot(value.expected_root, this.#project);
      journalQuery(value.query);
      if (value.cursor !== null) string(value.cursor, 16 * 1024, true);
      integer(value, "max_records", 1, 1000);
      integer(value, "max_bytes", 1024, 10 * MiB - 1024);
      return this.#read(
        descriptors ? "journal/descriptors" : "journal/page",
        (page, current) => {
          keys(page, ["watermark", "records", "next"]);
          mark(page.watermark, page.watermark?.generation);
          if (
            page.watermark.generation > current ||
            (value.query.generation !== null &&
              page.watermark.generation < value.query.generation) ||
            !Array.isArray(page.records) ||
            page.records.length > value.max_records
          )
            fail();
          const seen = new Set();
          for (const row of page.records) {
            if (descriptors) {
            journalDescriptor(row);
            if (row.generation > page.watermark.generation ||
                (value.query.generation !== null && row.generation > value.query.generation)) fail();
          } else journalRecord(row);
            if (
              seen.has(row.id) ||
              (value.query.writer !== null && row.record.writer !== value.query.writer) ||
              (value.query.mode.mode === "Id" && row.id !== value.query.mode.id) ||
              (value.query.mode.mode === "History" && row.record.key !== value.query.mode.key)
            )
              fail();
            seen.add(row.id);
          }
          if (page.next !== null) string(page.next, 16 * 1024, true);
          if (encoder.encode(encode(page)).length > value.max_bytes) fail();
        },
        readOptions,
        body,
      );
    } catch {
      return Promise.reject(new OojuroError("invalid_request"));
    }
  }
  journalStats(input, readOptions) {
    try {
      input = journalSnapshot(input, Math.min(MiB, this.#requestLimit));
      const { body, value } = this.#journalBody({
        expected_root: input.expectedRoot,
        query: input.query,
      });
      journalRoot(value.expected_root, this.#project);
      journalQuery(value.query);
      return this.#read("journal/stats", journalStats, readOptions, body);
    } catch {
      return Promise.reject(new OojuroError("invalid_request"));
    }
  }
  prepareJournalPeriods(input) {
    try {
      input = journalSnapshot(input, Math.min(MiB, this.#requestLimit));
      const { body, value } = this.#journalBody({
        request_id: input.requestId,
        expected_root: input.expectedRoot,
        query: input.query,
        width_us: input.widthUs,
        max_periods: input.maxPeriods,
      });
      string(value.request_id, 1024, true);
      journalRoot(value.expected_root, this.#project);
      journalQuery(value.query);
      integer(value, "width_us", 1);
      integer(value, "max_periods", 1, 1000);
      const minimum = options(input).min;
      return new PreparedJournalPeriods((signal) =>
        this.#read(
          "journal/periods",
          (periods) => {
            if (!Array.isArray(periods) || periods.length > value.max_periods) fail();
            let previous = -Infinity;
            for (const period of periods) {
              keys(period, ["start_us", "records", "text_bytes"]);
              integer(period, "start_us", -Number.MAX_SAFE_INTEGER);
              integer(period, "records", 1);
              integer(period, "text_bytes");
              if (
                period.start_us <= previous ||
                BigInt(period.start_us) % BigInt(value.width_us) !== 0n
              )
                fail();
              previous = period.start_us;
            }
          },
          { minGeneration: minimum, signal },
          body,
          { metered: true, searchStatus: true },
        ),
      );
    } catch {
      fail("invalid_request");
    }
  }
  access(input) {
    return this.#read("access", grant, input);
  }
  records(input) {
    return this.#read(
      "records",
      (value) => {
        if (!Array.isArray(value) || value.length > 10000) fail();
        value.forEach(record);
      },
      input,
    );
  }
  record(recordId, input) {
    try {
      id(recordId);
    } catch {
      return Promise.reject(new OojuroError("invalid_request"));
    }
    return this.#read(
      `records/${recordId}`,
      (value) => {
        record(value);
        if (value.id !== recordId) fail();
      },
      input,
    );
  }
  prepareSearch(input) {
    try {
      const body = encode({ request_id: input.requestId, query: input.query, k: input.k });
      const value = parse(body);
      keys(value, ["request_id", "query", "k"]);
      string(value.request_id, 1024, true);
      string(value.query, 4096, true);
      if (!value.query.trim() || !Number.isSafeInteger(value.k) || value.k < 1 || value.k > 100 || encoder.encode(body).length > this.#requestLimit) fail();
      const minimum = options(input).min;
      return new PreparedSearch(async (signal) => {
        try {
          const result = await this.#request("POST", "search", body, { min: minimum, signal, searchStatus: true });
          contentType(result.response, "application/json");
          const json = parse(new TextDecoder("utf-8", { fatal: true }).decode(await bytes(result.response, this.#responseLimit, signal)));
          keys(json, ["watermark", "value"]);
          mark(json.watermark, result.generation);
          if (!Array.isArray(json.value) || json.value.length > value.k) fail();
          for (const hit of json.value) {
            keys(hit, ["id", "writer", "writer_name", "memory_name", "score_bits", "continued", "excerpt_start", "excerpt_end", "excerpt", "trace"]);
            const { score_bits, excerpt, continued, excerpt_start, excerpt_end, ...row } = hit;
            record(row); string(excerpt);
            integer(hit, "excerpt_start"); integer(hit, "excerpt_end");
            const text = encoder.encode(row.trace.text);
            if (typeof score_bits !== "number" || !Number.isFinite(score_bits) || typeof continued !== "boolean" || excerpt_start > excerpt_end || excerpt_end > text.length) fail();
            let exact;
            try { exact = new TextDecoder("utf-8", { fatal: true }).decode(text.subarray(excerpt_start, excerpt_end)); }
            catch { fail(); }
            if (exact !== excerpt) fail();
          }
          return json;
        } catch (error) { throw safeError(error, true); }
      });
    } catch { fail("invalid_request"); }
  }
  search(input, options) {
    try { return this.prepareSearch(input).send(options); }
    catch { return Promise.reject(new OojuroError("invalid_request")); }
  }
  async blob(writer, name, input) {
    try {
      id(writer);
      string(name, 1024, true);
      if (/[\u0000-\u001f\u007f-\u009f]/u.test(name)) fail("invalid_request");
      const config = options(input);
      const result = await this.#request(
        "GET",
        `blobs?${new URLSearchParams({ writer, name })}`,
        undefined,
        config,
      );
      contentType(result.response, "application/octet-stream");
      const data = await bytes(result.response, Math.min(2 * MiB, this.#responseLimit), config.signal);
      const revision = result.response.headers.get("x-oojuro-blob-revision");
      if (revision !== null) id(revision);
      return { generation: result.generation, bytes: data, ...(revision === null ? {} : { revision }) };
    } catch (error) {
      throw safeError(error, false);
    }
  }
  async exportRecords(input) {
    try {
      const config = options(input);
      const result = await this.#request("GET", "export", undefined, config);
      contentType(result.response, "application/x-ndjson");
      const decoder = new TextDecoder("utf-8", { fatal: true });
      const records = [];
      let pending = "";
      const line = (text) => {
        if (!text || encoder.encode(text).length > 2 * MiB + 16384 || records.length >= 10000)
          fail();
        const row = parse(text);
        record(row);
        records.push(row);
      };
      for await (const part of chunks(result.response, this.#responseLimit, config.signal)) {
        pending += decoder.decode(part, { stream: true });
        let at;
        while ((at = pending.indexOf("\n")) !== -1) {
          line(pending.slice(0, at));
          pending = pending.slice(at + 1);
        }
        if (encoder.encode(pending).length > 2 * MiB + 16384) fail("response_too_large");
      }
      pending += decoder.decode();
      if (pending.length) fail("truncated_export");
      return { generation: result.generation, records };
    } catch (error) {
      throw safeError(error, false);
    }
  }
}

/** Snapshot one original capture event before persistence or delivery. */
export function captureEvent(input) {
  try {
    const event = ownFields(input, ["eventId", "runId", "adapter", "input"]);
    const fields = ownFields(event.input, nativeFields);
    utf8Length(fields.text, recordTextLimit);
    const text = fields.text;
    fields.text = "";
    const meta = journalSnapshot({ ...event, input: fields }, recordMetadataLimit - 128);
    id(meta.eventId);
    id(meta.runId);
    label(meta.adapter, 256);
    nativeInput(meta.input);
    id(meta.input.agent_id);
    meta.input.text = text;
    return meta;
  } catch {
    fail("invalid_request");
  }
}
export class PreparedCapture extends PreparedCommit {
  toJSON() { return { type: "PreparedCapture" }; }
  [inspect]() { return "PreparedCapture {}"; }
}
/** Append-only hosted capture adapter in the existing SDK. It accepts no read/admin key. */
export class OojuroCaptureClient {
  #origin;
  #token;
  #fetch;
  constructor(configuration) {
    try {
      const { baseUrl, token, fetch: transport = globalThis.fetch } = configuration;
      const url = new URL(baseUrl);
      const local = url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
      if ((!local && url.protocol !== "https:") || url.username || url.password ||
          url.search || url.hash || url.pathname !== "/" || typeof transport !== "function" ||
          typeof token !== "string" || !/^ojc_[0-9a-f]{64}_[A-Za-z0-9_-]{43}$/.test(token) ||
          "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_".indexOf(token.at(-1)) % 4 !== 0) fail();
      this.#origin = url.origin;
      this.#token = token;
      this.#fetch = transport;
    } catch { fail("invalid_configuration"); }
  }
  get origin() { return this.#origin; }
  toJSON() { return { type: "OojuroCaptureClient" }; }
  [inspect]() { return "OojuroCaptureClient {}"; }
  async #request(body, signal) {
    abort(signal);
    const response = await this.#fetch(this.#origin + "/api/capture" + (body ? "/events" : ""), {
      method: body ? "POST" : "GET",
      headers: { authorization: `Bearer ${this.#token}`,
        ...(body ? { "content-type": "application/vnd.oojuro.capture.v1",
          "content-length": String(body.byteLength), "content-encoding": "identity" } : {}) },
      ...(body ? { body } : {}), signal, redirect: "error", cache: "no-store",
      credentials: "omit", referrerPolicy: "no-referrer",
    });
    if (response.status !== 200) {
      await cancel(response.body);
      if (!Number.isInteger(response.status) || response.status < 100 || response.status > 599) fail();
      throw new OojuroError("http", { status: response.status });
    }
    contentType(response, "application/json");
    return frameMetadata(new TextDecoder("utf-8", { fatal: true }).decode(
      await bytes(response, recordMetadataLimit, signal, true)));
  }
  async discover({ signal } = {}) {
    try {
      const value = await this.#request(undefined, signal);
      keys(value, ["format", "journal", "agent_id", "key_id", "producer_id", "label", "expires_us", "limits"]);
      integer(value, "format", 1, 1);
      for (const name of ["journal", "agent_id", "key_id", "producer_id"]) id(value[name]);
      label(value.label, 128);
      integer(value, "expires_us", 1);
      keys(value.limits, ["events", "metadata_bytes", "text_bytes"]);
      integer(value.limits, "events", 100, 100);
      integer(value.limits, "metadata_bytes", recordMetadataLimit, recordMetadataLimit);
      integer(value.limits, "text_bytes", recordTextLimit, recordTextLimit);
      return value;
    } catch (error) { throw safeError(error, false); }
  }
  prepareBatch(input) {
    try {
      const original = ownFields(input, ["requestId", "events"]);
      if (typeof original.requestId !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(original.requestId) ||
          !Array.isArray(original.events) || original.events.length < 1 || original.events.length > 100) fail();
      const events = [];
      const texts = [];
      const ids = [];
      let total = 0;
      for (let index = 0; index < original.events.length; index++) {
        const descriptor = Object.getOwnPropertyDescriptor(original.events, String(index));
        if (!descriptor || !Object.hasOwn(descriptor, "value")) fail();
        const event = captureEvent(descriptor.value);
        if (ids.includes(event.eventId)) fail();
        ids.push(event.eventId);
        const length = utf8Length(event.input.text, recordTextLimit - total);
        total += length;
        texts.push(event.input.text);
        event.input.text = "";
        events.push({ event_id: event.eventId, run_id: event.runId, adapter: event.adapter,
          input: event.input, text_length: length });
      }
      const metadata = journalSnapshot({ format: 1, request_id: original.requestId, events }, recordMetadataLimit);
      const meta = encoder.encode(encode(metadata));
      const wire = new Uint8Array(4 + meta.length + total);
      new DataView(wire.buffer).setUint32(0, meta.length, false);
      wire.set(meta, 4);
      let at = 4 + meta.length;
      for (const text of texts) {
        const length = utf8Length(text, recordTextLimit);
        const written = encoder.encodeInto(text, wire.subarray(at, at + length));
        if (written.read !== text.length || written.written !== length) fail();
        at += length;
      }
      return new PreparedCapture(async (signal) => {
        try {
          const receipt = await this.#request(wire.slice(), signal);
          keys(receipt, ["request_id", "generation", "recorded_bytes", "records"]);
          if (receipt.request_id !== original.requestId || !Array.isArray(receipt.records) || receipt.records.length !== ids.length) fail();
          integer(receipt, "generation", 1);
          integer(receipt, "recorded_bytes", 0, total);
          const records = new Set();
          receipt.records.forEach((row, i) => {
            keys(row, ["event_id", "record_id", "replayed"]);
            if (row.event_id !== ids[i] || typeof row.replayed !== "boolean") fail();
            id(row.record_id);
            if (records.has(row.record_id)) fail();
            records.add(row.record_id);
          });
          return receipt;
        } catch (error) { throw safeError(error, true); }
      });
    } catch { fail("invalid_request"); }
  }
}

/** Shared-project read adapter; ojp_ credentials never authorize capture or administration. */
function projectRow(row, selection) {
  keys(row, ["journal", "record"]);
  id(row.journal);
  const descriptor = row.record;
  if (!object(descriptor)) fail();
  if (Object.hasOwn(descriptor, "capture")) {
    keys(descriptor, ["id", "record", "text_bytes", "generation", "capture"]);
    keys(descriptor.capture, ["event_id", "run_id"]);
    id(descriptor.capture.event_id); id(descriptor.capture.run_id);
    const native = { ...descriptor }; delete native.capture;
    sources.set(native, sources.get(descriptor));
    journalDescriptor(native);
  } else journalDescriptor(descriptor);
  const record = descriptor.record;
  id(record.agent_id);
  if ((selection.agentId !== undefined && record.agent_id !== selection.agentId) ||
      (selection.from !== undefined && record.valid_at < selection.from) ||
      (selection.through !== undefined && record.valid_at > selection.through)) fail();
  return row.journal + descriptor.id;
}
export class OojuroProjectClient {
  #base;
  #token;
  #fetch;
  constructor(configuration) {
    try {
      const { baseUrl, project, token, fetch: transport = globalThis.fetch } = configuration;
      const url = new URL(baseUrl);
      const local = url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
      if ((!local && url.protocol !== "https:") || url.username || url.password ||
          url.search || url.hash || url.pathname !== "/" || typeof transport !== "function" ||
          typeof token !== "string" || !/^ojp_[A-Za-z0-9_-]{32,100}$/.test(token)) fail();
      id(project);
      this.#base = `${url.origin}/api/projects/${project}/`;
      this.#token = token;
      this.#fetch = transport;
    } catch { fail("invalid_configuration"); }
  }
  toJSON() { return { type: "OojuroProjectClient" }; }
  [inspect]() { return "OojuroProjectClient {}"; }
  async #request(path, body, signal, minimum) {
    abort(signal);
    const response = await this.#fetch(this.#base + path, {
      method: body === undefined ? "GET" : "POST",
      headers: { authorization: `Bearer ${this.#token}`,
        ...(minimum === undefined ? {} : { "x-oojuro-min-generation": String(minimum) }),
        ...(body === undefined ? {} : { "content-type": "application/x-www-form-urlencoded" }) },
      ...(body === undefined ? {} : { body }),
      signal, redirect: "error", cache: "no-store", credentials: "omit", referrerPolicy: "no-referrer",
    });
    if (response.status !== 200) {
      await cancel(response.body);
      if (!Number.isInteger(response.status) || response.status < 100 || response.status > 599) fail();
      throw new OojuroError("http", { status: response.status });
    }
    return response;
  }
  async records(input = {}, { signal } = {}) {
    let body, selection;
    try {
      selection = journalSnapshot(input, 2 * MiB);
      if (!object(selection) || Object.keys(selection).some(k => !["from", "through", "agentId", "limit", "cursor"].includes(k))) fail();
      const form = new URLSearchParams();
      for (const key of ["from", "through"]) if (selection[key] !== undefined) {
        integer(selection, key, -Number.MAX_SAFE_INTEGER);
        form.set(key, String(selection[key]));
      }
      if (selection.from !== undefined && selection.through !== undefined && selection.from > selection.through) fail();
      if (selection.agentId !== undefined) { id(selection.agentId); form.set("agent_id", selection.agentId); }
      if (selection.limit !== undefined) { integer(selection, "limit", 1, 1000); form.set("limit", String(selection.limit)); }
      if (selection.cursor !== undefined && selection.cursor !== null) {
        string(selection.cursor, 2 * MiB, true);
        if (!/^[A-Za-z0-9_-]+$/.test(selection.cursor)) fail();
        form.set("cursor", selection.cursor);
      }
      body = form.toString();
      if (encoder.encode(body).length > 2 * MiB) fail();
    } catch { fail("invalid_request"); }
    try {
      const response = await this.#request("records", body, signal);
      contentType(response, "application/json");
      const value = frameMetadata(new TextDecoder("utf-8", { fatal: true }).decode(await bytes(response, 4 * MiB, signal, true)));
      keys(value, ["snapshot", "records", "next"]);
      id(value.snapshot);
      if (!Array.isArray(value.records) || value.records.length > (selection.limit ?? 100)) fail();
      if (value.next !== null) {
        string(value.next, 2 * MiB, true);
        if (!/^[A-Za-z0-9_-]+$/.test(value.next) || value.records.length === 0) fail();
      }
      const seen = new Set();
      for (const row of value.records) {
        const identity = projectRow(row, selection);
        if (seen.has(identity)) fail();
        seen.add(identity);
      }
      return value;
    } catch (error) { throw safeError(error, false); }
  }
  async search(input, { signal } = {}) {
    let selection, body;
    try {
      selection = journalSnapshot(input, 32 * 1024);
      if (!object(selection) || Object.keys(selection).some(k => !["requestId", "query", "from", "through", "agentId", "k"].includes(k))) fail();
      id(selection.requestId); string(selection.query, 4096, true);
      if (!selection.query.trim()) fail();
      const form = new URLSearchParams({ request_id: selection.requestId, q: selection.query });
      for (const key of ["from", "through"]) if (selection[key] !== undefined) {
        integer(selection, key, -Number.MAX_SAFE_INTEGER); form.set(key, String(selection[key]));
      }
      if (selection.from !== undefined && selection.through !== undefined && selection.from > selection.through) fail();
      if (selection.agentId !== undefined) { id(selection.agentId); form.set("agent_id", selection.agentId); }
      if (selection.k !== undefined) { integer(selection, "k", 1, 100); form.set("k", String(selection.k)); }
      body = form.toString();
      if (encoder.encode(body).length > 32 * 1024) fail();
    } catch { fail("invalid_request"); }
    abort(signal);
    try {
      const response = await this.#request("search", body, signal);
      contentType(response, "application/json");
      const value = frameMetadata(new TextDecoder("utf-8", { fatal: true }).decode(await bytes(response, 4 * MiB, signal, true)));
      keys(value, ["snapshot", "hits"]); id(value.snapshot);
      if (!Array.isArray(value.hits) || value.hits.length > (selection.k ?? 20)) fail();
      const seen = new Set();
      for (const hit of value.hits) {
        keys(hit, ["journal", "record", "score_bits", "continued", "excerpt_start", "excerpt_end", "excerpt"]);
        const identity = projectRow({ journal: hit.journal, record: hit.record }, selection);
        if (seen.has(identity) || !hit.record.record.indexed || typeof hit.score_bits !== "number" ||
            !Number.isFinite(hit.score_bits) || typeof hit.continued !== "boolean") fail();
        seen.add(identity);
        integer(hit, "excerpt_start", 0, hit.record.text_bytes);
        integer(hit, "excerpt_end", hit.excerpt_start, hit.record.text_bytes);
        string(hit.excerpt, 320);
        if (encoder.encode(hit.excerpt).length !== hit.excerpt_end - hit.excerpt_start) fail();
      }
      return value;
    } catch (error) { throw safeError(error, true); }
  }
  /** One event-linked manifest or part; arbitrary Blob names are not accepted. */
  async attachmentPart(input, { signal, minGeneration = 0 } = {}) {
    let path, limit;
    try {
      const selected = journalSnapshot(input, 4096);
      if (!object(selected) || Object.keys(selected).some(k => !["journal", "recordId", "part"].includes(k))) fail();
      id(selected.journal); id(selected.recordId);
      if (!Number.isSafeInteger(minGeneration) || minGeneration < 0) fail();
      let suffix = "manifest"; limit = 512 * 1024;
      if (selected.part !== undefined) {
        integer(selected, "part", 0, 63);
        suffix = `parts/${selected.part}`; limit = 256 * 1024;
      }
      path = `journals/${selected.journal}/records/${selected.recordId}/attachment/${suffix}`;
    } catch { fail("invalid_request"); }
    let response;
    try {
      response = await this.#request(path, undefined, signal, minGeneration);
      contentType(response, "application/octet-stream");
      const mark = generation(response, minGeneration);
      const revision = response.headers.get("x-oojuro-blob-revision"); id(revision);
      return { generation: mark, revision, bytes: await bytes(response, limit, signal, true) };
    } catch (error) { await cancel(response?.body); throw safeError(error, false); }
  }
  async content(input, { signal } = {}) {
    let path;
    try {
      const { journal, recordId } = ownFields(input, ["journal", "recordId"]);
      id(journal); id(recordId);
      path = `journals/${journal}/records/${recordId}/content`;
    } catch { fail("invalid_request"); }
    try {
      const response = await this.#request(path, undefined, signal);
      contentType(response, "text/plain");
      return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(await bytes(response, recordTextLimit, signal, true));
    } catch (error) { throw safeError(error, false); }
  }
}
