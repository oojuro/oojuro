// Node-only durable delivery. The protocol and validators stay in the existing client.
import { DatabaseSync } from "node:sqlite";
import { constants, lstatSync, openSync, closeSync, fstatSync, fsyncSync, mkdirSync } from "node:fs";
import { isAbsolute, join, dirname, parse as parsePath } from "node:path";
import { randomBytes } from "node:crypto";
import { OojuroCaptureClient, OojuroError, captureEvent } from "./index.js";
const inspect = Symbol.for("nodejs.util.inspect.custom");
const MiB = 1024 * 1024;
const fail = (code) => { throw new OojuroError(code); };
const randomId = () => randomBytes(32).toString("hex");
const internal = Symbol();
function safe(error) {
  if (error instanceof OojuroError) return error;
  return new OojuroError("spool_io");
}
function privateDirectory(path) {
  if (typeof process.getuid !== "function" || !isAbsolute(path) || path !== join(path) || path === parsePath(path).root)
    fail("private_spool_required");
  const parents = [];
  for (let at = dirname(path);; at = dirname(at)) {
    parents.push(at);
    if (at === dirname(at)) break;
  }
  for (const parent of parents.reverse()) {
    const m = lstatSync(parent);
    if (!m.isDirectory() || ![0, process.getuid()].includes(m.uid) ||
        (m.mode & 0o022) && !(m.uid === 0 && (m.mode & 0o1000))) fail("private_spool_required");
  }
  try { mkdirSync(path, { mode: 0o700 }); }
  catch (error) { if (error.code !== "EEXIST") throw error; }
  const m = lstatSync(path);
  if (!m.isDirectory() || m.uid !== process.getuid() || (m.mode & 0o777) !== 0o700) fail("private_spool_required");
  const fd = openSync(dirname(path), constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW);
  try { fsyncSync(fd); } finally { closeSync(fd); }
}
function privateFile(path, create = false) {
  let fd;
  try {
    if (create) {
      try { fd = openSync(path, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600); }
      catch (error) { if (error.code !== "EEXIST") throw error; }
    }
    if (fd === undefined) fd = openSync(path, constants.O_RDONLY | constants.O_NONBLOCK | constants.O_NOFOLLOW);
    const m = fstatSync(fd);
    if (!m.isFile() || m.uid !== process.getuid() || m.nlink !== 1 || (m.mode & 0o777) !== 0o600)
      fail("private_spool_required");
    if (create) fsyncSync(fd);
  } finally { if (fd !== undefined) closeSync(fd); }
}
function binding(origin, info) {
  return JSON.stringify([origin, info.journal, info.agent_id, info.producer_id]);
}
export class CaptureSpool {
  #db;
  #client;
  #binding;
  #maxBytes;
  #maxEvents;
  #flushing = false;
  constructor(key) { if (key !== internal) fail("use_spool_open"); }
  static async open({ directory, client, maxBytes = 64 * MiB, maxEvents = 10000, signal } = {}) {
    let db;
    try {
      if (!(client instanceof OojuroCaptureClient) || !Number.isSafeInteger(maxBytes) || maxBytes < 1024 || maxBytes > 256 * MiB ||
          !Number.isSafeInteger(maxEvents) || maxEvents < 1 || maxEvents > 100000) fail("invalid_configuration");
      privateDirectory(directory);
      const path = join(directory, "capture.sqlite");
      privateFile(path, true);
      for (const suffix of ["-journal", "-wal", "-shm"]) {
        try { privateFile(path + suffix); } catch (error) { if (error.code !== "ENOENT") throw error; }
      }
      db = new DatabaseSync(path, { timeout: 1000, allowExtension: false });
      db.exec("PRAGMA trusted_schema=OFF; PRAGMA journal_mode=DELETE; PRAGMA synchronous=EXTRA; PRAGMA secure_delete=ON;");
      db.exec(`PRAGMA max_page_count=${Math.ceil((2 * maxBytes + 8 * MiB) / 4096)};`);
      db.exec("BEGIN IMMEDIATE");
      try {
        const version = db.prepare("PRAGMA user_version").get().user_version;
        if (version === 0 && db.prepare("SELECT count(*) AS n FROM sqlite_master").get().n === 0) {
          db.exec(`CREATE TABLE identity (singleton INTEGER PRIMARY KEY CHECK(singleton=1), binding TEXT NOT NULL);
            CREATE TABLE events (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, body TEXT NOT NULL, bytes INTEGER NOT NULL);
            CREATE TABLE batch (singleton INTEGER PRIMARY KEY CHECK(singleton=1), request TEXT NOT NULL, key_id TEXT NOT NULL, through INTEGER NOT NULL);
            PRAGMA user_version=1;`);
        } else if (version !== 1) fail("invalid_spool");
        db.exec("COMMIT");
      } catch (error) { db.exec("ROLLBACK"); throw error; }
      let retained = db.prepare("SELECT binding FROM identity WHERE singleton=1").get()?.binding;
      if (retained === undefined) {
        const info = await client.discover({ signal: signal ?? AbortSignal.timeout(10000) });
        const selected = binding(client.origin, info);
        db.exec("BEGIN IMMEDIATE");
        try {
          db.prepare("INSERT OR IGNORE INTO identity VALUES(1,?)").run(selected);
          retained = db.prepare("SELECT binding FROM identity WHERE singleton=1").get().binding;
          if (retained !== selected) fail("spool_binding_mismatch");
          db.exec("COMMIT");
        } catch (error) { db.exec("ROLLBACK"); throw error; }
      }
      const identity = JSON.parse(retained);
      if (!Array.isArray(identity) || identity.length !== 4 || identity[0] !== client.origin ||
          identity.slice(1).some((id) => typeof id !== "string" || !/^[0-9a-f]{64}$/.test(id))) fail("spool_binding_mismatch");
      const fd = openSync(directory, constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW);
      try { fsyncSync(fd); } finally { closeSync(fd); }
      const spool = new CaptureSpool(internal);
      spool.#db = db;
      spool.#client = client;
      spool.#binding = retained;
      spool.#maxBytes = maxBytes;
      spool.#maxEvents = maxEvents;
      if (spool.pending().events > maxEvents || spool.pending().bytes > maxBytes) fail("spool_full");
      return spool;
    } catch (error) { db?.close(); throw safe(error); }
  }
  get agentId() { return JSON.parse(this.#binding)[2]; }
  toJSON() { return { type: "CaptureSpool" }; }
  [inspect]() { return "CaptureSpool {}"; }
  #transaction(operation) {
    if (!this.#db) fail("spool_closed");
    this.#db.exec("BEGIN IMMEDIATE");
    try { const result = operation(); this.#db.exec("COMMIT"); return result; }
    catch (error) { this.#db.exec("ROLLBACK"); throw safe(error); }
  }
  pending() {
    try {
      if (!this.#db) fail("spool_closed");
      const row = this.#db.prepare("SELECT count(*) AS events, coalesce(sum(bytes),0) AS bytes FROM events").get();
      return { events: row.events, bytes: row.bytes };
    } catch (error) { throw safe(error); }
  }
  /** Returns local persistence only. It is never a server acknowledgment. */
  enqueue(input) {
    try {
      const event = captureEvent(input);
      if (event.input.agent_id !== this.agentId) fail("spool_binding_mismatch");
      // Validate the complete single-event wire bounds before retaining anything.
      this.#client.prepareBatch({ requestId: "queued", events: [event] });
      const body = JSON.stringify(event), bytes = Buffer.byteLength(body);
      return this.#transaction(() => {
        const previous = this.#db.prepare("SELECT body FROM events WHERE id=?").get(event.eventId);
        if (previous) {
          if (previous.body !== body) fail("event_conflict");
          return { eventId: event.eventId, queued: true };
        }
        const pending = this.pending();
        if (pending.events >= this.#maxEvents || pending.bytes + bytes > this.#maxBytes) fail("spool_full");
        this.#db.prepare("INSERT INTO events(id,body,bytes) VALUES(?,?,?)").run(event.eventId, body, bytes);
        return { eventId: event.eventId, queued: true };
      });
    } catch (error) { throw safe(error); }
  }
  async flush({ timeoutMs = 30000, signal } = {}) {
    if (this.#flushing) fail("spool_busy");
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 300000) fail("invalid_request");
    this.#flushing = true;
    const deadline = AbortSignal.timeout(timeoutMs);
    signal = signal ? AbortSignal.any([signal, deadline]) : deadline;
    let delivered = 0;
    try {
      while (this.pending().events) {
        const info = await this.#client.discover({ signal });
        if (binding(this.#client.origin, info) !== this.#binding) fail("spool_binding_mismatch");
        const batch = this.#transaction(() => {
          let current = this.#db.prepare("SELECT request,key_id,through FROM batch WHERE singleton=1").get();
          if (!current) {
            const rows = this.#db.prepare("SELECT seq,body FROM events ORDER BY seq LIMIT ?").all(info.limits.events);
            if (!rows.length) return null;
            let through, textBytes = 0;
            const metadata = [];
            for (const row of rows) {
              const event = JSON.parse(row.body), length = Buffer.byteLength(event.input.text);
              const part = { event_id: event.eventId, run_id: event.runId, adapter: event.adapter,
                input: { ...event.input, text: "" }, text_length: length };
              const size = Buffer.byteLength(JSON.stringify({ format: 1, request_id: "f".repeat(64), events: [...metadata, part] }));
              if (textBytes + length > info.limits.text_bytes || size > info.limits.metadata_bytes) {
                if (!metadata.length) fail("capture_limit");
                break;
              }
              textBytes += length; metadata.push(part); through = row.seq;
            }
            current = { request: randomId(), key_id: info.key_id, through };
            this.#db.prepare("INSERT INTO batch VALUES(1,?,?,?)").run(current.request, current.key_id, through);
          } else if (current.key_id !== info.key_id) {
            current.request = randomId(); current.key_id = info.key_id;
            this.#db.prepare("UPDATE batch SET request=?,key_id=? WHERE singleton=1").run(current.request, current.key_id);
          }
          const events = this.#db.prepare("SELECT body FROM events WHERE seq<=? ORDER BY seq").all(current.through).map(row => JSON.parse(row.body));
          return { ...current, events };
        });
        if (!batch) break;
        await this.#client.prepareBatch({ requestId: batch.request, events: batch.events }).send({ signal });
        delivered += this.#transaction(() => {
          const current = this.#db.prepare("SELECT request FROM batch WHERE singleton=1").get();
          if (current?.request !== batch.request) return 0;
          const result = this.#db.prepare("DELETE FROM events WHERE seq<=?").run(batch.through);
          this.#db.exec("DELETE FROM batch WHERE singleton=1");
          return result.changes;
        });
      }
      return { delivered, remaining: this.pending().events };
    } catch (error) { throw safe(error); }
    finally { this.#flushing = false; }
  }
  close() {
    if (this.#flushing) fail("spool_busy");
    this.#db?.close(); this.#db = undefined;
  }
}
