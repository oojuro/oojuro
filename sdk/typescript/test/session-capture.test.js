import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, rmSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { OojuroCaptureClient, OojuroError, captureSession } from "../index.js";
import { CaptureSpool } from "../capture.js";
const id = c => c.repeat(64), SOURCE = id("1"), NAMESPACE = id("2"), AGENT = id("3"), PRODUCER = id("4"), KEY = id("5"), RUN = id("6"), JOURNAL = id("7");
const TOKEN = `ojc_${SOURCE}_${"A".repeat(43)}`;
const info = () => ({ format: 1, journal: NAMESPACE, agent_id: AGENT, key_id: KEY, producer_id: PRODUCER,
  label: "Reviewer", expires_us: 999999999999999, limits: { events: 100, metadata_bytes: 65536, text_bytes: 16777216 },
  journal_mode: "session", session_endpoint: "/api/capture/sessions" });
const route = (run = RUN, journal = JOURNAL) => ({ format: 1, journal, source: SOURCE, agent_id: AGENT, run_id: run,
  project: null, events_url: `/api/capture/journals/${journal}/events` });
const event = (c = "a", run = RUN) => ({ eventId: id(c), runId: run, adapter: "session-sdk-test", input: {
  agent_id: AGENT, parent_id: null, valid_at: 42, kind: "output", tool: null, status: null, exit: null, key: null,
  text: "synthetic original 🦉\0\r\n", indexed: true, attrs: {} } });
const json = value => new Response(JSON.stringify(value), { headers: { "content-type": "application/json" } });
const client = fetch => new OojuroCaptureClient({ baseUrl: "https://fixture.invalid", token: TOKEN, fetch });
const error = (code, outcome) => e => e instanceof OojuroError && e.code === code && (!outcome || e.outcome === outcome);
function decode(wire) { const bytes = Buffer.from(wire), n = bytes.readUInt32BE(0); return { meta: JSON.parse(bytes.subarray(4, 4+n)), text: bytes.subarray(4+n) }; }
function ack(wire) { const { meta } = decode(wire); return { request_id: meta.request_id, generation: 1, recorded_bytes: 0,
  records: meta.events.map(e => ({ event_id: e.event_id, record_id: e.event_id, replayed: false })) }; }
async function directory(run) { const path = mkdtempSync("/tmp/oojuro-session-sdk-"); try { await run(path); } finally { rmSync(path, { recursive: true, force: true }); } }

test("session provisioning freezes intent, validates physical source separately from namespace and uses exact bound path", async () => {
  const creates = [], wires = []; let lose = true;
  const c = client(async (url, init) => {
    if (init.method === "GET") return json(info());
    if (url.endsWith("/sessions")) {
      assert.equal(init.headers["content-type"], "application/json");
      creates.push(Buffer.from(init.body)); init.body.fill(0);
      if (lose) { lose = false; throw Error("accepted setup ACK lost"); }
      return json(route());
    }
    assert.equal(url, "https://fixture.invalid"+route().events_url);
    wires.push(Buffer.from(init.body)); return json(ack(init.body));
  });
  const input = { runId: RUN, label: "Original role" }, prepared = c.prepareSession(input);
  input.label = "Changed";
  await assert.rejects(prepared.send(), error("transport", "unknown"));
  const selected = await prepared.send();
  assert.deepEqual(creates[0], creates[1]); assert.equal(JSON.parse(creates[1]).label, "Original role");
  assert.equal(c.source, SOURCE); assert.notEqual(SOURCE, NAMESPACE);
  const bad = event("b", id("8"));
  assert.throws(() => c.prepareBatch({ requestId: "mixed", events: [event(), bad], session: selected }), error("invalid_request"));
  assert.equal(wires.length, 0);
  const foreign = { ...selected, source: id("9") };
  assert.throws(() => c.prepareBatch({ requestId: "foreign", events: [event()], session: foreign }), error("invalid_request"));
  await c.prepareBatch({ requestId: "same-request", events: [event()], session: selected }).send();
  assert.equal(wires.length, 1); assert.equal(decode(wires[0]).meta.events[0].run_id, RUN);
});

test("session response identity and routing refuse before any event or foreign fetch", async () => {
  const variants = [{ source: NAMESPACE }, { agent_id: id("8") }, { run_id: id("8") },
    { events_url: "https://foreign.invalid/events" }, { events_url: "/api/capture/events" }, { unexpected: true }];
  for (const change of variants) {
    let posts = 0;
    const c = client(async (url, init) => {
      assert.ok(url.startsWith("https://fixture.invalid/api/capture"));
      if (init.method === "GET") return json(info());
      posts++; return json({ ...route(), ...change });
    });
    await assert.rejects(c.prepareSession({ runId: RUN, label: "Reviewer" }).send(), error("invalid_response", "unknown"));
    assert.equal(posts, 1);
  }
  for (const change of [{ journal_mode: "fixed" }, { session_endpoint: "/elsewhere" }]) {
    await assert.rejects(client(async () => json({ ...info(), ...change })).discover(), error("invalid_response"));
  }
  assert.throws(() => captureSession({ ...route(), events_url: route().events_url+"?x=1" }), error("invalid_request"));
});

test("durable session spool retains run labels and exact pending bytes across lost ACKs, offline reopen and rotation refusal", async () => directory(async path => {
  let online = true, loseSetup = true, loseEvent = true, key = KEY;
  const mappings = new Map(), creates = [], wires = [];
  const c = client(async (url, init) => {
    if (!online) throw Error("offline");
    if (init.method === "GET") return json({ ...info(), key_id: key });
    if (url.endsWith("/sessions")) {
      const body = JSON.parse(Buffer.from(init.body)); creates.push(body);
      let saved = mappings.get(body.run_id);
      if (!saved) { saved = { label: body.label, route: route(body.run_id, body.run_id === RUN ? JOURNAL : id("9")) }; mappings.set(body.run_id, saved); }
      assert.equal(body.label, saved.label);
      if (loseSetup) { loseSetup = false; throw Error("accepted setup ACK lost"); }
      return json(saved.route);
    }
    const wire = Buffer.from(init.body), parsed = decode(wire), run = parsed.meta.events[0].run_id;
    assert.ok(parsed.meta.events.every(e => e.run_id === run));
    assert.equal(url, "https://fixture.invalid"+mappings.get(run).route.events_url);
    wires.push({ url, wire });
    if (loseEvent) { loseEvent = false; throw Error("accepted event ACK lost"); }
    return json(ack(wire));
  });
  let spool = await CaptureSpool.open({ directory: path, client: c, sessionLabel: "Original role" });
  spool.enqueue(event()); spool.enqueue(event("b", id("8")));
  await assert.rejects(spool.flush(), error("transport", "unknown"));
  assert.equal(wires.length, 0); spool.close(); online = false;
  spool = await CaptureSpool.open({ directory: path, client: c, sessionLabel: "Changed role" });
  assert.equal(spool.pending().events, 2); await assert.rejects(spool.flush(), error("transport"));
  online = true; await assert.rejects(spool.flush(), error("transport", "unknown"));
  assert.equal(spool.session(RUN).journal, JOURNAL); assert.equal(spool.session(id("8")), undefined);
  spool.close(); spool = await CaptureSpool.open({ directory: path, client: c });
  key = id("a"); await assert.rejects(spool.flush(), error("spool_binding_mismatch"));
  assert.equal(spool.pending().events, 2); assert.equal(wires.length, 1);
  key = KEY; assert.deepEqual(await spool.flush(), { delivered: 2, remaining: 0 });
  assert.equal(mappings.size, 2); assert.ok(creates.every(v => v.label.startsWith("Original role / ")));
  assert.equal(wires[0].url, wires[1].url); assert.deepEqual(wires[0].wire, wires[1].wire);
  assert.equal(spool.session(id("8")).journal, id("9"));
  spool.close();
  const db = new DatabaseSync(path+"/capture.sqlite");
  assert.equal(db.prepare("PRAGMA user_version").get().user_version, 2);
  assert.equal(db.prepare("SELECT count(*) AS n FROM sessions").get().n, 2); db.close();
}));

test("legacy spools retain v1 identity and refuse session-mode use instead of migrating", async () => directory(async path => {
  let mode = false, posts = 0;
  const c = client(async (_url, init) => {
    if (init.method === "GET") { const value = info(); if (!mode) { delete value.journal_mode; delete value.session_endpoint; } return json(value); }
    posts++; return json(ack(init.body));
  });
  let spool = await CaptureSpool.open({ directory: path, client: c }); spool.enqueue(event()); spool.close();
  const db = new DatabaseSync(path+"/capture.sqlite");
  const retained = db.prepare("SELECT binding FROM identity").get().binding;
  assert.equal(retained, JSON.stringify([c.origin, NAMESPACE, AGENT, PRODUCER]));
  assert.equal(db.prepare("PRAGMA user_version").get().user_version, 1); db.close();
  mode = true; spool = await CaptureSpool.open({ directory: path, client: c });
  await assert.rejects(spool.flush(), error("spool_binding_mismatch")); assert.equal(posts, 0);
  assert.equal(spool.pending().events, 1); spool.close();
  const reopened = new DatabaseSync(path+"/capture.sqlite");
  assert.equal(reopened.prepare("PRAGMA user_version").get().user_version, 1);
  assert.equal(reopened.prepare("SELECT binding FROM identity").get().binding, retained); reopened.close();
}));

test("session mapping retention is bounded and mixed persisted batches refuse before provisioning", async () => {
  await directory(async path => {
    let posts = 0;
    const c = client(async (_url, init) => { if (init.method === "GET") return json(info()); posts++; throw Error("must not mutate"); });
    let spool = await CaptureSpool.open({ directory: path, client: c });
    spool.enqueue(event()); spool.enqueue(event("b", id("8"))); spool.close();
    let db = new DatabaseSync(path+"/capture.sqlite");
    db.prepare("INSERT INTO batch VALUES(1,?,?,?)").run(id("e"), KEY, 2); db.close();
    spool = await CaptureSpool.open({ directory: path, client: c });
    await assert.rejects(spool.flush(), error("spool_binding_mismatch")); assert.equal(posts, 0); spool.close();
  });
  await directory(async path => {
    const c = client(async () => json(info()));
    let spool = await CaptureSpool.open({ directory: path, client: c }); spool.close();
    const db = new DatabaseSync(path+"/capture.sqlite"); db.exec("BEGIN IMMEDIATE");
    const insert = db.prepare("INSERT INTO sessions VALUES(?,?,NULL)");
    for (let n = 0; n < 1024; n++) insert.run(n.toString(16).padStart(64, "0"), "retained intent");
    db.exec("COMMIT"); db.close();
    spool = await CaptureSpool.open({ directory: path, client: c });
    assert.throws(() => spool.enqueue(event()), error("session_limit")); assert.equal(spool.pending().events, 0); spool.close();
  });
});

test("explicit reuse freezes target intent and refuses a different returned journal", async () => {
  const requests = []; let lose = true;
  const c = client(async (_url, init) => {
    if (init.method === "GET") return json(info());
    requests.push(Buffer.from(init.body));
    if (lose) { lose = false; throw Error("setup ACK lost"); }
    return json(route());
  });
  const input = { runId: RUN, label: "Continue reviewer", reuseJournal: JOURNAL };
  const prepared = c.prepareSession(input); input.reuseJournal = id("8");
  await assert.rejects(prepared.send(), error("transport", "unknown"));
  assert.equal((await prepared.send()).journal, JOURNAL);
  assert.deepEqual(requests[0], requests[1]);
  assert.equal(JSON.parse(requests[1]).reuse_journal, JOURNAL);
  for (const target of ["display name", "../", id("A"), null, undefined]) {
    assert.throws(() => c.prepareSession({ runId: RUN, label: "Reviewer", reuseJournal: target }), error("invalid_request"));
  }
  let posts = 0;
  const wrong = client(async (_url, init) => {
    if (init.method === "GET") return json(info());
    posts++; return json(route(RUN, id("9")));
  });
  await assert.rejects(wrong.prepareSession({ runId: RUN, label: "Reviewer", reuseJournal: JOURNAL }).send(), error("invalid_response", "unknown"));
  assert.equal(posts, 1, "no event dispatch or fallback after mismatched target");
});

test("reuse upgrade preserves v2 pending defaults and v3 per-run targets across policy changes and lost ACKs", async () => directory(async path => {
  const mappings = new Map(), creates = [], wires = [];
  let loseSetup = true, loseEvent = true;
  const selected = id("9"), next = id("8"), third = id("b"), defaultRun = id("c");
  const c = client(async (url, init) => {
    if (init.method === "GET") return json(info());
    if (url.endsWith("/sessions")) {
      const body = JSON.parse(Buffer.from(init.body)); creates.push(body);
      let saved = mappings.get(body.run_id);
      if (!saved) {
        saved = { body, route: route(body.run_id, body.reuse_journal ?? (body.run_id === RUN ? JOURNAL : body.run_id)) };
        mappings.set(body.run_id, saved);
      }
      assert.deepEqual(body, saved.body);
      if (loseSetup) { loseSetup = false; throw Error("accepted setup ACK lost"); }
      return json(saved.route);
    }
    const wire = Buffer.from(init.body), run = decode(wire).meta.events[0].run_id;
    assert.equal(url, "https://fixture.invalid" + mappings.get(run).route.events_url);
    wires.push(wire);
    if (loseEvent) { loseEvent = false; throw Error("accepted event ACK lost"); }
    return json(ack(wire));
  });
  let spool = await CaptureSpool.open({ directory: path, client: c });
  spool.enqueue(event());
  await assert.rejects(spool.flush(), error("transport", "unknown"));
  spool.close();
  let db = new DatabaseSync(path + "/capture.sqlite");
  assert.equal(db.prepare("PRAGMA user_version").get().user_version, 2); db.close();
  spool = await CaptureSpool.open({ directory: path, client: c, reuseJournal: selected });
  spool.enqueue(event("b", next)); spool.close();
  db = new DatabaseSync(path + "/capture.sqlite");
  assert.equal(db.prepare("PRAGMA user_version").get().user_version, 3);
  assert.equal(db.prepare("SELECT reuse_journal FROM sessions WHERE run=?").get(RUN).reuse_journal, null);
  assert.equal(db.prepare("SELECT reuse_journal FROM sessions WHERE run=?").get(next).reuse_journal, selected);
  db.close();
  spool = await CaptureSpool.open({ directory: path, client: c });
  spool.enqueue(event("c", third)); spool.close();
  spool = await CaptureSpool.open({ directory: path, client: c, reuseJournal: null });
  spool.enqueue(event("d", defaultRun));
  await assert.rejects(spool.flush(), error("transport", "unknown"));
  spool.close(); spool = await CaptureSpool.open({ directory: path, client: c });
  assert.deepEqual(await spool.flush(), { delivered: 4, remaining: 0 });
  assert.deepEqual(wires[0], wires[1], "pending request/original wire survives policy changes and reopen");
  assert.equal(mappings.get(RUN).body.reuse_journal, undefined);
  assert.equal(mappings.get(next).body.reuse_journal, selected);
  assert.equal(mappings.get(third).body.reuse_journal, selected, "undefined open retains saved policy");
  assert.equal(mappings.get(defaultRun).body.reuse_journal, undefined, "null restores unseen runs to default");
  assert.equal(spool.session(next).journal, spool.session(third).journal);
  assert.notEqual(spool.session(defaultRun).journal, selected);
  assert.deepEqual(creates[0], creates[1], "v2 pending setup never adopts the later reuse target");
  spool.close();
}));
