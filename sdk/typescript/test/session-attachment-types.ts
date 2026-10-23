import { captureSession, type CaptureSession, type OojuroCaptureClient, type OojuroClient, type OojuroProjectClient } from "../index.js";
import { prepareJournalAttachment, downloadJournalAttachmentTo, type JournalAttachmentDescriptor } from "../attachments.js";
import type { FileSink } from "../files.js";
declare const capture: OojuroCaptureClient;
declare const uploader: OojuroClient;
declare const project: OojuroProjectClient;
declare const selected: CaptureSession;
declare const descriptor: JournalAttachmentDescriptor;
declare const source: AsyncIterable<Uint8Array>;
declare const sink: FileSink;
const physicalSource: string = capture.source;
const session = captureSession(selected);
prepareJournalAttachment(uploader, capture, { ...descriptor, session }).send(source).then(receipt => {
  downloadJournalAttachmentTo(project, receipt, sink);
});
// @ts-expect-error Capture does not grant native file-write authority.
prepareJournalAttachment(capture, capture, descriptor);
void physicalSource;

import { CaptureSpool } from "../capture.js";
capture.prepareSession({ runId: "a".repeat(64), label: "Continue reviewer", reuseJournal: "b".repeat(64) });
CaptureSpool.open({ directory: "/private/spool", client: capture, reuseJournal: "b".repeat(64) });
CaptureSpool.open({ directory: "/private/spool", client: capture, reuseJournal: null });
// @ts-expect-error Project read authority is not capture session authority.
project.prepareSession({ runId: "a".repeat(64), label: "Reviewer", reuseJournal: "b".repeat(64) });
