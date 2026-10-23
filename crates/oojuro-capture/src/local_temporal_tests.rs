use super::*;
use serde_json::{Value, json};
use std::{fs, os::unix::process::ExitStatusExt, process::Command};
static NEXT: AtomicU64 = AtomicU64::new(0);
const KEY: [u8; 32] = [87; 32];
struct Fixture {
    root: PathBuf,
}
fn stream() -> Stream {
    Stream {
        namespace: "synthetic".into(),
        workflow_id: "workflow".into(),
        run_id: "run".into(),
        generation: "generation-1".into(),
    }
}
fn source() -> Source {
    stream().source_id().unwrap()
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "native-temporal-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn open(&self, strict: bool) -> LocalCapture {
        let mut o = Options::new(
            "temporal:synthetic",
            7,
            if strict { "strict-1" } else { "relaxed-1" },
        );
        o.recorded_at = || 12345;
        LocalCapture::open_temporal(
            &self.root.join("authority"),
            &KEY,
            o,
            Box::new(Mode(strict)),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
struct Mode(bool);
impl Policy for Mode {
    fn decide(&mut self, r: &Rec, _: &str) -> Decision {
        if self.0 && (r.kind == kind::THINKING || r.tool == "blocked_tool") {
            Decision::Withhold("synthetic-denied")
        } else {
            Decision::Store {
                indexed: kind::indexed(r.kind) && !r.from_oojuro() && !r.text.contains("STOREONLY"),
            }
        }
    }
}
fn frame(id: &str, sequence: u32, kind: &str, payload: Value) -> Vec<u8> {
    let mut bytes=serde_json::to_vec(&json!({"version":1,"id":id,"sequence":sequence,"valid_us":123456,"kind":kind,"tool":if kind.starts_with("tool_"){Some("blocked_tool")}else{None},
 "operation_id":if kind.starts_with("tool_"){Some("operation")}else{None},"status":if kind=="tool_result"{Some("ok")}else{None},"payload":payload,"attributes":{}})).unwrap();
    bytes.push(b'\n');
    bytes
}
fn cursor(c: &LocalCapture) -> u64 {
    c.temporal_position(source(), &stream(), "new-event")
        .unwrap()
        .cursor
}
fn append(
    c: &mut LocalCapture,
    id: &str,
    seq: u32,
    kind: &str,
    payload: Value,
) -> (Vec<u8>, StreamReceipt) {
    let bytes = frame(id, seq, kind, payload);
    let at = cursor(c);
    let receipt = c
        .append_temporal(source(), &stream(), at, &bytes, true)
        .unwrap();
    (bytes, receipt)
}
fn raw(c: &LocalCapture) -> Vec<u8> {
    let mut bytes = Vec::new();
    c.restore_raw(source(), &mut bytes, true).unwrap();
    bytes
}
#[test]
fn raw_and_parsed_are_atomic_redacted_searchable_and_exact_after_restart() {
    let f = Fixture::new();
    let mut c = f.open(false);
    let token = format!("sk-ant-api03-{}", "Qx7".repeat(35));
    let (a, receipt) = append(&mut c, "a", 1, "input", json!(format!("hello {token}")));
    let (b, _) = append(&mut c, "b", 2, "output", json!("RESULTMARKER output"));
    assert_eq!(receipt.outcome.first_id, Some(1));
    assert!(!c.get(1).unwrap().unwrap().0.text.contains(&token));
    assert!(
        !c.search("RESULTMARKER", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(raw(&c), [a.clone(), b.clone()].concat());
    drop(c);
    let c = f.open(true);
    assert_eq!(
        c.temporal_receipt(source(), &stream(), 0, &a, true)
            .unwrap(),
        Some(receipt)
    );
    assert_eq!(raw(&c), [a, b].concat());
}
#[test]
fn original_retry_precedes_policy_and_never_reallocates_or_accepts_changed_intent() {
    let f = Fixture::new();
    let mut c = f.open(true);
    let (bytes, first) = append(&mut c, "original", 1, "thinking", json!("WITHHELDTHOUGHT"));
    assert_eq!(first.outcome.stored, 0);
    append(&mut c, "later", 2, "output", json!("ordinary output"));
    let at = cursor(&c);
    drop(c);
    let mut c = f.open(false);
    assert_eq!(
        c.append_temporal(source(), &stream(), 0, &bytes, true)
            .unwrap(),
        first
    );
    assert_eq!(cursor(&c), at);
    assert_eq!(c.report(source()).unwrap().event_ids, vec![1]);
    assert!(
        c.append_temporal(source(), &stream(), at, &bytes, true)
            .is_err()
    );
    assert!(
        c.append_temporal(
            source(),
            &stream(),
            0,
            &frame("original", 1, "thinking", json!("changed")),
            true
        )
        .is_err()
    );
    assert_eq!(cursor(&c), at);
    assert!(
        c.search("WITHHELDTHOUGHT", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn retained_call_policy_and_lookup_eligibility_survive_relaxation() {
    let f = Fixture::new();
    let mut c = f.open(true);
    append(
        &mut c,
        "call",
        1,
        "tool_call",
        json!({"command":"synthetic"}),
    );
    drop(c);
    let mut c = f.open(false);
    append(&mut c, "result", 2, "tool_result", json!("WITHHELDRESULT"));
    assert_eq!(c.report(source()).unwrap().event_ids.len(), 0);
    assert!(
        c.search("WITHHELDRESULT", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    let mut call: Value =
        serde_json::from_slice(&frame("lookup", 3, "tool_call", json!({"id":1}))).unwrap();
    call["tool"] = json!("mcp__oojuro__get");
    call["operation_id"] = json!("lookup");
    let mut bytes = serde_json::to_vec(&call).unwrap();
    bytes.push(b'\n');
    c.append_temporal(source(), &stream(), cursor(&c), &bytes, true)
        .unwrap();
    call["id"] = json!("lookup-result");
    call["sequence"] = json!(4);
    call["kind"] = json!("tool_result");
    call["status"] = json!("ok");
    call["payload"] = json!("RECALLEDONLY");
    let mut bytes = serde_json::to_vec(&call).unwrap();
    bytes.push(b'\n');
    c.append_temporal(source(), &stream(), cursor(&c), &bytes, true)
        .unwrap();
    assert_eq!(c.get(2).unwrap().unwrap().0.parent, 1);
    assert!(!c.get(2).unwrap().unwrap().0.indexed);
}
#[test]
fn source_forks_cursor_sequence_cross_adapter_and_consent_refuse_without_progress() {
    let f = Fixture::new();
    let mut c = f.open(false);
    let bytes = frame("a", 1, "input", json!("private"));
    assert!(
        c.append_temporal(source(), &stream(), 0, &bytes, false)
            .is_err()
    );
    assert_eq!(c.log.sources().count(), 0);
    assert!(
        c.append_temporal([0; 16], &stream(), 0, &bytes, true)
            .is_err()
    );
    assert_eq!(c.log.sources().count(), 0);
    append(&mut c, "a", 1, "input", json!("first"));
    let before = cursor(&c);
    assert!(
        c.append_temporal(
            source(),
            &stream(),
            before,
            &frame("b", 3, "input", json!("gap")),
            true
        )
        .is_err()
    );
    let mut fork = stream();
    fork.generation = "changed".into();
    assert!(
        c.append_temporal(
            source(),
            &fork,
            before,
            &frame("b", 2, "input", json!("fork")),
            true
        )
        .is_err()
    );
    assert!(
        c.capture_file([0; 16], &f.root.join("never-open"), |_| true)
            .is_err()
    );
    assert_eq!(cursor(&c), before);
    drop(c);
    assert!(
        LocalCapture::open(
            &f.root.join("authority"),
            &KEY,
            Options::new("temporal:synthetic", 7, "relaxed-1"),
            Box::new(Mode(false))
        )
        .is_err()
    );
}
#[test]
fn committed_projection_failure_recovers_exact_original_receipt() {
    let f = Fixture::new();
    let mut c = f.open(false);
    c.fail_projection = true;
    let bytes = frame("original", 1, "output", json!("original committed"));
    let outcome = match c.append_temporal(source(), &stream(), 0, &bytes, true) {
        Err(Error::CommittedUnavailable(o)) => o,
        _ => panic!("missing committed failure"),
    };
    drop(c);
    let mut c = f.open(true);
    let receipt = c
        .append_temporal(source(), &stream(), 0, &bytes, true)
        .unwrap();
    assert_eq!(receipt.outcome, outcome);
    assert_eq!(c.report(source()).unwrap().event_ids, vec![1]);
    assert_eq!(raw(&c), bytes);
}
#[test]
fn invalid_operation_and_payload_schema_never_commit_a_raw_candidate() {
    for (kind, payload) in [
        ("tool_result", json!("orphan")),
        ("tool_call", json!("not-object")),
    ] {
        let f = Fixture::new();
        let mut c = f.open(false);
        assert!(
            c.append_temporal(source(), &stream(), 0, &frame("x", 1, kind, payload), true)
                .is_err()
        );
        assert!(
            c.log
                .sources()
                .all(|s| c.log.progress(s).unwrap().raw_end == 0)
        );
    }
    let f = Fixture::new();
    let mut c = f.open(false);
    let mut e: Value = serde_json::from_slice(&frame("x", 1, "input", json!("payload"))).unwrap();
    e["unknown"] = json!("forbidden");
    let mut bytes = serde_json::to_vec(&e).unwrap();
    bytes.push(b'\n');
    assert!(
        c.append_temporal(source(), &stream(), 0, &bytes, true)
            .is_err()
    );
    assert_eq!(c.log.sources().count(), 0);
}
#[test]
fn crash_worker() {
    let Some(root) = std::env::var_os("OOJURO_TEMPORAL_TEST_ROOT") else {
        return;
    };
    let f = Fixture {
        root: PathBuf::from(root),
    };
    let mut c = f.open(false);
    c.append_temporal(
        source(),
        &stream(),
        0,
        &frame("crash", 1, "output", json!("CRASHMARKER")),
        true,
    )
    .unwrap();
    std::mem::forget(f);
    panic!("crash hook did not fire");
}
#[test]
fn sigkill_before_caller_ack_restores_raw_ids_and_original_intent() {
    for phase in ["after-authority", "after-projection"] {
        let f = Fixture::new();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["local::temporal_tests::crash_worker", "--exact"])
            .env("OOJURO_TEMPORAL_TEST_ROOT", &f.root)
            .env("OOJURO_LOCAL_TEST_CRASH", phase)
            .output()
            .unwrap();
        assert_eq!(output.status.signal(), Some(libc::SIGKILL));
        let mut c = f.open(true);
        let bytes = frame("crash", 1, "output", json!("CRASHMARKER"));
        let receipt = c
            .append_temporal(source(), &stream(), 0, &bytes, true)
            .unwrap();
        assert_eq!(receipt.outcome.first_id, Some(1));
        assert_eq!(c.report(source()).unwrap().event_ids, vec![1]);
        assert_eq!(raw(&c), bytes);
    }
}
