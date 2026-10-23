import {
  OojuroClient,
  type JournalRecordInput,
  type JournalDescriptor,
  type JournalDescriptorsPage,
  type Read,
} from "../index.js";
const client = new OojuroClient({
  baseUrl: "https://example.invalid",
  project: "1".repeat(64),
  token: "caller",
  maxFramedRequestBytes: 16 * 1024 * 1024 + 65536 + 4,
});
declare const input: JournalRecordInput;
client.prepareJournalRecordFramed(input).send();
client
  .journalRecordFramed({ expectedRoot: input.expectedRoot, id: "2".repeat(64) })
  .then((v) => v.value.record.text);
declare const page: Read<JournalDescriptorsPage>;
const descriptor: JournalDescriptor = page.value.records[0];
void descriptor.text_bytes;
// @ts-expect-error Metadata-only discovery has no full text.
void descriptor.record.text;
// @ts-expect-error Root pin is mandatory.
client.journalRecordFramed({ id: "2".repeat(64) });
// @ts-expect-error Framed append retains explicit input/consent contract.
client.prepareJournalRecordFramed({ requestId: "original", input: { text: "body" } });
