//! Synthetic files only. SIGKILL exercises process restart, not host/AZ/power-loss durability.
use super::*;
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::{fs::DirBuilderExt, process::ExitStatusExt},
    process::Command,
};
const SOURCE: Source = [42; 16];
const NEXT_SOURCE: Source = [43; 16];
const KEY: [u8; 32] = [77; 32];
const WRITER: &str = "codex:-synthetic-local";
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    ram: PathBuf,
    file: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let base = std::env::current_dir().unwrap().join("target");
        fs::create_dir_all(&base).unwrap();
        let root = base.join(format!("local-capture-test-{}-{n}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let ram = PathBuf::from("/dev/shm")
            .join(format!("local-capture-test-{}-{n}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&ram).unwrap();
        let file = root.join(".codex/sessions/rollout-synthetic.jsonl");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        Self { root, ram, file }
    }
    fn options(&self) -> Options {
        let mut o = Options::new(WRITER, 1, "synthetic-policy-v1");
        o.recorded_at = || 123456;
        o
    }
    fn open(&self, policy: Mode) -> LocalCapture {
        LocalCapture::open(
            &self.root.join("authority"),
            &KEY,
            &self.ram,
            self.options(),
            Box::new(policy),
        )
        .unwrap()
    }
    fn append(&self, bytes: &[u8]) {
        OpenOptions::new()
            .append(true)
            .open(&self.file)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(&self.ram);
    }
}
#[derive(Clone, Copy)]
enum Mode {
    Allow,
    Strict,
    WithholdAll,
}
impl Policy for Mode {
    fn decide(&mut self, rec: &Rec, _: &str) -> Decision {
        if matches!(self, Self::WithholdAll) {
            return Decision::Withhold("synthetic-all");
        }
        if matches!(self, Self::Strict) {
            if rec.kind == kind::THINKING {
                return Decision::Withhold("thinking-off");
            }
            if rec.tool == "blocked_tool" {
                return Decision::Withhold("excluded-tool");
            }
        }
        Decision::Store {
            indexed: kind::indexed(rec.kind)
                && !(matches!(self, Self::Strict) && rec.text.contains("STOREONLYMARKER")),
        }
    }
}
fn line(value: Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(&value).unwrap();
    b.push(b'\n');
    b
}
fn meta() -> Vec<u8> {
    line(
        json!({"timestamp":"2026-01-02T03:04:05Z","type":"session_meta","payload":{"cwd":"/synthetic/local","originator":"codex_cli_rs","base_instructions":{"text":""}}}),
    )
}
fn item(payload: Value) -> Vec<u8> {
    line(json!({"timestamp":"2026-01-02T03:04:06Z","type":"response_item","payload":payload}))
}
fn message(text: &str) -> Vec<u8> {
    item(
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}),
    )
}
fn reasoning(text: &str) -> Vec<u8> {
    item(json!({"type":"reasoning","summary":[{"type":"summary_text","text":text}]}))
}
fn call(name: &str, id: &str, body: &str) -> Vec<u8> {
    item(json!({"type":"function_call","name":name,"call_id":id,"arguments":body}))
}
fn result(id: &str, text: &str) -> Vec<u8> {
    item(json!({"type":"function_call_output","call_id":id,"output":text}))
}
fn signature(capture: &LocalCapture, query: &str, k: usize) -> Vec<(u64, u64, bool, i64, u64)> {
    capture
        .search(query, 0, i64::MAX, k)
        .unwrap()
        .into_iter()
        .map(|h| {
            (
                h.hit.id,
                h.hit.bits.to_bits(),
                h.hit.continued,
                h.hit.time,
                h.hit.row,
            )
        })
        .collect()
}

#[test]
fn consent_precedes_source_inspection_and_cache_requires_explicit_tmpfs() {
    let f = Fixture::new();
    let mut c = f.open(Mode::Allow);
    assert_eq!(
        c.capture_file(SOURCE, &f.root.join("does-not-exist"), |_| false)
            .unwrap(),
        None
    );
    assert_eq!(c.log.sources().count(), 0);
    let result = LocalCapture::open(
        &f.root.join("forbidden"),
        &KEY,
        Path::new("/proc"),
        f.options(),
        Box::new(Mode::Allow),
    );
    assert!(matches!(result, Err(Error::Invalid(_))));
    assert!(!f.root.join("forbidden").exists());
}

#[test]
fn withheld_thinking_tool_results_and_unindexed_outputs_stay_authorized_on_relaxed_rebuild() {
    let f = Fixture::new();
    let initial = [
        meta(),
        reasoning("RAWONLYTHOUGHTMARKER"),
        call("blocked_tool", "c-denied", "BLOCKEDCALLMARKER"),
        message("STOREONLYMARKER battery"),
        message("VISIBLEMARKER battery"),
    ]
    .concat();
    fs::write(&f.file, &initial).unwrap();
    let mut options = f.options();
    options.chunk_bytes = 97;
    let mut c = LocalCapture::open(
        &f.root.join("authority"),
        &KEY,
        &f.ram,
        options.clone(),
        Box::new(Mode::Strict),
    )
    .unwrap();
    let report = c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap();
    assert_eq!(report.event_ids, vec![1, 2]);
    assert!(
        c.search("RAWONLYTHOUGHTMARKER", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        c.search("STOREONLYMARKER", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        c.get(1)
            .unwrap()
            .unwrap()
            .0
            .text
            .contains("STOREONLYMARKER")
    );
    let cache = c.projection.as_ref().unwrap().path.clone();
    drop(c);
    assert!(!cache.exists());
    f.append(
        &[
            result("c-denied", "BLOCKEDRESULTMARKER"),
            reasoning("NEWTHOUGHTMARKER battery"),
        ]
        .concat(),
    );
    options.policy_id = "relaxed-policy-v2".into();
    let mut c = LocalCapture::open(
        &f.root.join("authority"),
        &KEY,
        &f.ram,
        options,
        Box::new(Mode::Allow),
    )
    .unwrap();
    let report = c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap();
    assert_eq!(report.event_ids, vec![1, 2, 3]);
    c.rebuild().unwrap();
    for query in [
        "RAWONLYTHOUGHTMARKER",
        "BLOCKEDCALLMARKER",
        "BLOCKEDRESULTMARKER",
        "STOREONLYMARKER",
    ] {
        assert!(
            c.search(query, 0, i64::MAX, 10).unwrap().is_empty(),
            "{query}"
        );
    }
    assert_eq!(
        c.search("NEWTHOUGHTMARKER", 0, i64::MAX, 10).unwrap()[0]
            .hit
            .id,
        3
    );
    let mut raw = Vec::new();
    c.restore_raw(SOURCE, &mut raw, true).unwrap();
    assert!(
        raw.windows(b"RAWONLYTHOUGHTMARKER".len())
            .any(|b| b == b"RAWONLYTHOUGHTMARKER")
    );
    let old = c.projection.as_ref().unwrap().path.clone();
    c.projection = None;
    assert!(!old.exists());
    c.rebuild().unwrap();
    assert_eq!(c.report(SOURCE).unwrap(), report);
    assert!(
        c.search("STOREONLYMARKER", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        c.search("NEWTHOUGHTMARKER", 0, i64::MAX, 10).unwrap()[0]
            .hit
            .id,
        3
    );
}

#[test]
fn all_withheld_malformed_and_blank_lines_advance_without_allocating_ids() {
    let f = Fixture::new();
    let bytes = [
        meta(),
        message("WITHHELDORIGINAL"),
        b"\nnot json\n\xff\0\n".to_vec(),
    ]
    .concat();
    fs::write(&f.file, &bytes).unwrap();
    let mut c = f.open(Mode::WithholdAll);
    let r = c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap();
    assert_eq!(r.raw_end, bytes.len() as u64);
    assert_eq!(r.parsed_end, r.raw_end);
    assert!(r.event_ids.is_empty());
    drop(c);
    let mut c = f.open(Mode::Allow);
    assert_eq!(
        c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap(),
        r
    );
    assert!(
        c.search("WITHHELDORIGINAL", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    f.append(&message("NEWALLOWED battery"));
    let report = c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap();
    assert_eq!(report.event_ids, vec![1]);
}

#[test]
fn projection_failure_reports_committed_ids_blocks_reads_and_retry_reuses_receipt() {
    let f = Fixture::new();
    fs::write(
        &f.file,
        [meta(), message("COMMITTEDMARKER battery")].concat(),
    )
    .unwrap();
    let mut c = f.open(Mode::Allow);
    c.fail_projection = true;
    let outcome = match c.capture_file(SOURCE, &f.file, |_| true) {
        Err(Error::CommittedUnavailable(o)) => o,
        _ => panic!("expected committed-unavailable"),
    };
    assert_eq!(outcome.first_id, Some(1));
    assert_eq!(outcome.stored, 1);
    assert!(matches!(
        c.search("battery", 0, i64::MAX, 10),
        Err(Error::Unavailable)
    ));
    assert_eq!(c.get(1).unwrap_err(), Error::Unavailable);
    assert!(matches!(
        c.capture_file(SOURCE, &f.file, |_| panic!(
            "must refuse before source read"
        )),
        Err(Error::Unavailable)
    ));
    c.fail_projection = false;
    c.rebuild().unwrap();
    let report = c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap();
    assert_eq!(report.event_ids, vec![1]);
    assert_eq!(signature(&c, "battery", 10)[0].0, 1);
    drop(c);
    let c = f.open(Mode::Allow);
    assert_eq!(c.report(SOURCE).unwrap(), report);
}

#[test]
fn partial_non_utf8_prefix_mutation_rotation_and_tool_links_across_blocks() {
    let f = Fixture::new();
    let mut partial = message("partial battery 🚀");
    partial.pop();
    let bytes = [
        b"\xff\0\n\n".to_vec(),
        meta(),
        call("shell", "c1", "battery command"),
        partial,
    ]
    .concat();
    fs::write(&f.file, &bytes).unwrap();
    let mut options = f.options();
    options.chunk_bytes = 31;
    let mut c = LocalCapture::open(
        &f.root.join("authority"),
        &KEY,
        &f.ram,
        options.clone(),
        Box::new(Mode::Allow),
    )
    .unwrap();
    let before = c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap();
    assert_eq!(before.raw_end, bytes.len() as u64);
    assert!(before.parsed_end < before.raw_end);
    assert_eq!(before.event_ids, vec![1]);
    drop(c);
    f.append(&[b"\n".to_vec(), result("c1", "battery result")].concat());
    let expected = fs::read(&f.file).unwrap();
    let mut c = LocalCapture::open(
        &f.root.join("authority"),
        &KEY,
        &f.ram,
        options,
        Box::new(Mode::Allow),
    )
    .unwrap();
    let report = c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap();
    assert_eq!(report.event_ids, vec![1, 2, 3]);
    assert_eq!(c.get(3).unwrap().unwrap().0.parent, 1);
    let mut exact = Vec::new();
    c.restore_raw(SOURCE, &mut exact, true).unwrap();
    assert_eq!(exact, expected);
    let mut changed = expected.clone();
    changed[0] = b'x';
    fs::write(&f.file, changed).unwrap();
    assert!(c.capture_file(SOURCE, &f.file, |_| true).is_err());
    assert_eq!(c.report(SOURCE).unwrap(), report);
    fs::write(&f.file, b"short").unwrap();
    assert!(c.capture_file(SOURCE, &f.file, |_| true).is_err());
    fs::rename(&f.file, f.root.join("retired-source")).unwrap();
    fs::write(
        &f.file,
        [meta(), message("new generation battery")].concat(),
    )
    .unwrap();
    assert!(c.capture_file(SOURCE, &f.file, |_| true).is_err());
    let next = c
        .capture_file(NEXT_SOURCE, &f.file, |_| true)
        .unwrap()
        .unwrap();
    assert_eq!(next.event_ids, vec![4]);
    let (ev, provenance) = c.get(4).unwrap().unwrap();
    assert_eq!(provenance.source, NEXT_SOURCE);
    assert_eq!(sink::parse_src(&ev.args).unwrap().0, 2);
}

#[test]
fn oversized_line_fails_without_claiming_dropped_suffix_or_advancing_parsed_cursor() {
    let f = Fixture::new();
    fs::write(&f.file, vec![b'x'; MAX_LINE_BYTES + 1]).unwrap();
    let mut c = f.open(Mode::Allow);
    assert!(matches!(
        c.capture_file(SOURCE, &f.file, |_| true),
        Err(Error::Invalid(_))
    ));
    let p = c.log.progress(SOURCE).unwrap();
    assert_eq!(p.parsed_end, 0);
    assert_eq!(p.raw_end, MAX_LINE_BYTES as u64);
    assert_eq!(
        fs::metadata(&f.file).unwrap().len(),
        MAX_LINE_BYTES as u64 + 1
    );
}

#[test]
fn oracle_ids_bits_decimal_ties_identifier_continuations_and_deleted_cache_rebuild_match() {
    let f = Fixture::new();
    let data = [
        meta(),
        message("battery electrode zebra"),
        message("zebra electrode battery"),
        message("lookup_needle anchor battery"),
        message("lookup needle fallback battery"),
        call("shell", "c1", "battery electrode test"),
        result("c1", "battery completed"),
    ]
    .concat();
    fs::write(&f.file, &data).unwrap();
    let mut options = f.options();
    options.first_id = 9;
    options.chunk_bytes = 79;
    let mut c = LocalCapture::open(
        &f.root.join("authority"),
        &KEY,
        &f.ram,
        options.clone(),
        Box::new(Mode::Allow),
    )
    .unwrap();
    let r = c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap();
    assert_eq!(r.event_ids, (9..15).collect::<Vec<_>>());
    let oracle_opts = crate::ingest::Options {
        batch_bytes: 79,
        ..crate::ingest::Options::default()
    };
    let mut oracle = crate::ingest::Capture::open(&f.ram.join("oracle"), oracle_opts).unwrap();
    oracle.state.next_id = 9;
    oracle.save().unwrap();
    oracle.set_policy(Box::new(Mode::Allow));
    oracle.poll(&[f.file.parent().unwrap().to_owned()]).unwrap();
    let agent = oracle.state.writers[WRITER];
    let compare = |c: &LocalCapture| {
        for query in [
            "battery",
            "battery electrode",
            "lookup_needle battery",
            "absent_needle battery",
            "\"battery electrode\"",
            "zebra",
        ] {
            for k in [1, 10] {
                let expected: Vec<_> = oracle
                    .db
                    .search_where(agent, query, 0, i64::MAX, k, &mut |_| true)
                    .unwrap()
                    .into_iter()
                    .map(|h| (h.id, h.bits.to_bits(), h.continued, h.time, h.row))
                    .collect();
                assert_eq!(signature(c, query, k), expected, "{query}, k={k}");
            }
        }
        for id in 9..15 {
            let original = oracle.db.get(id).unwrap().unwrap();
            let (native, provenance) = c.get(id).unwrap().unwrap();
            assert_eq!(
                (
                    native.id,
                    native.session,
                    native.seq,
                    native.turn,
                    native.kind,
                    native.parent,
                    native.ts_us,
                    &native.text,
                    &native.tool
                ),
                (
                    original.id,
                    original.session,
                    original.seq,
                    original.turn,
                    original.kind,
                    original.parent,
                    original.ts_us,
                    &original.text,
                    &original.tool
                )
            );
            assert_eq!(
                sink::parse_src(&native.args),
                sink::parse_src(&original.args)
            );
            assert_eq!(
                provenance.line_start,
                sink::parse_src(&native.args).unwrap().1
            );
        }
    };
    compare(&c);
    let ties = signature(&c, "zebra", 10);
    assert_eq!(ties.iter().map(|h| h.0).collect::<Vec<_>>(), vec![10, 9]);
    let continued = signature(&c, "lookup_needle battery", 10);
    assert!(continued.iter().any(|h| h.2));
    let cache = c.projection.as_ref().unwrap().path.clone();
    c.projection = None;
    assert!(!cache.exists());
    c.rebuild().unwrap();
    compare(&c);
    c.log.compact().unwrap();
    drop(c);
    let c = LocalCapture::open(
        &f.root.join("authority"),
        &KEY,
        &f.ram,
        options,
        Box::new(Mode::WithholdAll),
    )
    .unwrap();
    compare(&c);
    let mut restored = Vec::new();
    c.restore_raw(SOURCE, &mut restored, true).unwrap();
    assert_eq!(restored, data);
}

#[test]
fn crash_worker() {
    let Some(root) = std::env::var_os("OOJURO_LOCAL_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let ram = PathBuf::from(std::env::var_os("OOJURO_LOCAL_TEST_RAM").unwrap());
    let mut c = LocalCapture::open(
        &root.join("authority"),
        &KEY,
        &ram,
        Options::new(WRITER, 1, "synthetic"),
        Box::new(Mode::Allow),
    )
    .unwrap();
    c.capture_file(
        SOURCE,
        &root.join(".codex/sessions/rollout-synthetic.jsonl"),
        |_| true,
    )
    .unwrap();
    panic!("crash hook did not fire");
}
#[test]
fn sigkill_after_authority_or_projection_before_ack_recovers_exact_original_ids() {
    for phase in ["after-authority", "after-projection"] {
        let f = Fixture::new();
        let data = [meta(), message("SIGKILLMARKER battery")].concat();
        fs::write(&f.file, &data).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("local::tests::crash_worker")
            .arg("--exact")
            .env("OOJURO_LOCAL_TEST_ROOT", &f.root)
            .env("OOJURO_LOCAL_TEST_RAM", &f.ram)
            .env("OOJURO_LOCAL_TEST_CRASH", phase)
            .output()
            .unwrap();
        assert_eq!(output.status.signal(), Some(libc::SIGKILL));
        let mut c = f.open(Mode::WithholdAll);
        let report = c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap();
        assert_eq!(report.event_ids, vec![1]);
        assert_eq!(c.get(1).unwrap().unwrap().0.text, "SIGKILLMARKER battery");
        let mut raw = Vec::new();
        c.restore_raw(SOURCE, &mut raw, true).unwrap();
        assert_eq!(raw, data);
    }
}

#[test]
fn escaped_tool_argument_and_metadata_secrets_are_masked_before_projection_and_after_rebuild() {
    let f = Fixture::new();
    // Synthetic format-only credential, assembled at runtime; never a real account key.
    let token = ["ghp_", "aB7cD9eF2gH4jK6mN8pQ3rS5tU7vW9xY1zA3"].concat();
    let body = format!(r#"{{"command":"echo ok\n{token}","keep":"unicode 🚀"}}"#);
    let data = [meta(), item(json!({"type":"function_call","name":token,"namespace":token,"call_id":"synthetic-secret-call","arguments":body}))].concat();
    fs::write(&f.file, &data).unwrap();
    let mut c = f.open(Mode::Allow);
    c.capture_file(SOURCE, &f.file, |_| true).unwrap();
    for _ in 0..2 {
        let event = c.get(1).unwrap().unwrap().0;
        assert!(!event.text.contains(&token));
        assert!(!event.tool.contains(&token));
        assert!(!event.args.contains(&token));
        assert!(event.text.contains(r"\n"));
        assert!(event.text.contains("🚀"));
        let _: Value = serde_json::from_str(&event.text).unwrap();
        let _: Value = serde_json::from_str(&event.args).unwrap();
        c.rebuild().unwrap();
    }
    let mut raw = Vec::new();
    c.restore_raw(SOURCE, &mut raw, true).unwrap();
    assert_eq!(raw, data);
    fn no_token(path: &Path, token: &[u8]) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                no_token(&entry.path(), token);
            } else {
                assert!(
                    !fs::read(entry.path())
                        .unwrap()
                        .windows(token.len())
                        .any(|v| v == token)
                );
            }
        }
    }
    no_token(&f.root.join("authority"), token.as_bytes());
    no_token(&c.projection.as_ref().unwrap().path, token.as_bytes());
}

#[test]
fn oversized_sealed_context_refuses_entire_candidate_batch_without_cursor_or_id_advance() {
    let f = Fixture::new();
    let bytes = [
        meta(),
        call(
            &"N".repeat(log::MAX_CONTEXT_BYTES),
            "huge-tool",
            "small synthetic args",
        ),
    ]
    .concat();
    fs::write(&f.file, &bytes).unwrap();
    let mut c = f.open(Mode::Allow);
    assert!(matches!(
        c.capture_file(SOURCE, &f.file, |_| true),
        Err(Error::Invalid(_))
    ));
    assert_eq!(c.log.progress(SOURCE).unwrap().raw_end, 0);
    assert_eq!(c.log.progress(SOURCE).unwrap().parsed_end, 0);
    assert_eq!(c.log.next_id(), 1);
    assert_eq!(fs::read(&f.file).unwrap(), bytes);
}

#[test]
fn metadata_output_bound_refuses_progress_and_smaller_chunk_retry_loses_no_blank_bytes() {
    let f = Fixture::new();
    let bytes = vec![b'\n'; MAX_BLOCK_BYTES];
    fs::write(&f.file, &bytes).unwrap();
    let mut c = f.open(Mode::Allow);
    assert!(matches!(
        c.capture_file(SOURCE, &f.file, |_| true),
        Err(Error::Invalid(_))
    ));
    assert_eq!(c.log.progress(SOURCE).unwrap().raw_end, 0);
    assert_eq!(c.log.next_id(), 1);
    drop(c);
    let mut options = f.options();
    options.chunk_bytes = 4096;
    let mut c = LocalCapture::open(
        &f.root.join("authority"),
        &KEY,
        &f.ram,
        options,
        Box::new(Mode::Allow),
    )
    .unwrap();
    let report = c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap();
    assert_eq!(report.raw_end, bytes.len() as u64);
    assert_eq!(report.parsed_end, report.raw_end);
    assert!(report.event_ids.is_empty());
    let mut raw = Vec::new();
    c.restore_raw(SOURCE, &mut raw, true).unwrap();
    assert_eq!(raw, bytes);
}

#[test]
fn incomplete_raw_line_cannot_be_reinterpreted_by_relaxing_its_policy_identity() {
    let f = Fixture::new();
    let mut tail = reasoning("PENDINGTHOUGHTMARKER battery");
    tail.pop();
    fs::write(&f.file, [meta(), tail].concat()).unwrap();
    let mut original = f.options();
    original.policy_id = "strict-snapshot-v1".into();
    let mut c = LocalCapture::open(
        &f.root.join("authority"),
        &KEY,
        &f.ram,
        original.clone(),
        Box::new(Mode::Strict),
    )
    .unwrap();
    let before = c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap();
    assert!(before.raw_end > before.parsed_end);
    drop(c);
    f.append(b"\n");
    let mut relaxed = f.options();
    relaxed.policy_id = "relaxed-snapshot-v2".into();
    let mut c = LocalCapture::open(
        &f.root.join("authority"),
        &KEY,
        &f.ram,
        relaxed.clone(),
        Box::new(Mode::Allow),
    )
    .unwrap();
    assert!(matches!(
        c.capture_file(SOURCE, &f.file, |_| true),
        Err(Error::Invalid(_))
    ));
    assert_eq!(c.report(SOURCE).unwrap(), before);
    drop(c);
    let mut c = LocalCapture::open(
        &f.root.join("authority"),
        &KEY,
        &f.ram,
        original,
        Box::new(Mode::Strict),
    )
    .unwrap();
    let done = c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap();
    assert!(done.event_ids.is_empty());
    assert_eq!(done.raw_end, done.parsed_end);
    drop(c);
    let c = LocalCapture::open(
        &f.root.join("authority"),
        &KEY,
        &f.ram,
        relaxed,
        Box::new(Mode::Allow),
    )
    .unwrap();
    assert!(
        c.search("PENDINGTHOUGHTMARKER", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
}
