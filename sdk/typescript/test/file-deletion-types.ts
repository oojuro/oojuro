import { OojuroClient } from "../index.js";
import { planFileDeletion, prepareFileDeletion, OojuroFileDeletionError, type FileDeletionDescriptor, type FileDeletionProgress } from "../file-deletion.js";
const client = new OojuroClient({ baseUrl: "https://synthetic.invalid", project: "1".repeat(64), token: "ojv2_" + "A".repeat(43) });
const descriptor: FileDeletionDescriptor = await planFileDeletion(client, { deleteId: "3".repeat(64), writer: "2".repeat(64), name: "synthetic selected file", manifestRevision: "4".repeat(64), consent: "delete-selected-user-file" });
const operation = prepareFileDeletion(client, descriptor);
const first: FileDeletionProgress = await operation.removeManifest();
const after: FileDeletionProgress = await operation.cleanupParts();
const indices: readonly number[] = after.remainingPartIndices; void indices; void first;
try { await operation.cleanupParts(); } catch (error) { if (error instanceof OojuroFileDeletionError) { const held: boolean = error.progress.logicalFileDeleted; void held; } }
// @ts-expect-error The retained selected manifest revision is readonly.
descriptor.manifestRevision = "0".repeat(64);
// @ts-expect-error Upload consent is distinct from explicit selected-file deletion consent.
await planFileDeletion(client, { deleteId: "3".repeat(64), writer: "2".repeat(64), name: "synthetic selected file", manifestRevision: "4".repeat(64), consent: "intentional-user-file" });
// @ts-expect-error Exact original manifest revision is required in this bounded slice.
await planFileDeletion(client, { deleteId: "3".repeat(64), writer: "2".repeat(64), name: "synthetic selected file", consent: "delete-selected-user-file" });
