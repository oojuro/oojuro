//! Per-agent stores at tenant scale: N agents, each with a few turns.
//! Reports process memory, threads, open files, disk, append and reopen
//! time, i.e. the fixed cost of one engine per agent.
//! Usage: many_agents <dir> <agents> [turns per agent]
use oojuro_trace::{EventIn, Options, TraceDb};
use std::time::Instant;

fn status(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find(|l| l.starts_with(field))
        .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(0)
}

fn report(stage: &str, agents: u32, t: f64) {
    let fds = std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count());
    println!(
        "{{\"stage\":\"{stage}\",\"agents\":{agents},\"seconds\":{t:.2},\"rss_mb\":{:.1},\"anon_mb\":{:.1},\"threads\":{},\"fds\":{fds}}}",
        status("VmRSS:") as f64 / 1024.0,
        status("RssAnon:") as f64 / 1024.0,
        status("Threads:")
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(&a[1]);
    let agents: u32 = a[2].parse().unwrap();
    let turns: u32 = a.get(3).map_or(5, |s| s.parse().unwrap());
    let _ = std::fs::remove_dir_all(&dir);
    report("start", 0, 0.0);
    let text = "tool output line with some words about files and tests and errors\n".repeat(20);
    let t = Instant::now();
    {
        let db = TraceDb::open(&dir, Options::default()).unwrap();
        let mut id = 0u64;
        for turn in 0..turns {
            for ag in 0..agents {
                id += 1;
                let e = EventIn {
                    id,
                    session: ag as u64 + 1,
                    agent: ag,
                    turn,
                    seq: turn,
                    parent: 0,
                    revises: 0,
                    kind: 5,
                    status: 2,
                    exit_code: Some(0),
                    tokens_in: 0,
                    tokens_out: 0,
                    ts_us: 1_000_000 * (turn as i64 * agents as i64 + ag as i64),
                    duration_us: 0,
                    tool: "bash",
                    args: "",
                    text: &text,
                    indexed: true,
                };
                db.append(&[e]).unwrap();
            }
        }
        report("appended", agents, t.elapsed().as_secs_f64());
        let t = Instant::now();
        let mut hits = 0;
        for ag in 0..agents {
            hits += db.search(ag, "errors in tests", i64::MAX / 2, 20).unwrap().len();
        }
        println!("{{\"stage\":\"searched\",\"agents\":{agents},\"ms_per_search\":{:.3},\"hits\":{hits}}}", t.elapsed().as_secs_f64() * 1e3 / agents as f64);
        db.flush().unwrap();
        println!("{{\"stage\":\"disk\",\"agents\":{agents},\"bytes\":{},\"files\":{}}}", db.disk_bytes(), db.files().len());
    }
    let t = Instant::now();
    let db = TraceDb::open(&dir, Options::default()).unwrap();
    report("reopened", agents, t.elapsed().as_secs_f64());
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
