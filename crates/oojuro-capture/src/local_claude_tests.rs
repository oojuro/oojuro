//! Synthetic explicitly selected Claude transcripts; no discovery or private logs.
use super::*;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::process::ExitStatusExt,
    process::Command,
};

const KEY: [u8; 32] = [93; 32];
const SOURCE: Source = [52; 16];
const WRITER: &str = "claude-code:-synthetic";
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    file: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "native-claude-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let file = root.join(".claude/projects/-synthetic/session.jsonl");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        Self { root, file }
    }
    fn options(&self) -> Options {
        let mut options = Options::new(WRITER, 3, "synthetic-v1");
        options.recorded_at = || 123456;
        options
    }
    fn open(&self, strict: bool) -> LocalCapture {
        let mut options = self.options();
        if strict {
            options.policy_id = "synthetic-strict-v1".into();
        }
        LocalCapture::open_claude(
            &self.root.join("authority"),
            &KEY,
            options,
            Box::new(Mode(strict)),
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
    }
}
struct Mode(bool);
impl Policy for Mode {
    fn decide(&mut self, rec: &Rec, _: &str) -> Decision {
        if self.0 && (rec.kind == kind::THINKING || rec.tool == "blocked_tool") {
            return Decision::Withhold("synthetic-denied");
        }
        Decision::Store {
            indexed: kind::indexed(rec.kind)
                && !rec.from_oojuro()
                && !(self.0 && rec.text.contains("STOREONLY")),
        }
    }
}
fn line(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}
fn assistant(blocks: Value) -> Vec<u8> {
    line(
        json!({"type":"assistant","cwd":"/synthetic","timestamp":"2026-01-02T03:04:05Z","message":{"model":"synthetic-model","content":blocks}}),
    )
}
fn call(id: &str, name: &str) -> Vec<u8> {
    assistant(
        json!([{"type":"tool_use","id":id,"name":name,"input":{"command":"synthetic command"}}]),
    )
}
fn result(id: &str, text: &str) -> Vec<u8> {
    line(
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id,"content":text}]}}),
    )
}
fn message(text: &str) -> Vec<u8> {
    line(json!({"type":"user","origin":{"kind":"human"},"message":{"content":text}}))
}
fn raw(capture: &LocalCapture) -> Vec<u8> {
    let mut bytes = Vec::new();
    capture.restore_raw(SOURCE, &mut bytes, true).unwrap();
    bytes
}

#[test]
fn denied_raw_consent_precedes_absent_source_inspection() {
    let f = Fixture::new();
    let mut capture = f.open(false);
    let mut asked = false;
    assert_eq!(
        capture
            .capture_file(SOURCE, &f.root.join("absent"), |_| {
                asked = true;
                false
            })
            .unwrap(),
        None
    );
    assert!(asked);
    assert_eq!(capture.log.sources().count(), 0);
}

#[test]
fn native_records_match_the_legacy_parser_on_complete_authorized_main_file_bytes() {
    let f = Fixture::new();
    let bytes = [message("person request 🚀"), assistant(json!([{"type":"thinking","thinking":"reported thought"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"echo hi","first":1}}])), result("t1", "synthetic result"),
        line(json!({"type":"attachment","attachment":{"type":"date"},"rendered":[{"content":"reported context"}]})),
        line(json!({"type":"system","subtype":"compact_boundary","content":"context replaced"})),
        line(json!({"type":"queue-operation"})), assistant(json!([{"type":"text","text":"ordinary output"}]))].concat();
    fs::write(&f.file, &bytes).unwrap();
    let mut legacy = claude::new_ctx(&f.file);
    let mut expected = Vec::new();
    let mut counts = ParseCounts::default();
    let mut redacted = redact::Counts::new();
    let redactor = redact::Redactor::new();
    let mut offset = 0;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        claude::parse_line(
            &mut legacy,
            &line[..line.len() - 1],
            offset,
            &redactor,
            &mut redacted,
            &mut counts,
            &mut expected,
        );
        offset += line.len() as u64;
    }
    let mut capture = f.open(false);
    let report = capture
        .capture_file(SOURCE, &f.file, |_| true)
        .unwrap()
        .unwrap();
    assert_eq!(report.event_ids.len(), expected.len());
    for (id, rec) in report.event_ids.iter().zip(expected) {
        let (event, provenance) = capture.get(*id).unwrap().unwrap();
        assert_eq!(
            (
                event.kind,
                event.text,
                event.seq,
                event.turn,
                event.ts_us,
                event.session,
                event.parent_session
            ),
            (
                rec.kind,
                rec.text,
                rec.seq,
                rec.turn,
                rec.ts_us,
                rec.session,
                rec.parent_session
            )
        );
        assert_eq!(
            (provenance.line_start, provenance.emission),
            (rec.line, rec.k)
        );
    }
    assert_eq!(capture.get(4).unwrap().unwrap().0.parent, 3);
    assert_eq!(capture.get(4).unwrap().unwrap().0.tool, "Bash");
    assert!(
        !capture
            .search("ordinary output", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(raw(&capture), bytes);
    drop(capture);
    let mut reopened = f.open(true);
    assert_eq!(
        reopened
            .capture_file(SOURCE, &f.file, |_| true)
            .unwrap()
            .unwrap(),
        report
    );
    assert_eq!(raw(&reopened), bytes);
}

#[test]
fn denied_calls_and_store_only_eligibility_survive_policy_relaxation_and_restart() {
    let f = Fixture::new();
    fs::write(
        &f.file,
        [
            assistant(json!([{"type":"thinking","thinking":"RAWTHOUGHT"}])),
            call("denied", "blocked_tool"),
            message("STOREONLY remembered"),
        ]
        .concat(),
    )
    .unwrap();
    let mut capture = f.open(true);
    let before = capture
        .capture_file(SOURCE, &f.file, |_| true)
        .unwrap()
        .unwrap();
    assert_eq!(before.event_ids, vec![1]);
    drop(capture);
    f.append(&result("denied", "DENIEDRESULTMARKER"));
    let mut reopened = f.open(false);
    let report = reopened
        .capture_file(SOURCE, &f.file, |_| true)
        .unwrap()
        .unwrap();
    assert_eq!(report.event_ids, vec![1]);
    assert!(
        reopened
            .search("DENIEDRESULTMARKER RAWTHOUGHT STOREONLY", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        String::from_utf8(raw(&reopened))
            .unwrap()
            .contains("DENIEDRESULTMARKER")
    );
}

#[test]
fn referenced_tool_file_is_never_opened_or_read_and_stub_stays_explicitly_unread() {
    use std::os::unix::ffi::OsStrExt;
    let f = Fixture::new();
    let saved = f
        .file
        .parent()
        .unwrap()
        .join("session/tool-results/output.txt");
    fs::create_dir_all(saved.parent().unwrap()).unwrap();
    fs::write(&saved, "OUTSIDECONSENTMARKER").unwrap();
    let name = std::ffi::CString::new(saved.as_os_str().as_bytes()).unwrap();
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    assert!(fd >= 0);
    assert!(
        unsafe { libc::inotify_add_watch(fd, name.as_ptr(), libc::IN_OPEN | libc::IN_ACCESS) } >= 0
    );
    let stub = format!(
        "<persisted-output>\nOutput too large. Full output saved to: {}\n\nPreview: visible stub\n</persisted-output>",
        saved.display()
    );
    let bytes = result("tool", &stub);
    fs::write(&f.file, &bytes).unwrap();
    let mut capture = f.open(false);
    capture.capture_file(SOURCE, &f.file, |_| true).unwrap();
    let mut buffer = [0u8; 1024];
    let observed = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    unsafe {
        libc::close(fd);
    }
    assert_eq!(observed, -1, "secondary file was opened or read");
    let event = capture.get(1).unwrap().unwrap().0;
    assert_eq!(event.text, stub);
    assert!(event.args.contains("unread"));
    assert!(!event.text.contains("OUTSIDECONSENTMARKER"));
    assert_eq!(raw(&capture), bytes);
}

#[test]
fn lookup_capacity_and_duplicate_ids_refuse_before_ack_and_resume_exactly() {
    let f = Fixture::new();
    let initial = assistant(Value::Array(
        (0..64)
            .map(
                |i| json!({"type":"tool_use","id":format!("lookup-{i}"),"name":"mcp__oojuro__get","input":{}}),
            )
            .collect(),
    ));
    fs::write(&f.file, &initial).unwrap();
    let mut capture = f.open(false);
    let before = capture
        .capture_file(SOURCE, &f.file, |_| true)
        .unwrap()
        .unwrap();
    assert_eq!(before.event_ids.len(), 64);
    drop(capture);
    let suffix = assistant(
        json!([{"type":"tool_use","id":"overflow","name":"mcp__oojuro__get","input":{}}]),
    );
    f.append(&suffix);
    let mut reopened = f.open(false);
    assert_eq!(
        reopened.capture_file(SOURCE, &f.file, |_| true),
        Err(Error::Invalid("outstanding lookup capacity"))
    );
    assert_eq!(reopened.report(SOURCE).unwrap(), before);
    assert_eq!(raw(&reopened), initial);
    assert_eq!(
        fs::read(&f.file).unwrap(),
        [initial.clone(), suffix].concat()
    );
    drop(reopened);
    // Replace only the unacknowledged suffix explicitly; every retained prefix byte is unchanged.
    fs::write(
        &f.file,
        [
            initial.clone(),
            result("lookup-0", "matched completion"),
            call("next", "Bash"),
        ]
        .concat(),
    )
    .unwrap();
    let mut reopened = f.open(false);
    let report = reopened
        .capture_file(SOURCE, &f.file, |_| true)
        .unwrap()
        .unwrap();
    assert_eq!(report.event_ids.len(), 66);
    assert_eq!(&report.event_ids[64..], &[65, 66]);
    f.append(&call("next", "Bash"));
    let before = reopened.report(SOURCE).unwrap();
    assert_eq!(
        reopened.capture_file(SOURCE, &f.file, |_| true),
        Err(Error::Invalid("duplicate unresolved call ID"))
    );
    assert_eq!(reopened.report(SOURCE).unwrap(), before);
}

#[test]
fn many_completed_calls_do_not_bound_total_session_length_and_same_line_reuse_is_safe() {
    let f = Fixture::new();
    let mut bytes = Vec::new();
    for i in 0..90 {
        bytes.extend(call("reused", "Bash"));
        bytes.extend(result("reused", &format!("completed {i}")));
    }
    fs::write(&f.file, &bytes).unwrap();
    let mut capture = f.open(false);
    let report = capture
        .capture_file(SOURCE, &f.file, |_| true)
        .unwrap()
        .unwrap();
    assert_eq!(report.event_ids.len(), 180);
    for id in (2..=180).step_by(2) {
        assert_eq!(capture.get(id).unwrap().unwrap().0.parent, id - 1);
    }
    assert_eq!(raw(&capture), bytes);
}

#[test]
fn a_single_multiblock_line_refuses_capacity_without_cursor_or_allocator_progress() {
    let f = Fixture::new();
    let bytes = assistant(Value::Array(
        (0..65)
            .map(|i| json!({"type":"tool_use","id":format!("c{i}"),"name":"Bash","input":{}}))
            .collect(),
    ));
    fs::write(&f.file, &bytes).unwrap();
    let mut capture = f.open(false);
    assert_eq!(
        capture.capture_file(SOURCE, &f.file, |_| true),
        Err(Error::Invalid("outstanding call capacity"))
    );
    let report = capture.report(SOURCE).unwrap();
    assert_eq!(
        (report.raw_end, report.parsed_end, report.event_ids.len()),
        (0, 0, 0)
    );
    assert_eq!(fs::read(&f.file).unwrap(), bytes);
}

#[test]
fn partial_unicode_malformed_and_non_utf8_lines_roundtrip_and_resume_under_original_policy() {
    let f = Fixture::new();
    let mut bytes = [
        b"\n{bad-json}\n\xff\n".as_slice(),
        message("Unicode 🚀 exact").as_slice(),
    ]
    .concat();
    let complete = message("continued suffix");
    bytes.extend(&complete[..complete.len() - 1]);
    fs::write(&f.file, &bytes).unwrap();
    let mut options = f.options();
    options.chunk_bytes = 7;
    let mut capture = LocalCapture::open_claude(
        &f.root.join("authority"),
        &KEY,
        options.clone(),
        Box::new(Mode(false)),
    )
    .unwrap();
    let before = capture
        .capture_file(SOURCE, &f.file, |_| true)
        .unwrap()
        .unwrap();
    assert_eq!(before.raw_end, bytes.len() as u64);
    assert!(before.parsed_end < before.raw_end);
    assert_eq!(raw(&capture), bytes);
    drop(capture);
    options.policy_id = "changed".into();
    f.append(b"\n");
    let mut wrong = LocalCapture::open_claude(
        &f.root.join("authority"),
        &KEY,
        options,
        Box::new(Mode(false)),
    )
    .unwrap();
    assert!(matches!(
        wrong.capture_file(SOURCE, &f.file, |_| true),
        Err(Error::Invalid(_))
    ));
    assert_eq!(wrong.report(SOURCE).unwrap(), before);
    drop(wrong);
    let mut capture = f.open(false);
    let after = capture
        .capture_file(SOURCE, &f.file, |_| true)
        .unwrap()
        .unwrap();
    assert_eq!(after.event_ids, vec![1, 2]);
    assert_eq!(after.parsed_end, after.raw_end);
    bytes.push(b'\n');
    assert_eq!(raw(&capture), bytes);
}

#[test]
fn cross_harness_opens_are_rejected_in_both_directions() {
    let f = Fixture::new();
    drop(f.open(false));
    assert!(
        LocalCapture::open(
            &f.root.join("authority"),
            &KEY,
            f.options(),
            Box::new(Mode(false))
        )
        .is_err()
    );
    let codex = f.root.join("codex-authority");
    drop(LocalCapture::open(&codex, &KEY, f.options(), Box::new(Mode(false))).unwrap());
    assert!(LocalCapture::open_claude(&codex, &KEY, f.options(), Box::new(Mode(false))).is_err());
}

#[test]
fn crash_worker() {
    let Some(root) = std::env::var_os("OOJURO_CLAUDE_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let mut capture = LocalCapture::open_claude(
        &root.join("authority"),
        &KEY,
        Options::new(WRITER, 3, "synthetic-v1"),
        Box::new(Mode(false)),
    )
    .unwrap();
    capture
        .capture_file(
            SOURCE,
            &root.join(".claude/projects/-synthetic/session.jsonl"),
            |_| true,
        )
        .unwrap();
    panic!("crash hook did not fire");
}

#[test]
fn sigkill_after_authority_and_projection_before_ack_recovers_ids_and_raw() {
    for phase in ["after-authority", "after-projection"] {
        let f = Fixture::new();
        let bytes = [message("CRASHMARKER synthetic"), call("t", "Bash")].concat();
        fs::write(&f.file, &bytes).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("local::claude_tests::crash_worker")
            .arg("--exact")
            .env("OOJURO_CLAUDE_TEST_ROOT", &f.root)
            .env("OOJURO_LOCAL_TEST_CRASH", phase)
            .output()
            .unwrap();
        assert_eq!(output.status.signal(), Some(libc::SIGKILL));
        let mut capture = f.open(true);
        let report = capture
            .capture_file(SOURCE, &f.file, |_| true)
            .unwrap()
            .unwrap();
        assert_eq!(report.event_ids, vec![1, 2]);
        assert_eq!(raw(&capture), bytes);
    }
}

#[test]
fn same_line_completed_call_reuse_preserves_sequential_links() {
    let f = Fixture::new();
    let bytes = assistant(json!([
        {"type":"tool_use","id":"same","name":"Bash","input":{}},
        {"type":"bash_tool_result","tool_use_id":"same","content":"matched"},
        {"type":"tool_use","id":"same","name":"Bash","input":{}}
    ]));
    fs::write(&f.file, &bytes).unwrap();
    let mut capture = f.open(false);
    assert_eq!(
        capture
            .capture_file(SOURCE, &f.file, |_| true)
            .unwrap()
            .unwrap()
            .event_ids,
        vec![1, 2, 3]
    );
    assert_eq!(capture.get(2).unwrap().unwrap().0.parent, 1);
    assert_eq!(
        decode_context(&capture.log.progress(SOURCE).unwrap().context)
            .unwrap()
            .calls[0]
            .2,
        3
    );
}

#[test]
fn subagent_paths_and_origin_are_reported_provenance_not_verified_human_authority() {
    let mut f = Fixture::new();
    f.file = f
        .file
        .parent()
        .unwrap()
        .join("session/subagents/agent-synthetic.jsonl");
    fs::create_dir_all(f.file.parent().unwrap()).unwrap();
    let bytes = [
        line(json!({"type":"user","message":{"content":"parent instruction"}})),
        line(json!({"type":"user","origin":{"kind":"peer"},"message":{"content":"reported peer"}})),
    ]
    .concat();
    fs::write(&f.file, &bytes).unwrap();
    let mut capture = f.open(false);
    capture.capture_file(SOURCE, &f.file, |_| true).unwrap();
    let first = capture.get(1).unwrap().unwrap().0;
    let args: Value = serde_json::from_str(&first.args).unwrap();
    assert_eq!(args["from"], "parent-agent");
    assert_eq!(
        first.parent_session,
        crate::hash64(
            &claude::parent_transcript(&f.file)
                .unwrap()
                .to_string_lossy()
        )
    );
    let second = capture.get(2).unwrap().unwrap().0;
    let args: Value = serde_json::from_str(&second.args).unwrap();
    assert_eq!(args["from"], "peer");
    assert_eq!(first.parent_session, second.parent_session);
    assert_eq!(raw(&capture), bytes);
}

#[test]
fn incomplete_or_unknown_context_fields_refuse_initial_and_historical_receipts() {
    for historical in [false, true] {
        let f = Fixture::new();
        let initial = encode_context(&claude_adapter::new_ctx(&f.file)).unwrap();
        let mut bad: Value = serde_json::from_slice(&initial).unwrap();
        if historical {
            bad["unknown"] = json!(true);
        } else {
            bad.as_object_mut().unwrap().remove("calls");
        }
        let bad = serde_json::to_vec(&bad).unwrap();
        let mut log = CaptureLog::open(
            &f.root.join("authority"),
            &KEY,
            Identity {
                writer: WRITER.into(),
                agent: 3,
                first_id: 1,
                adapter: CLAUDE_ADAPTER.into(),
            },
        )
        .unwrap();
        log.register(
            SOURCE,
            b"synthetic-descriptor",
            if historical { &initial } else { &bad },
        )
        .unwrap();
        if historical {
            log.commit(Batch {
                raw: b" \n".to_vec(),
                parsed: ParsedBatch {
                    source: SOURCE,
                    raw_start: 0,
                    parsed_before: 0,
                    outcome: Outcome {
                        raw_end: 2,
                        parsed_end: 2,
                        first_id: None,
                        stored: 0,
                        next_id: 1,
                    },
                    policy: PolicyReceipt {
                        policy_id: "synthetic-v1".into(),
                        raw_authorization: "synthetic".into(),
                        lines: vec![Line {
                            start: 0,
                            end: 2,
                            kind: LineKind::Blank,
                            emissions: vec![],
                        }],
                    },
                    context: bad,
                    events: vec![],
                },
            })
            .unwrap();
        }
        drop(log);
        assert!(matches!(
            LocalCapture::open_claude(
                &f.root.join("authority"),
                &KEY,
                f.options(),
                Box::new(Mode(false))
            ),
            Err(Error::Corrupt)
        ));
    }
}

#[test]
fn a_missing_input_cannot_hide_an_excluded_tool_from_policy_or_publish_its_result() {
    let f = Fixture::new();
    let bytes = [
        assistant(json!([{"type":"tool_use","id":"blocked","name":"blocked_tool"}])),
        result("blocked", "NOLEAKMARKER"),
    ]
    .concat();
    fs::write(&f.file, &bytes).unwrap();
    let mut capture = f.open(true);
    assert_eq!(
        capture.capture_file(SOURCE, &f.file, |_| true),
        Err(Error::Invalid("tool call input missing"))
    );
    let before = capture.report(SOURCE).unwrap();
    assert_eq!(
        (before.raw_end, before.parsed_end, before.event_ids.len()),
        (0, 0, 0)
    );
    assert_eq!(fs::read(&f.file).unwrap(), bytes);
    drop(capture);
    let mut reopened = f.open(false);
    assert_eq!(
        reopened.capture_file(SOURCE, &f.file, |_| true),
        Err(Error::Invalid("tool call input missing"))
    );
    assert_eq!(reopened.report(SOURCE).unwrap(), before);
    assert!(
        reopened
            .search("NOLEAKMARKER", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    assert!(raw(&reopened).is_empty());
    assert_eq!(fs::read(&f.file).unwrap(), bytes);
}

#[test]
fn nonobject_inputs_still_reach_exclusion_policy_and_bind_results_across_restart() {
    for input in [Value::Null, json!(""), json!([])] {
        let f = Fixture::new();
        let bytes = assistant(
            json!([{"type":"tool_use","id":"blocked","name":"blocked_tool","input":input}]),
        );
        fs::write(&f.file, &bytes).unwrap();
        let mut capture = f.open(true);
        let report = capture
            .capture_file(SOURCE, &f.file, |_| true)
            .unwrap()
            .unwrap();
        assert!(report.event_ids.is_empty());
        drop(capture);
        f.append(&result("blocked", "DENIEDNONOBJECTMARKER"));
        let mut reopened = f.open(false);
        let report = reopened
            .capture_file(SOURCE, &f.file, |_| true)
            .unwrap()
            .unwrap();
        assert!(report.event_ids.is_empty());
        assert!(
            reopened
                .search("DENIEDNONOBJECTMARKER", 0, i64::MAX, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(raw(&reopened), fs::read(&f.file).unwrap());
    }
}

#[test]
fn missing_empty_or_nonstring_call_ids_refuse_without_ack_before_same_line_and_later_results() {
    for id in [None, Some(Value::Null), Some(json!("")), Some(json!(7))] {
        let f = Fixture::new();
        let mut block = json!({"type":"tool_use","name":"blocked_tool","input":{}});
        if let Some(id) = id {
            block["id"] = id;
        }
        let bytes = [assistant(json!([block,{"type":"bash_tool_result","tool_use_id":"","content":"SAMELINENOLEAK"}])),result("","LATERNOLEAK")].concat();
        fs::write(&f.file, &bytes).unwrap();
        let mut capture = f.open(true);
        assert_eq!(
            capture.capture_file(SOURCE, &f.file, |_| true),
            Err(Error::Invalid("tool call ID missing"))
        );
        let before = capture.report(SOURCE).unwrap();
        assert_eq!(
            (before.raw_end, before.parsed_end, before.event_ids.len()),
            (0, 0, 0)
        );
        drop(capture);
        let mut reopened = f.open(false);
        assert_eq!(
            reopened.capture_file(SOURCE, &f.file, |_| true),
            Err(Error::Invalid("tool call ID missing"))
        );
        assert_eq!(reopened.report(SOURCE).unwrap(), before);
        assert!(
            reopened
                .search("SAMELINENOLEAK LATERNOLEAK", 0, i64::MAX, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(fs::read(&f.file).unwrap(), bytes);
    }
}

#[test]
fn same_line_denied_calls_bind_both_assistant_and_later_user_completions() {
    let f = Fixture::new();
    let first = assistant(json!([
        {"type":"tool_use","id":"a","name":"blocked_tool","input":{}},
        {"type":"bash_tool_result","tool_use_id":"a","content":"SAMELINEDENIED"},
        {"type":"tool_use","id":"b","name":"blocked_tool","input":{}}
    ]));
    fs::write(&f.file, &first).unwrap();
    let mut capture = f.open(true);
    assert!(
        capture
            .capture_file(SOURCE, &f.file, |_| true)
            .unwrap()
            .unwrap()
            .event_ids
            .is_empty()
    );
    drop(capture);
    f.append(&result("b", "LATERDENIED"));
    let mut reopened = f.open(false);
    assert!(
        reopened
            .capture_file(SOURCE, &f.file, |_| true)
            .unwrap()
            .unwrap()
            .event_ids
            .is_empty()
    );
    assert!(
        reopened
            .search("SAMELINEDENIED LATERDENIED", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(raw(&reopened), fs::read(&f.file).unwrap());
}
