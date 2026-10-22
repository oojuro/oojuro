import { OojuroClient } from "../index.js";
import { prepareFileUpload, downloadFile, downloadFileTo, FILE_PART_BYTES, type FileSink, type FileUploadDescriptor } from "../files.js";
const writer = "2".repeat(64);
const client = new OojuroClient({ baseUrl: "https://synthetic.invalid", project: "1".repeat(64), token: "ojv2_" + "A".repeat(43) });
const descriptor: FileUploadDescriptor = { uploadId: "3".repeat(64), writer, name: "synthetic file", expected: null,
  size: 1, sha256: "4".repeat(64), consent: "intentional-user-file" };
async function* source() { yield new Uint8Array([1]); }
const receipt = await prepareFileUpload(client, descriptor).send(source());
const sink: FileSink = { write(part) { const bounded: Uint8Array = part; void bounded; }, commit(done) { const bytes: number = done.size; void bytes; }, abort() {} };
await downloadFileTo(client, { writer, name: descriptor.name, minGeneration: receipt.generation }, sink);
const stream = await downloadFile(client, { writer, name: descriptor.name });
for await (const part of stream.chunks) { const bytes: Uint8Array = part; void bytes; }
await stream.completion;
const exact: 262144 = FILE_PART_BYTES; void exact;
// @ts-expect-error Explicit user-file consent is mandatory.
prepareFileUpload(client, { ...descriptor, consent: true });
// @ts-expect-error A transactional sink requires abort.
await downloadFileTo(client, { writer, name: descriptor.name }, { write() {}, commit() {} });
