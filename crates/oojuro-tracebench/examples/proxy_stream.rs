//! Build a synthetic "proxy capture" TraceBench stream from a TBEV0001 base:
//! a model proxy sees the whole conversation in every request, so each turn
//! batch re-sends every earlier event of its session (same text, tool and
//! args) before the turn's new events.
//!
//! Usage: proxy_stream <base.tbe> <out.tbe> <keep 1 in N sessions> [max_sessions] [msg|req]
//!
//! `msg` (default): each re-sent event is its own record. `req`: the re-sent
//! history is ONE stored-only record per turn (kind system, tool
//! `proxy.request`): the earlier events' texts joined as a request body
//! (`<kind>\n<text>\n\n` per message), the shape a proxy sees on the wire.
//!
//! Copies get new dense ids, the turn's clock, status none, no parent, no
//! tokens; originals keep their fields (ids, parents and revisions are
//! remapped). Sessions are renumbered densely in order of first appearance
//! and `seq` is the append position within the output session. Revisions
//! are not re-sent. Prints raw bytes of the base subset and of the stream.
use oojuro_tracebench::events::{Base, Event, Tier};
use std::collections::HashMap;
use std::io::Write;

#[derive(Clone)]
struct Prior {
    kind: u8,
    tool: String,
    args: String,
    text: String,
    tokens_out: u32,
}

fn keep(session: u64, n: u64) -> bool {
    let h = blake3::hash(format!("proxy|{session}").as_bytes());
    u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap()) % n == 0
}

#[allow(clippy::too_many_arguments)]
fn put(out: &mut Vec<u8>, id: u64, e: &Event, session: u64, seq: u32, parent: u64, revises: u64, ts: i64) {
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&session.to_le_bytes());
    out.extend_from_slice(&e.agent.to_le_bytes());
    out.extend_from_slice(&e.turn.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&parent.to_le_bytes());
    out.extend_from_slice(&revises.to_le_bytes());
    out.push(e.kind);
    out.push(e.status);
    out.extend_from_slice(&e.exit_code.unwrap_or(i32::MIN).to_le_bytes());
    out.extend_from_slice(&e.tokens_in.to_le_bytes());
    out.extend_from_slice(&e.tokens_out.to_le_bytes());
    out.extend_from_slice(&ts.to_le_bytes());
    out.extend_from_slice(&e.duration_us.to_le_bytes());
    out.extend_from_slice(&(e.tool.len() as u16).to_le_bytes());
    out.extend_from_slice(&(e.args.len() as u32).to_le_bytes());
    out.extend_from_slice(&(e.text.len() as u32).to_le_bytes());
    out.extend_from_slice(e.tool.as_bytes());
    out.extend_from_slice(e.args.as_bytes());
    out.extend_from_slice(e.text.as_bytes());
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let base = Base::open(a[0].as_ref())?;
    let n: u64 = a[2].parse()?;
    let max_sessions: usize = a.get(3).map_or(usize::MAX, |s| s.parse().unwrap());
    let req = a.get(4).is_some_and(|m| m == "req");
    let tier = Tier::new(&base, 1);
    let mut f = std::io::BufWriter::new(std::fs::File::create(&a[1])?);
    f.write_all(b"TBEV0001")?;
    let mut sessions: HashMap<u64, (u64, u32, Vec<Prior>)> = HashMap::new();
    let mut ids: HashMap<u64, u64> = HashMap::new();
    let (mut next_id, mut base_raw, mut out_raw, mut copies, mut originals) = (1u64, 0u64, 0u64, 0u64, 0u64);
    let mut b = Vec::new();
    let mut buf = Vec::new();
    for i in 0..tier.plan.len() {
        let e0 = tier.plan[i];
        if !keep(e0.session, n) {
            continue;
        }
        if !sessions.contains_key(&e0.session) {
            if sessions.len() >= max_sessions {
                continue;
            }
            let s = sessions.len() as u64 + 1;
            sessions.insert(e0.session, (s, 0, Vec::new()));
        }
        tier.batch(i, &mut b);
        let (sid, seq, prior) = sessions.get_mut(&e0.session).unwrap();
        buf.clear();
        let mut count = 0u32;
        let ts = b.iter().map(|e| e.ts_us).min().unwrap();
        if req && !prior.is_empty() {
            let mut body = String::new();
            for p in prior.iter() {
                body.push_str(oojuro_tracebench::events::KIND_NAMES[p.kind as usize]);
                body.push('\n');
                body.push_str(&p.text);
                body.push_str("\n\n");
            }
            let ev = Event {
                id: 0,
                session: 0,
                agent: e0.agent,
                turn: b[0].turn,
                seq: 0,
                parent: 0,
                revises: 0,
                kind: 0,
                status: 0,
                exit_code: None,
                tokens_in: 0,
                tokens_out: 0,
                ts_us: ts,
                duration_us: 0,
                tool: "proxy.request",
                args: "",
                text: &body,
            };
            put(&mut buf, next_id, &ev, *sid, *seq, 0, 0, ts);
            out_raw += ev.raw_bytes();
            next_id += 1;
            *seq += 1;
            count += 1;
            copies += 1;
        }
        for p in prior.iter().filter(|_| !req) {
            let ev = Event {
                id: 0,
                session: 0,
                agent: e0.agent,
                turn: b[0].turn,
                seq: 0,
                parent: 0,
                revises: 0,
                kind: p.kind,
                status: 0,
                exit_code: None,
                tokens_in: 0,
                tokens_out: 0,
                ts_us: ts,
                duration_us: 0,
                tool: &p.tool,
                args: &p.args,
                text: &p.text,
            };
            let _ = p.tokens_out;
            put(&mut buf, next_id, &ev, *sid, *seq, 0, 0, ts);
            out_raw += ev.raw_bytes();
            next_id += 1;
            *seq += 1;
            count += 1;
            copies += 1;
        }
        for e in &b {
            let parent = ids.get(&e.parent).copied().unwrap_or(0);
            let revises = ids.get(&e.revises).copied().unwrap_or(0);
            ids.insert(e.id, next_id);
            put(&mut buf, next_id, e, *sid, *seq, parent, revises, e.ts_us);
            base_raw += e.raw_bytes();
            out_raw += e.raw_bytes();
            next_id += 1;
            *seq += 1;
            count += 1;
            originals += 1;
            if !e.is_revision() {
                prior.push(Prior {
                    kind: e.kind,
                    tool: e.tool.into(),
                    args: e.args.into(),
                    text: e.text.into(),
                    tokens_out: e.tokens_out,
                });
            }
        }
        f.write_all(&count.to_le_bytes())?;
        f.write_all(&buf)?;
    }
    f.flush()?;
    println!(
        "{{\"sessions\": {}, \"original_events\": {originals}, \"copies\": {copies}, \"base_raw\": {base_raw}, \"proxy_raw\": {out_raw}, \"ratio\": {:.2}}}",
        sessions.len(),
        out_raw as f64 / base_raw as f64
    );
    Ok(())
}
