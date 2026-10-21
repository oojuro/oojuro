//! `periods` on a synthetic journal: ten days of a steady writer (one
//! coordinator session per day, a subagent per hour) with planted
//! episodes, each of which must come first by its own signal:
//! - a stuck loop (the same failing call, again and again) by `repeat`,
//!   while a polling loop (the same call, succeeding) shows only with
//!   `include_waiting`;
//! - an error burst by `errors`;
//! - an hour of tools this writer never used by `unusual`, ahead of a big
//!   but ordinary hour (shape, not size), which leads by `volume`;
//! - Oojuro's own tool calls never count;
//! - task trees list the subagents; `per session` names the instance;
//! - `when` phrases and anchors narrow the period;
//! - forgetting the loop's records removes it (and its parent link), and a
//!   reopened journal answers the same.
mod common;
use common::Scratch;
use oojuro::{
    Filters, Journal, Target,
    append::NewRecord,
    periods::{By, Per, Periods, PeriodsArgs, periods},
};

const W: &str = "claude-code:-p-demo";
const MIN: i64 = 60_000_000;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;

fn sid(key: &str) -> String {
    format!("{:016x}", oojuro_capture::hash64(key))
}
fn main_key(day: i64) -> String {
    format!("/p/-p-demo/main-{day}.jsonl")
}
fn sub_key(day: i64, name: &str) -> String {
    format!("/p/-p-demo/main-{day}/subagents/agent-{name}.jsonl")
}

struct Gen {
    recs: Vec<NewRecord>,
    n: u64,
}

impl Gen {
    #[allow(clippy::too_many_arguments)]
    fn rec(&mut self, kind: &str, session: &str, ts: i64, tool: &str, text: String, call: &str, status: &str) {
        self.recs.push(NewRecord {
            writer: W.into(),
            kind: kind.into(),
            ts_us: ts,
            text,
            tool: tool.into(),
            session: session.into(),
            call_id: call.into(),
            status: status.into(),
            via: "test".into(),
            ..NewRecord::default()
        });
    }
    /// One call and its result; `input` sets the call's text.
    fn call(&mut self, session: &str, ts: i64, tool: &str, input: String, ok: bool) {
        self.n += 1;
        let id = format!("c{}", self.n);
        self.rec("tool_call", session, ts, tool, input, &id, "");
        let out = if ok { format!("done {}", self.n) } else { format!("error: step {} failed", self.n) };
        self.rec("tool_result", session, ts + 1_000_000, tool, out, &id, if ok { "ok" } else { "error" });
    }
}

const TOOLS: [&str; 4] = ["Read", "Bash", "Edit", "Grep"];

/// Ordinary work in `session` from `t` on: `calls` calls about 90 s apart,
/// every input different, results ok except every `fail_every`-th.
fn ordinary(g: &mut Gen, session: &str, t: i64, calls: usize, fail_every: usize, seed: u64) {
    g.rec("user_input", session, t, "", format!("task {seed}: keep going"), "", "");
    for i in 0..calls {
        let tool = TOOLS[(seed as usize + i) % TOOLS.len()];
        let input = format!("{{\"command\":\"step {seed} {i} {}\"}}", "x".repeat((seed as usize * 7 + i * 13) % 50));
        let ok = fail_every == 0 || !(i + seed as usize).is_multiple_of(fail_every);
        g.call(session, t + 2_000_000 + i as i64 * 90_000_000, tool, input, ok);
    }
    g.rec("assistant", session, t + calls as i64 * 90_000_000 + 3_000_000, "", format!("done with {seed}"), "", "");
}

/// The planted episodes and where they are.
struct World {
    t_end: i64,
    loop_at: i64,
    wait_at: i64,
    err_at: i64,
    odd_at: i64,
    big_at: i64,
    echo_at: i64,
    loop_day: i64,
}

fn build(j: &mut Journal) -> World {
    let now = oojuro_capture::time::now_us();
    let t_end = now - now.rem_euclid(HOUR) - HOUR;
    let t0 = t_end - 10 * DAY;
    let mut g = Gen { recs: Vec::new(), n: 0 };
    let w = World {
        t_end,
        loop_day: 8,
        loop_at: t0 + 8 * DAY + 10 * HOUR,
        wait_at: t0 + 8 * DAY + 14 * HOUR,
        err_at: t0 + 9 * DAY + 3 * HOUR,
        odd_at: t0 + 8 * DAY + 20 * HOUR,
        big_at: t0 + 9 * DAY + 12 * HOUR,
        echo_at: t0 + 9 * DAY + 16 * HOUR,
    };
    for h in 0..10 * 24 {
        let t = t0 + h * HOUR;
        let day = h / 24;
        let main = main_key(day);
        let calls = if t == w.big_at { 150 } else { 6 };
        // the big hour packs its calls into the hour
        if t == w.big_at {
            g.rec("user_input", &main, t, "", "big task".into(), "", "");
            for i in 0..calls {
                let tool = TOOLS[i % TOOLS.len()];
                g.call(
                    &main,
                    t + 1_000_000 + i as i64 * 20_000_000,
                    tool,
                    format!("{{\"command\":\"big {i} {}\"}}", "y".repeat(i % 37)),
                    true,
                );
            }
        } else {
            ordinary(&mut g, &main, t, calls, 0, h as u64);
        }
        ordinary(&mut g, &sub_key(day, &format!("h{h}")), t + 10 * MIN, 12, 24, h as u64 + 1000);
        if t == w.echo_at {
            for i in 0..50 {
                g.call(
                    &main,
                    t + 30 * MIN + i * 10_000_000,
                    "mcp__oojuro__search",
                    format!("{{\"query\":\"q{i}\"}}"),
                    true,
                );
            }
        }
    }
    // a stuck loop: the same failing test run, 8 times in a few minutes
    let lk = sub_key(w.loop_day, "loop");
    g.rec("user_input", &lk, w.loop_at, "", "make the tests pass".into(), "", "");
    for i in 0..8 {
        g.call(&lk, w.loop_at + MIN + i * 20_000_000, "Bash", "{\"command\":\"cargo test -p parser\"}".into(), false);
    }
    // same tool, same input length, different inputs, all failing: not a loop
    let fk = sub_key(8, "fake");
    for i in 0..8u8 {
        let pkg = String::from_utf8(vec![b'a' + i; 6]).unwrap();
        g.call(
            &fk,
            w.loop_at + 3 * HOUR + MIN + i64::from(i) * 20_000_000,
            "Bash",
            format!("{{\"command\":\"cargo test -p {pkg}\"}}"),
            false,
        );
    }
    // a polling loop: the same status check, succeeding
    let wk = sub_key(8, "wait");
    for i in 0..8 {
        g.call(&wk, w.wait_at + MIN + i * 20_000_000, "Bash", "{\"command\":\"gh run view 4242\"}".into(), true);
    }
    // the anchor phrase, then an error burst: different calls, all failing
    let ek = sub_key(9, "err");
    g.rec("user_input", &ek, w.err_at - 10 * MIN, "", "now deploy the zebracorn release".into(), "", "");
    for i in 0..12 {
        g.call(
            &ek,
            w.err_at + MIN + i * 15_000_000,
            "Bash",
            format!("{{\"command\":\"deploy step {i} {}\"}}", "z".repeat(i as usize)),
            false,
        );
    }
    // an unusual hour: a new top-level session using tools never seen
    let ok = "/p/-p-demo/odd.jsonl";
    for i in 0..40 {
        let tool = ["WebFetch", "WebSearch", "mcp__chrome__click", "mcp__chrome__shot", "NotebookEdit", "Task"][i % 6];
        g.call(ok, w.odd_at + MIN + i as i64 * 60_000_000, tool, format!("{{\"url\":\"https://e.x/{i}\"}}"), true);
    }
    g.recs.sort_by_key(|r| r.ts_us);
    for chunk in g.recs.chunks(500) {
        j.owner().append(chunk).unwrap();
    }
    w
}

fn run(j: &Journal, w: &World, by: By, per: Per, f: impl FnOnce(&mut PeriodsArgs)) -> Periods {
    let mut a = PeriodsArgs {
        by: Some(by),
        per,
        from_us: Some(w.t_end - 2 * DAY - 6 * HOUR),
        to_us: Some(w.t_end),
        writers: vec![W.into()],
        k: 10,
        samples: 3,
        tz: Some("UTC".into()),
        ..PeriodsArgs::default()
    };
    f(&mut a);
    periods(&j.reader(), &a).unwrap()
}

fn has_agent(p: &oojuro::periods::Period, key: &str) -> bool {
    p.agents.iter().any(|a| a.agent_id == sid(key))
}

#[test]
fn each_planted_episode_comes_first_by_its_signal() {
    let s = Scratch::new("periods");
    let mut j = Journal::open(&s.path("journal")).unwrap();
    let w = build(&mut j);
    let (lk, wk, ek) = (sub_key(w.loop_day, "loop"), sub_key(8, "wait"), sub_key(9, "err"));

    // stuck loop first by repeat; its task tree is the day's coordinator
    let r = run(&j, &w, By::Repeat, Per::Task, |_| {});
    let top = &r.periods[0];
    assert_eq!(r.window, "5m");
    assert!(has_agent(top, &lk), "{top:?}");
    assert_eq!(top.agent_id, sid(&main_key(w.loop_day)), "per task: the tree's root");
    assert!(top.repeats_stuck >= 6 && top.repeat_bits > 10.0, "{top:?}");
    let a = top.agents.iter().find(|a| a.agent_id == sid(&lk)).unwrap();
    assert_eq!(a.parent_id.as_deref(), Some(sid(&main_key(w.loop_day)).as_str()));
    assert!(top.samples.iter().all(|x| x.why == "stuck" || x.why == "first"));
    assert!(r.periods.iter().all(|p| !has_agent(p, &wk)), "polling is waiting, not stuck");
    let fk = sub_key(8, "fake");
    assert!(r.periods.iter().all(|p| !has_agent(p, &fk)), "same length, different calls: not a repeat");
    assert!(r.periods.iter().all(|p| p.repeats_exact));
    let errs = run(&j, &w, By::Errors, Per::Task, |_| {});
    let fake = errs.periods.iter().find(|p| has_agent(p, &fk)).expect("the failing calls are an error burst");
    assert!(!fake.repeats_exact && fake.repeats_stuck >= 6, "outside `repeat`, counts are candidates: {fake:?}");
    // with waiting loops, the polling loop shows up too
    let r = run(&j, &w, By::Repeat, Per::Task, |a| a.include_waiting = true);
    assert!(r.periods.iter().any(|p| has_agent(p, &wk) && p.repeats_waiting >= 6));
    // per session: the instance itself
    let r = run(&j, &w, By::Repeat, Per::Session, |_| {});
    assert_eq!(r.periods[0].agent_id, sid(&lk));

    // error burst first by errors
    let r = run(&j, &w, By::Errors, Per::Task, |_| {});
    let top = &r.periods[0];
    assert!(has_agent(top, &ek) && top.errors >= 12 && top.error_bits > 20.0, "{top:?}");
    assert!(top.samples.iter().any(|x| x.why == "error" && x.kind == "tool_result"));

    // unusual: the hour of new tools, ahead of the big ordinary hour
    let r = run(&j, &w, By::Unusual, Per::Task, |_| {});
    let top = &r.periods[0];
    assert_eq!(top.agent_id, sid("/p/-p-demo/odd.jsonl"), "{:#?}", &r.periods[..3.min(r.periods.len())]);
    assert!(top.unusual.new > 0.0 && top.unusual.mix > 0.0, "{top:?}");
    let big =
        r.periods.iter().position(|p| p.from_us <= w.big_at && p.to_us > w.big_at && p.agent_id == sid(&main_key(9)));
    assert!(big.is_none_or(|i| i > 0), "the big ordinary hour is not the most unusual");

    // volume: the big hour leads; Oojuro's own calls never count
    let r = run(&j, &w, By::Volume, Per::Writer, |_| {});
    assert_eq!(r.periods[0].from_us, w.big_at, "{:?}", r.periods[0]);
    assert!(r.periods[0].events > 300);
    let echo_hour = run(&j, &w, By::Volume, Per::Writer, |a| {
        a.from_us = Some(w.echo_at);
        a.to_us = Some(w.echo_at + HOUR - 1);
    });
    assert!(echo_hour.periods[0].events < 60, "echo counted: {}", echo_hour.periods[0].events);
    assert!(!echo_hour.periods[0].tools.contains_key("mcp__oojuro__search"));

    // when: a plain phrase equals explicit bounds; an anchor narrows to after it
    let from = oojuro_capture::time::format_rfc3339_us(w.t_end - 2 * DAY - 6 * HOUR);
    let to = oojuro_capture::time::format_rfc3339_us(w.t_end);
    let by_when = run(&j, &w, By::Errors, Per::Task, |a| {
        a.from_us = None;
        a.to_us = None;
        a.when = Some(format!("between {from} and {to}"));
    });
    let plain = run(&j, &w, By::Errors, Per::Task, |_| {});
    let key = |r: &Periods| r.periods.iter().map(|p| (p.from_us, p.agent_id.clone(), p.score)).collect::<Vec<_>>();
    assert_eq!(key(&by_when)[..1], key(&plain)[..1]);
    let after = run(&j, &w, By::Repeat, Per::Task, |a| {
        a.from_us = None;
        a.to_us = None;
        a.when = Some("after the first \"zebracorn\"".into());
    });
    let anchor = &after.when.as_ref().unwrap().anchors[0];
    assert_eq!(anchor.agent_id, sid(&ek));
    assert!(after.periods.iter().all(|p| !has_agent(p, &lk)), "the loop was before the anchor");

    // forgetting the loop removes it, and its parent link
    let ids: Vec<u64> = {
        let r = j.reader();
        let ids: Vec<u64> = r
            .range(&Filters { writers: vec![W.into()], ..Filters::default() }, usize::MAX)
            .unwrap()
            .into_iter()
            .filter(|x| x.agent_id == sid(&lk))
            .map(|x| x.id)
            .collect();
        assert_eq!(r.get_id(ids[0]).unwrap().unwrap().parent_id, Some(sid(&main_key(w.loop_day))));
        ids
    };
    assert_eq!(ids.len(), 17);
    let before = run(&j, &w, By::Errors, Per::Task, |_| {});
    for id in &ids {
        j.forget(&Target::Id { id: *id }, false).unwrap();
    }
    let r = run(&j, &w, By::Repeat, Per::Task, |_| {});
    assert!(r.periods.iter().all(|p| !has_agent(p, &lk)), "a forgotten loop is gone");
    assert!(j.reader().db().parent_of(oojuro_capture::hash64(&lk)).is_none());
    let errors_after = run(&j, &w, By::Errors, Per::Task, |_| {});
    // and a reopened journal answers the same, from its stored columns
    drop(j);
    let j = Journal::open(&s.path("journal")).unwrap();
    let again = run(&j, &w, By::Errors, Per::Task, |_| {});
    assert_eq!(key(&again), key(&errors_after));
    assert!(key(&before).len() >= key(&again).len());
    let r = run(&j, &w, By::Repeat, Per::Task, |_| {});
    assert!(r.periods.iter().all(|p| !has_agent(p, &lk)));
}
