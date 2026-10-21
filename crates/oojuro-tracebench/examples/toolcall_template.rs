//! Template + variables for tool-call text (CLP / OpenZL style), measured on
//! the tool-call share of a TraceBench stream. Per agent, tool calls in
//! append order are cut into frames of `frame` raw bytes (the random access
//! unit) and compressed two ways with zstd 19:
//! - rows: `tool \0 args \0 text` per call, concatenated (the store today);
//! - columns: every JSON string value and number of args and text becomes a
//!   variable; the remaining text (keys, punctuation) is the template. A frame
//!   holds template ids, new templates, and one value stream per (template,
//!   variable position), so similar values sit together.
//! Both lossless by construction (template + values rebuild the bytes; checked).
//! With `dict`, each layout gets a dictionary trained on its own first 10% of
//! frames (training frames are excluded from the totals).
//!
//! Usage: toolcall_template <events.tbe> [frame_kib=64] [dict=0|1] [streams|single]
//! (`single`: one value stream in record order instead of one per position)
use oojuro_tracebench::events::{Base, TOOL_CALL, Tier};
use std::collections::HashMap;

/// Split a JSON (or any) text into (template, variables): string values and
/// numbers become \x01; object keys stay. Non-JSON text is one variable.
fn split(s: &str) -> (String, Vec<&str>) {
    let b = s.as_bytes();
    let t = s.trim_start();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return ("\x01".into(), vec![s]);
    }
    let (mut tmpl, mut vars) = (String::with_capacity(64), Vec::new());
    let mut i = 0;
    // stack of containers: true = object (strings alternate key/value)
    let mut stack: Vec<bool> = Vec::new();
    let mut expect_key = false;
    while i < b.len() {
        let c = b[i];
        match c {
            b'{' => {
                stack.push(true);
                expect_key = true;
                tmpl.push('{');
                i += 1;
            }
            b'[' => {
                stack.push(false);
                expect_key = false;
                tmpl.push('[');
                i += 1;
            }
            b'}' | b']' => {
                stack.pop();
                tmpl.push(c as char);
                i += 1;
            }
            b',' => {
                expect_key = stack.last() == Some(&true);
                tmpl.push(',');
                i += 1;
            }
            b':' => {
                expect_key = false;
                tmpl.push(':');
                i += 1;
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(b.len());
                let lit = &s[start..i];
                if expect_key {
                    tmpl.push_str(lit);
                } else {
                    tmpl.push('\x01');
                    vars.push(lit);
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < b.len() && matches!(b[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    i += 1;
                }
                tmpl.push('\x01');
                vars.push(&s[start..i]);
            }
            _ => {
                // whitespace, literals (true/false/null) stay in the template;
                // any other byte (malformed JSON) is kept in order too
                let ch = s[i..].chars().next().unwrap();
                tmpl.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    (tmpl, vars)
}

fn rebuild(tmpl: &str, vars: &[&str]) -> String {
    let mut out = String::new();
    let mut k = 0;
    for ch in tmpl.chars() {
        if ch == '\x01' {
            out.push_str(vars[k]);
            k += 1;
        } else {
            out.push(ch);
        }
    }
    out
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Call {
    tool: String,
    args: String,
    text: String,
}

fn rows_frame(calls: &[Call]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in calls {
        out.extend_from_slice(c.tool.as_bytes());
        out.push(0);
        out.extend_from_slice(c.args.as_bytes());
        out.push(0);
        out.extend_from_slice(c.text.as_bytes());
        out.push(0);
    }
    out
}

/// Variant: template ids + new templates + ONE value stream in record order.
fn single_frame(calls: &[Call]) -> Vec<u8> {
    let mut tmpl_ids: HashMap<String, u32> = HashMap::new();
    let (mut new_tmpls, mut ids, mut vals) = (Vec::new(), Vec::new(), Vec::new());
    for c in calls {
        let (ta, va) = split(&c.args);
        let (tt, vt) = split(&c.text);
        let key = format!("{}\0{}\0{}", c.tool, ta, tt);
        let n = tmpl_ids.len() as u32;
        let id = *tmpl_ids.entry(key.clone()).or_insert_with(|| {
            new_tmpls.extend_from_slice(key.as_bytes());
            new_tmpls.push(0xff);
            n
        });
        varint(&mut ids, id as u64);
        for v in va.iter().chain(vt.iter()) {
            vals.extend_from_slice(v.as_bytes());
            vals.push(0);
        }
    }
    let mut out = Vec::new();
    varint(&mut out, ids.len() as u64);
    out.extend_from_slice(&ids);
    varint(&mut out, new_tmpls.len() as u64);
    out.extend_from_slice(&new_tmpls);
    out.extend_from_slice(&vals);
    out
}

fn cols_frame(calls: &[Call]) -> Vec<u8> {
    let mut tmpl_ids: HashMap<String, u32> = HashMap::new();
    let mut new_tmpls = Vec::new();
    let mut ids = Vec::new();
    let mut streams: Vec<Vec<u8>> = Vec::new();
    let mut slot_base: Vec<u32> = Vec::new();
    for c in calls {
        let (ta, va) = split(&c.args);
        let (tt, vt) = split(&c.text);
        let key = format!("{}\0{}\0{}", c.tool, ta, tt);
        let n = va.len() + vt.len();
        assert_eq!(rebuild(&ta, &va), c.args);
        assert_eq!(rebuild(&tt, &vt), c.text);
        let id = match tmpl_ids.get(&key) {
            Some(&i) => i,
            None => {
                let i = tmpl_ids.len() as u32;
                tmpl_ids.insert(key.clone(), i);
                new_tmpls.extend_from_slice(key.as_bytes());
                new_tmpls.push(0xff);
                slot_base.push(streams.len() as u32);
                streams.extend((0..n).map(|_| Vec::new()));
                i
            }
        };
        varint(&mut ids, id as u64);
        let base = slot_base[id as usize] as usize;
        for (k, v) in va.iter().chain(vt.iter()).enumerate() {
            streams[base + k].extend_from_slice(v.as_bytes());
            streams[base + k].push(0);
        }
    }
    let mut out = Vec::new();
    varint(&mut out, ids.len() as u64);
    out.extend_from_slice(&ids);
    varint(&mut out, new_tmpls.len() as u64);
    out.extend_from_slice(&new_tmpls);
    for s in &streams {
        varint(&mut out, s.len() as u64);
    }
    for s in &streams {
        out.extend_from_slice(s);
    }
    out
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let base = Base::open(a[0].as_ref()).unwrap();
    let frame = a.get(1).map_or(64, |x| x.parse::<usize>().unwrap()) << 10;
    let dict = a.get(2).is_some_and(|x| x == "1");
    let single = a.get(3).is_some_and(|x| x == "single");
    let tier = Tier::new(&base, 1);
    let mut per_agent: HashMap<u32, Vec<Call>> = HashMap::new();
    let mut b = Vec::new();
    for i in 0..tier.plan.len() {
        tier.batch(i, &mut b);
        for e in &b {
            if e.kind == TOOL_CALL && !e.is_revision() {
                per_agent.entry(e.agent).or_default().push(Call { tool: e.tool.into(), args: e.args.into(), text: e.text.into() });
            }
        }
    }
    let (mut raw, mut rows_z, mut cols_z, mut rows_whole, mut cols_whole) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut agents: Vec<_> = per_agent.into_iter().collect();
    agents.sort_by_key(|x| x.0);
    for (_, calls) in &agents {
        let mut frames: Vec<&[Call]> = Vec::new();
        let (mut start, mut acc) = (0, 0usize);
        for (k, c) in calls.iter().enumerate() {
            acc += c.tool.len() + c.args.len() + c.text.len();
            if acc >= frame {
                frames.push(&calls[start..=k]);
                start = k + 1;
                acc = 0;
            }
        }
        if start < calls.len() {
            frames.push(&calls[start..]);
        }
        let train = if dict { (frames.len() / 10).max(1).min(frames.len().saturating_sub(1)) } else { 0 };
        let rf: Vec<Vec<u8>> = frames.iter().map(|f| rows_frame(f)).collect();
        let cf: Vec<Vec<u8>> = frames.iter().map(|f| if single { single_frame(f) } else { cols_frame(f) }).collect();
        let mk = |fs: &[Vec<u8>]| -> Option<Vec<u8>> {
            if train == 0 {
                return None;
            }
            let sizes: Vec<usize> = fs[..train].iter().map(Vec::len).collect();
            let cat: Vec<u8> = fs[..train].concat();
            let size = (cat.len() / 64).clamp(16 << 10, 112 << 10);
            zstd::dict::from_continuous(&cat, &sizes, size).ok()
        };
        let (rd, cd) = (mk(&rf), mk(&cf));
        let z = |x: &[u8], d: &Option<Vec<u8>>| -> u64 {
            let mut c = match d {
                Some(d) => zstd::bulk::Compressor::with_dictionary(19, d).unwrap(),
                None => zstd::bulk::Compressor::new(19).unwrap(),
            };
            c.compress(x).unwrap().len() as u64
        };
        for k in train..frames.len() {
            raw += frames[k].iter().map(|c| (c.tool.len() + c.args.len() + c.text.len()) as u64).sum::<u64>();
            rows_z += z(&rf[k], &rd) + rd.as_ref().map_or(0, |d| d.len() as u64) / (frames.len() - train) as u64;
            cols_z += z(&cf[k], &cd) + cd.as_ref().map_or(0, |d| d.len() as u64) / (frames.len() - train) as u64;
        }
        rows_whole += zstd::bulk::compress(&rf[train..].concat(), 19).unwrap().len() as u64;
        cols_whole += zstd::bulk::compress(&(if single { single_frame } else { cols_frame })(&calls.iter().skip(frames[..train].iter().map(|f| f.len()).sum()).map(|c| Call { tool: c.tool.clone(), args: c.args.clone(), text: c.text.clone() }).collect::<Vec<_>>()), 19).unwrap().len() as u64;
    }
    println!(
        "{{\"frame_kib\": {}, \"dict\": {dict}, \"tool_call_raw\": {raw}, \"rows\": {:.4}, \"columns\": {:.4}, \"gain\": {:.3}, \"whole_rows\": {:.4}, \"whole_columns\": {:.4}}}",
        frame >> 10,
        rows_z as f64 / raw as f64,
        cols_z as f64 / raw as f64,
        1.0 - cols_z as f64 / rows_z as f64,
        rows_whole as f64 / raw as f64,
        cols_whole as f64 / raw as f64
    );
}
