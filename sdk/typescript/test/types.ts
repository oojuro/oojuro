import {
  OojuroClient,
  PreparedCommit,
  reportedTrace,
  type AuthorizationGrant,
  type CommitInput,
  type Outcome,
  type Read,
  type TraceRecord,
} from "../index.js";
const project = "1".repeat(64),
  writer = "2".repeat(64);
const client = new OojuroClient({
  baseUrl: "https://example.invalid",
  project,
  token: "provided-by-caller",
  fetch,
});
const trace = reportedTrace("reported untrusted content", {
  indexed: false,
  raw: { source_generation: "local", start: 0, end: 1, emission: 0 },
});
const input: CommitInput = {
  requestId: "caller-owned",
  scope: writer,
  mutations: [
    { kind: "Record", writer, trace },
    { kind: "Blob", writer, name: "artifact", expected: null, bytes: [0, 255] },
  ],
};
const pending: PreparedCommit = client.prepareCommit(input);
const outcome: Promise<Outcome> = pending.send({ signal: new AbortController().signal });
const records: Promise<Read<TraceRecord[]>> = client.records({ minGeneration: 7 });
void outcome;
void records;
// @ts-expect-error Reported trace claims are not an authorization grant.
const authority: AuthorizationGrant = trace;
// @ts-expect-error No implicit raw capture/parser context API.
client.captureRaw("/private/harness/log");
client.prepareCommit({
  requestId: "x",
  scope: writer,
  // @ts-expect-error Arbitrary server lifecycle/admin mutations are outside this client.
  mutations: [{ kind: "Grant", credential: writer, grant: {} }],
});
// @ts-expect-error No JS bigint-to-number coercion or silent u64 precision conversion.
reportedTrace("x", { parent: 1n });
// @ts-expect-error Prepared commits can only be created by the validated client path.
new PreparedCommit(() => {});
void authority;
