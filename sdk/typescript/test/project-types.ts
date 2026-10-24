import { OojuroProjectClient, type ProjectRecordRow } from "../index.js";
const client = new OojuroProjectClient({ baseUrl: "https://example.invalid", project: "provided", token: "provided" });
client.search({ requestId: "provided", query: "words", from: 0, k: 5 }).then(page => page.hits[0]?.excerpt);
// @ts-expect-error Metered searches require an explicit retained request identity.
client.search({ query: "words" });
client.records({ from: 0, through: 100, agentId: "provided", limit: 10, cursor: null }).then(page => {
  const snapshot: string = page.snapshot;
  const row: ProjectRecordRow | undefined = page.records[0];
  if (row) client.content({ journal: row.journal, recordId: row.record.id }).then(text => text.length);
  void snapshot;
});
// @ts-expect-error Project read clients cannot append.
client.prepareBatch({});
// @ts-expect-error Read pages have vector snapshots, never an invented global generation.
client.records().then(page => page.generation);
// @ts-expect-error Content paths require both opaque identities.
client.content({ recordId: "provided" });
