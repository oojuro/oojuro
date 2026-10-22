import {createHash} from "node:crypto";
const sha=bytes=>createHash("sha256").update(bytes).digest("hex");
export const W="2".repeat(64),P="1".repeat(64),TOKEN="ojv2_"+"A".repeat(43);
export function store() {
  return { blobs: new Map(), requests: new Map(), generation: 0, readable: true, writable: true, calls: [], after: null };
}
export function clientOptions(state) {
  return { baseUrl: "https://synthetic.invalid", project: P, token: TOKEN,
    fetch: async (url, init) => {
      const path = new URL(url);
      state.calls.push({ path: path.pathname, query: path.search, body: init.body });
      let response;
      if (init.method === "POST") {
        if (!state.writable) return new Response(null, { status: 401 });
        const input = JSON.parse(init.body);
        if (input.scope !== W || input.mutations[0].writer !== W) return new Response(null, { status: 403 });
        const old = state.requests.get(input.request_id);
        if (old) {
          if (old.body !== init.body) return new Response(null, { status: 409 });
          response = old.response;
        } else {
          const part = input.mutations[0];
          const oldBlob = state.blobs.get(part.name);
          if ((oldBlob?.revision ?? null) !== part.expected) return new Response(null, { status: 409 });
          const revision = sha(init.body);
          state.blobs.set(part.name, { bytes: Uint8Array.from(part.bytes), revision });
          const generation = ++state.generation;
          response = { outcome: { generation, record_ids: {}, blob_revisions: { 0: revision }, writer: null, key: null, destruction: null, usage: { request_tag: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", intent_fingerprint: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", scope_tag: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc", committed_at_us: 1, recorded_bytes: 0, kept_bytes: 0 } } };
          state.requests.set(input.request_id, { body: init.body, response });
        }
        if (state.after) await state.after(input, state);
        return new Response(JSON.stringify(response), { headers: { "content-type": "application/json", "x-oojuro-generation": String(response.outcome.generation) } });
      }
      if (!state.readable) return new Response(null, { status: 401 });
      if (path.searchParams.get("writer") !== W) return new Response(null, { status: 403 });
      const blob = state.blobs.get(path.searchParams.get("name"));
      if (!blob) return new Response(null, { status: 404 });
      const headers = { "content-type": "application/octet-stream", "x-oojuro-generation": String(state.generation) };
      if (!state.omitRevision) headers["x-oojuro-blob-revision"] = blob.revision;
      const reply = new Response(blob.bytes.slice(), { headers });
      if (state.afterRead) await state.afterRead(path.searchParams.get("name"), state);
      return reply;
    } };
}
