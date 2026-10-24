import { OojuroCaptureClient, type CaptureEvent, captureEvent } from "../index.js";
import { CaptureSpool } from "../capture.js";
const client = new OojuroCaptureClient({ baseUrl: "https://example.invalid", token: "provided" });
new OojuroCaptureClient({ baseUrl: "https://example.invalid", token: "provided", compression: "identity" });
// @ts-expect-error Capture compression is negotiated gzip or identity, not arbitrary coding.
new OojuroCaptureClient({ baseUrl: "https://example.invalid", token: "provided", compression: "br" });
declare const event: CaptureEvent;
client.prepareBatch({ requestId: "original", events: [captureEvent(event)] }).send();
client.discover().then(v => v.producer_id);
client.prepareSession({ runId: event.runId, label: "Reviewer" }).send().then(session => {
  client.prepareBatch({ requestId: "bound-original", events: [event], session }).send();
});
CaptureSpool.open({ directory: "/protected/capture", client }).then(async spool => {
  const queued: true = spool.enqueue(event).queued;
  const remaining: number = (await spool.flush({ timeoutMs: 30000 })).remaining;
  const known = spool.session(event.runId);
  if (known) client.prepareBatch({ requestId: "known-route", events: [event], session: known });
  spool.close(); void queued; void remaining;
});
// @ts-expect-error Capture cannot read or administer a project.
client.records();
// @ts-expect-error Full original input is required.
client.prepareBatch({ requestId: "invalid", events: [{ eventId: "original" }] });
// @ts-expect-error Local persistence is not a committed server receipt.
CaptureSpool.open({ directory: "/protected/capture", client }).then(s => s.enqueue(event).generation);
