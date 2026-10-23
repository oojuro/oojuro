import { OojuroCaptureClient, type CaptureEvent, captureEvent } from "../index.js";
import { CaptureSpool } from "../capture.js";
const client = new OojuroCaptureClient({ baseUrl: "https://example.invalid", token: "provided" });
declare const event: CaptureEvent;
client.prepareBatch({ requestId: "original", events: [captureEvent(event)] }).send();
client.discover().then(v => v.producer_id);
CaptureSpool.open({ directory: "/protected/capture", client }).then(async spool => {
  const queued: true = spool.enqueue(event).queued;
  const remaining: number = (await spool.flush({ timeoutMs: 30000 })).remaining;
  spool.close(); void queued; void remaining;
});
// @ts-expect-error Capture cannot read or administer a project.
client.records();
// @ts-expect-error Full original input is required.
client.prepareBatch({ requestId: "invalid", events: [{ eventId: "original" }] });
// @ts-expect-error Local persistence is not a committed server receipt.
CaptureSpool.open({ directory: "/protected/capture", client }).then(s => s.enqueue(event).generation);
