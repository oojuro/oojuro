// Experimental current-v2 wire client. No lifecycle/admin, capture, forwarding or raw-file adapter.
const encoder = new TextEncoder();
const opaquePattern = /^[0-9a-f]{64}$/;
const sources = new WeakMap();
const inspect = Symbol.for("nodejs.util.inspect.custom");
const MiB = 1024 * 1024;

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
  keys(value, ["generation", "record_ids", "blob_revisions", "writer", "key", "destruction"]);
  integer(value, "generation");
  if (
    value.generation !== generation ||
    value.writer !== null ||
    value.key !== null ||
    value.destruction !== null
  )
    fail();
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
function safeError(error, committing) {
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
  const code =
    error instanceof OojuroError && known.includes(error.code) ? error.code : "transport";
  return new OojuroError(code, {
    status:
      error instanceof OojuroError &&
      Number.isInteger(error.status) &&
      error.status >= 100 &&
      error.status <= 599
        ? error.status
        : undefined,
    outcome: committing ? "unknown" : "none",
  });
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
async function* chunks(response, limit, signal) {
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
      const part = await reader.read();
      abort(signal);
      if (part.done) {
        if (declared !== null && bytes !== Number(declared)) fail("truncated_response");
        ended = true;
        return;
      }
      if (!(part.value instanceof Uint8Array) || (bytes += part.value.length) > limit)
        fail("response_too_large");
      yield part.value;
    }
  } finally {
    signal?.removeEventListener("abort", stop);
    if (!ended) void reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
async function bytes(response, limit, signal) {
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
export class OojuroClient {
  #base;
  #token;
  #fetch;
  #requestLimit;
  #responseLimit;
  constructor(configuration) {
    try {
      const {
        baseUrl,
        project,
        token,
        fetch: transport = globalThis.fetch,
        maxRequestBytes = 8 * MiB,
        maxResponseBytes = 10 * MiB,
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
      ])
        if (!Number.isSafeInteger(value) || value < 1 || value > max) fail();
      this.#base = `${url.origin}/v2/projects/${project}/`;
      this.#token = token;
      this.#fetch = transport;
      this.#requestLimit = maxRequestBytes;
      this.#responseLimit = maxResponseBytes;
    } catch {
      fail("invalid_configuration");
    }
  }
  toJSON() {
    return { type: "OojuroClient" };
  }
  [inspect]() {
    return "OojuroClient {}";
  }
  async #request(method, path, body, { min = 0, signal, searchStatus = false } = {}) {
    abort(signal);
    const response = await this.#fetch(this.#base + path, {
      method,
      headers: {
        authorization: `Bearer ${this.#token}`,
        "x-oojuro-min-generation": String(min),
        ...(body === undefined ? {} : { "content-type": "application/json" }),
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
  async #read(path, validate, input, body) {
    try {
      const config = options(input);
      const result = await this.#request(body === undefined ? "GET" : "POST", path, body, config);
      contentType(result.response, "application/json");
      const json = parse(
        new TextDecoder("utf-8", { fatal: true }).decode(
          await bytes(result.response, this.#responseLimit, config.signal),
        ),
      );
      keys(json, ["watermark", "value"]);
      mark(json.watermark, result.generation);
      validate(json.value);
      return json;
    } catch (error) {
      throw safeError(error, false);
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
