//! Synthetic fixtures follow official Pi v3 documentation; no private sessions.
use super::*;
use crate::status;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::process::ExitStatusExt,
    process::Command,
};
const KEY: [u8; 32] = [83; 32];
const SOURCE: Source = [57; 16];
const WRITER: &str = "pi:--synthetic--";
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
                "native-pi-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let file = root.join(".pi/agent/sessions/--synthetic--/session.jsonl");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        Self { root, file }
    }
    fn open(&self, strict: bool) -> LocalCapture {
        LocalCapture::open_pi(
            &self.root.join("authority"),
            &KEY,
            Options::new(WRITER, 3, "synthetic-v1"),
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
    fn decide(&mut self, r: &Rec, _: &str) -> Decision {
        if self.0 && (r.kind == kind::THINKING || r.tool == "blocked_tool") {
            return Decision::Withhold("synthetic-denied");
        }
        Decision::Store {
            indexed: kind::indexed(r.kind) && !r.from_oojuro() && !r.text.contains("STOREONLY"),
        }
    }
}
fn line(v: Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(&v).unwrap();
    b.push(b'\n');
    b
}
fn header() -> Vec<u8> {
    line(
        json!({"type":"session","version":3,"id":"synthetic-session","cwd":"/synthetic","timestamp":"2026-01-02T03:04:05Z","parentSession":"/not-opened/parent.jsonl"}),
    )
}
fn entry(id: &str, parent: Value, msg: Value) -> Vec<u8> {
    line(
        json!({"type":"message","id":id,"parentId":parent,"timestamp":"2026-01-02T03:04:05Z","message":msg}),
    )
}
fn user(id: &str, text: &str) -> Vec<u8> {
    entry(
        id,
        Value::Null,
        json!({"role":"user","content":text,"timestamp":1767323045001_i64}),
    )
}
fn call(id: &str, name: &str) -> Vec<u8> {
    entry(
        &format!("entry-{id}"),
        json!("previous"),
        json!({"role":"assistant","content":[{"type":"toolCall","id":id,"name":name,"arguments":{"command":"synthetic command"}}]}),
    )
}
fn result(id: &str, name: &str, text: &str) -> Vec<u8> {
    entry(
        &format!("result-{id}"),
        json!("previous"),
        json!({"role":"toolResult","toolCallId":id,"toolName":name,"content":[{"type":"text","text":text}],"isError":false}),
    )
}
fn raw(c: &LocalCapture) -> Vec<u8> {
    let mut b = Vec::new();
    c.restore_raw(SOURCE, &mut b, true).unwrap();
    b
}
fn capture(c: &mut LocalCapture, f: &Fixture) -> Report {
    c.capture_file(SOURCE, &f.file, |_| true).unwrap().unwrap()
}

#[test]
fn atomic_raw_roundtrip_redaction_timestamps_links_and_restart() {
    let f = Fixture::new();
    let secret = format!("sk-ant-api03-{}", "Qx7".repeat(35));
    let bytes=[header(),user("u1",&format!("hello {secret}")),
        call("call-1","bash"),result("call-1","bash","successful tool output"),
        entry("a2",json!("u1"),json!({"role":"assistant","content":[{"type":"thinking","thinking":"reason"},{"type":"text","text":"final answer"}]}))].concat();
    fs::write(&f.file, &bytes).unwrap();
    let mut c = f.open(false);
    let first = capture(&mut c, &f);
    assert_eq!(first.event_ids, vec![1, 2, 3, 4, 5]);
    let event = c.get(1).unwrap().unwrap().0;
    assert_eq!(event.ts_us, 1767323045001000);
    assert!(!event.text.contains(&secret));
    assert_eq!(c.get(3).unwrap().unwrap().0.parent, 2);
    assert_eq!(c.get(3).unwrap().unwrap().0.tool, "bash");
    assert_eq!(raw(&c), bytes);
    assert!(
        !c.search("successful tool output", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    drop(c);
    let mut c = f.open(false);
    assert_eq!(capture(&mut c, &f), first);
    assert_eq!(raw(&c), bytes);
}
#[test]
fn withheld_call_and_thinking_never_reappear_after_policy_relaxes() {
    let f = Fixture::new();
    fs::write(&f.file,[header(),call("blocked","blocked_tool"),entry("t",Value::Null,json!({"role":"assistant","content":[{"type":"thinking","thinking":"PRIVATETHOUGHT"}]})),user("u","STOREONLY marker")].concat()).unwrap();
    let mut c = f.open(true);
    assert_eq!(capture(&mut c, &f).event_ids, vec![1]);
    drop(c);
    f.append(&result("blocked", "blocked_tool", "PRIVATERESULT"));
    let mut c = f.open(false);
    assert_eq!(capture(&mut c, &f).event_ids, vec![1]);
    assert!(
        c.search("PRIVATERESULT PRIVATETHOUGHT STOREONLY", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        String::from_utf8(raw(&c))
            .unwrap()
            .contains("PRIVATERESULT")
    );
}
#[test]
fn branching_context_edits_compaction_and_unknown_roles_preserve_observed_history() {
    let f = Fixture::new();
    let bytes=[header(),user("root","original observed text"),user("branch","alternate observed text"),
        line(json!({"type":"context_edit","id":"edit","parentId":"branch","targetId":"root","replacement":null})),
        line(json!({"type":"compaction","id":"compact","parentId":"edit","firstKeptEntryId":"compact","summary":"historical summary","systemMessage":{"role":"system","content":"bounded prompt checkpoint"}})),
        line(json!({"type":"branch_summary","id":"summary","parentId":"root","fromId":"branch","summary":"other branch"})),
        entry("ext",Value::Null,json!({"role":"unknownExtension","content":"raw only extension"})),
        line(json!({"type":"custom","id":"state","parentId":null,"data":{"private":"raw state"}})),
        b"{malformed\n".to_vec(),vec![0xff,b'\n']].concat();
    fs::write(&f.file, &bytes).unwrap();
    let mut c = f.open(false);
    let r = capture(&mut c, &f);
    assert_eq!(r.event_ids.len(), 5);
    assert_eq!(raw(&c), bytes);
    let one = c.get(1).unwrap().unwrap().0;
    assert_eq!(one.text, "original observed text");
    let edit = c.get(3).unwrap().unwrap().0;
    assert!(!edit.indexed);
    assert!(edit.text.contains("targetId"));
    assert!(
        c.search("raw only extension", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn unfinished_line_keeps_policy_identity_and_resumes_exactly() {
    let f = Fixture::new();
    let mut bytes = [header(), user("u", "complete first")].concat();
    let mut tail = user("v", "tail second");
    tail.pop();
    bytes.extend(&tail);
    fs::write(&f.file, &bytes).unwrap();
    let mut c = f.open(false);
    let before = capture(&mut c, &f);
    assert!(before.parsed_end < before.raw_end);
    drop(c);
    f.append(b"\n");
    let mut changed = Options::new(WRITER, 3, "changed-policy");
    changed.chunk_bytes = 17;
    let mut c = LocalCapture::open_pi(
        &f.root.join("authority"),
        &KEY,
        changed,
        Box::new(Mode(false)),
    )
    .unwrap();
    assert!(c.capture_file(SOURCE, &f.file, |_| true).is_err());
    assert_eq!(c.report(SOURCE).unwrap(), before);
    drop(c);
    let mut c = f.open(false);
    assert_eq!(capture(&mut c, &f).event_ids, vec![1, 2]);
    bytes.push(b'\n');
    assert_eq!(raw(&c), bytes);
}
#[test]
fn unsupported_header_missing_calls_and_forged_result_names_refuse_before_ack() {
    for bytes in [
        line(json!({"type":"session","version":4,"id":"x","cwd":"/synthetic"})),
        user("u","missing header"),
        [header(),result("unknown","bash","orphan")].concat(),
        [header(),call("c","blocked_tool"),result("c","allowed_tool","forged")].concat(),
        [header(),entry("bad",Value::Null,json!({"role":"assistant","content":[{"type":"toolCall","name":"bash","arguments":{}}]}))].concat(),
        [header(),entry("bad",Value::Null,json!({"role":"assistant","content":[{"type":"toolCall","id":"c","name":"bash"},{"type":"toolCall","id":"c","name":"bash","arguments":{}}]}))].concat(),
    ] {
        let f=Fixture::new();fs::write(&f.file,bytes).unwrap();let mut c=f.open(false);
        assert!(c.capture_file(SOURCE,&f.file,|_|true).is_err());
        let r=c.report(SOURCE).unwrap();assert_eq!(r.raw_end,0);assert!(r.event_ids.is_empty());
    }
}
#[test]
fn consent_precedes_path_access_and_adapter_authority_cannot_be_reinterpreted() {
    let f = Fixture::new();
    let mut c = f.open(false);
    assert_eq!(
        c.capture_file(SOURCE, &f.root.join("does-not-exist"), |_| false)
            .unwrap(),
        None
    );
    assert!(c.authority().unwrap().sources().next().is_none());
    drop(c);
    assert!(
        LocalCapture::open(
            &f.root.join("authority"),
            &KEY,
            Options::new(WRITER, 3, "synthetic-v1"),
            Box::new(Mode(false))
        )
        .is_err()
    );
    assert!(
        LocalCapture::open_claude(
            &f.root.join("authority"),
            &KEY,
            Options::new(WRITER, 3, "synthetic-v1"),
            Box::new(Mode(false))
        )
        .is_err()
    );
}
#[test]
fn completed_calls_allow_reuse_and_lookup_markers_remain_stored_only() {
    let f = Fixture::new();
    let mut bytes = header();
    for n in 0..70 {
        bytes.extend(call("reuse", "bash"));
        bytes.extend(result("reuse", "bash", &format!("completed number {n}")));
    }
    bytes.extend(call("lookup", "mcp__oojuro__get"));
    bytes.extend(result("lookup", "mcp__oojuro__get", "RECALLEDONLY"));
    fs::write(&f.file, bytes).unwrap();
    let mut c = f.open(false);
    assert_eq!(capture(&mut c, &f).event_ids.len(), 142);
    assert!(!c.get(141).unwrap().unwrap().0.indexed);
    assert!(!c.get(142).unwrap().unwrap().0.indexed);
    assert!(
        c.search("RECALLEDONLY", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    for n in (2..=140).step_by(2) {
        assert_eq!(c.get(n).unwrap().unwrap().0.parent, n - 1);
    }
}
#[test]
fn unresolved_call_capacity_refuses_without_evicting_retained_policy_state() {
    let f = Fixture::new();
    let mut bytes = header();
    for n in 0..64 {
        bytes.extend(call(&format!("call-{n}"), "bash"));
    }
    fs::write(&f.file, &bytes).unwrap();
    let mut c = f.open(false);
    let before = capture(&mut c, &f);
    f.append(&call("overflow", "bash"));
    assert!(c.capture_file(SOURCE, &f.file, |_| true).is_err());
    assert_eq!(c.report(SOURCE).unwrap(), before);
    assert_eq!(raw(&c), bytes);
}
#[test]
fn direct_bash_exit_and_large_output_path_are_recorded_without_sidefile_access() {
    let f = Fixture::new();
    let other = f.root.join("must-not-read");
    fs::write(&other, "SIDECONTENT").unwrap();
    let bytes=[header(),entry("bash",Value::Null,json!({"role":"bashExecution","command":"synthetic","output":"visible output","exitCode":7,"cancelled":false,"truncated":true,"fullOutputPath":other})),
        entry("system",Value::Null,json!({"role":"system","content":"","sections":{"preamble":"visible system checkpoint"},"toolsAdded":[{"name":"read","parameters":{}}]}))].concat();
    fs::write(&f.file, &bytes).unwrap();
    let mut c = f.open(false);
    capture(&mut c, &f);
    let e = c.get(1).unwrap().unwrap().0;
    assert_eq!(e.exit_code, Some(7));
    assert_eq!(e.status, status::ERROR);
    assert!(e.text.contains("visible output"));
    assert!(!e.text.contains("SIDECONTENT"));
    let e = c.get(2).unwrap().unwrap().0;
    assert!(e.text.contains("visible system checkpoint"));
    assert!(!e.indexed);
}
#[test]
fn crash_worker() {
    let Some(root) = std::env::var_os("OOJURO_PI_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let mut c = LocalCapture::open_pi(
        &root.join("authority"),
        &KEY,
        Options::new(WRITER, 3, "synthetic-v1"),
        Box::new(Mode(false)),
    )
    .unwrap();
    c.capture_file(
        SOURCE,
        &root.join(".pi/agent/sessions/--synthetic--/session.jsonl"),
        |_| true,
    )
    .unwrap();
    panic!("crash hook did not fire");
}
#[test]
fn sigkill_after_authority_or_projection_recovers_before_ack_without_duplicates() {
    for phase in ["after-authority", "after-projection"] {
        let f = Fixture::new();
        let bytes = [header(), user("u", "CRASHMARKER"), call("c", "bash")].concat();
        fs::write(&f.file, &bytes).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["local::pi_tests::crash_worker", "--exact"])
            .env("OOJURO_PI_TEST_ROOT", &f.root)
            .env("OOJURO_LOCAL_TEST_CRASH", phase)
            .output()
            .unwrap();
        assert_eq!(output.status.signal(), Some(libc::SIGKILL));
        let mut c = f.open(false);
        assert_eq!(capture(&mut c, &f).event_ids, vec![1, 2]);
        assert_eq!(raw(&c), bytes);
    }
}

#[test]
fn split_header_chunks_match_full_capture_and_empty_results_retire_calls() {
    let a = Fixture::new();
    let b = Fixture::new();
    let bytes = [
        header(),
        call("reused", "bash"),
        result("reused", "bash", ""),
        call("reused", "bash"),
        result("reused", "bash", "later completion"),
        user("final", "snowman \u{2603} final"),
    ]
    .concat();
    fs::write(&a.file, &bytes).unwrap();
    fs::write(&b.file, &bytes).unwrap();
    let mut one = a.open(false);
    let first = capture(&mut one, &a);
    let mut options = Options::new(WRITER, 3, "synthetic-v1");
    options.chunk_bytes = 17;
    let mut split = LocalCapture::open_pi(
        &b.root.join("authority"),
        &KEY,
        options,
        Box::new(Mode(false)),
    )
    .unwrap();
    let second = capture(&mut split, &b);
    assert_eq!(first.event_ids, second.event_ids);
    assert_eq!(raw(&split), bytes);
    for id in first.event_ids {
        let x = one.get(id).unwrap().unwrap().0;
        let y = split.get(id).unwrap().unwrap().0;
        assert_eq!(
            (x.text, x.parent, x.tool, x.ts_us),
            (y.text, y.parent, y.tool, y.ts_us)
        );
    }
}

#[test]
fn unknown_content_blocks_cannot_relabel_thinking_as_user_or_tool_output() {
    let f = Fixture::new();
    let blocked = json!([{"type":"thinking","thinking":"WITHHELDTHOUGHTMARKER"}]);
    let bytes=[header(),entry("user",Value::Null,json!({"role":"user","content":blocked})),
        entry("custom",Value::Null,json!({"role":"custom","content":blocked})),
        call("c","bash"),entry("result",Value::Null,json!({"role":"toolResult","toolCallId":"c","toolName":"bash","content":blocked,"isError":false}))].concat();
    fs::write(&f.file, &bytes).unwrap();
    let mut c = f.open(true);
    let r = capture(&mut c, &f);
    for id in r.event_ids {
        assert!(
            !c.get(id)
                .unwrap()
                .unwrap()
                .0
                .text
                .contains("WITHHELDTHOUGHTMARKER")
        );
    }
    assert!(
        String::from_utf8(raw(&c))
            .unwrap()
            .contains("WITHHELDTHOUGHTMARKER")
    );
}

#[test]
fn direct_bash_lookup_results_remain_stored_but_are_never_indexed() {
    let f = Fixture::new();
    let bytes=[header(),entry("lookup",Value::Null,json!({"role":"bashExecution","command":"oojuro get 17","output":"DIRECTRECALLEDMARKER","exitCode":0,"cancelled":false,"truncated":false}))].concat();
    fs::write(&f.file, &bytes).unwrap();
    let mut c = f.open(false);
    capture(&mut c, &f);
    let event = c.get(1).unwrap().unwrap().0;
    assert!(event.text.contains("DIRECTRECALLEDMARKER"));
    assert!(!event.indexed, "direct lookup result was indexed");
    assert!(
        c.search("DIRECTRECALLEDMARKER", 0, i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(raw(&c), bytes);
}
