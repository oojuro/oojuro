//! Crash gate for the shared log: a child process with 8 writers on 4 stores
//! appends 3-record turns and prints each acknowledgement; it is killed with
//! SIGKILL mid-write, three times. After each kill every acknowledged record
//! must read back exactly and every turn must be whole (all-or-nothing).
//!
//! Its own test binary: a child is forked while spawning, and a fork briefly
//! holds every descriptor of the parent, including other tests' store locks.
use oojuro_nine::{LogBinding, Nine, Options, Receipts, SharedLog, SharedLogOptions};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const CHILD_ENV: &str = "NINE_SHARED_LOG_CHILD";
const RUN_ENV: &str = "NINE_SHARED_LOG_RUN";

fn log_options() -> SharedLogOptions {
    SharedLogOptions {
        segment_bytes: 8 << 10,
        ..SharedLogOptions::default()
    }
}

fn store(dir: &Path, log: &Arc<SharedLog>, s: u64) -> Result<Nine> {
    let options = Options {
        flush_text_bytes: 4_000,
        flush_postings: usize::MAX,
        receipts: Receipts::Off,
        shared_log: Some(LogBinding {
            log: Arc::clone(log),
            store: s,
        }),
        ..Options::default()
    };
    Nine::open_with(&dir.join(format!("s{s}")), options)
}

fn kill_text(run: u32, thread: usize, i: u64, part: usize) -> String {
    format!("kill run{run} t{thread} i{i} part{part} {}", "z".repeat((i as usize * 7 + part) % 23))
}

/// Child half (a no-op unless spawned by the gate below).
#[test]
fn kill_test_child_writer() {
    let Some(dir) = std::env::var_os(CHILD_ENV) else { return };
    let run: u32 = std::env::var(RUN_ENV).unwrap().parse().unwrap();
    let dir = PathBuf::from(dir);
    let log = SharedLog::open(&dir.join("log"), log_options()).unwrap();
    let stores: Vec<Arc<Nine>> = (0..4).map(|s| Arc::new(store(&dir, &log, s).unwrap())).collect();
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let db = Arc::clone(&stores[t % 4]);
            std::thread::spawn(move || {
                // Appends until the gate kills the process.
                for i in 0..u64::MAX {
                    let texts: Vec<String> = (0..3).map(|p| kill_text(run, t, i, p)).collect();
                    let items: Vec<(i64, &str)> = texts.iter().map(|x| (i as i64, x.as_str())).collect();
                    let ids = db.put_batch(&items).unwrap();
                    println!("ack {} {run} {t} {i} {}", t % 4, ids.join(" "));
                }
            })
        })
        .collect();
    for t in threads {
        let _ = t.join();
    }
}

#[cfg(unix)]
#[test]
fn acknowledged_batches_survive_killing_the_process() -> Result<()> {
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    let shm = Path::new("/dev/shm");
    let base = if shm.is_dir() { shm.to_path_buf() } else { std::env::temp_dir() };
    let dir = base.join(format!("nine-shared-kill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let mut acked: Vec<(usize, String, String)> = Vec::new();
    for run in 0..3u32 {
        let mut child = Command::new(std::env::current_exe()?)
            .args(["--exact", "kill_test_child_writer", "--nocapture", "--test-threads", "1"])
            .env(CHILD_ENV, &dir)
            .env(RUN_ENV, run.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
                if line.starts_with("ack ") && tx.send(line).is_err() {
                    break;
                }
            }
        });
        let want = 40 + 60 * run as usize;
        let start = std::time::Instant::now();
        let mut lines = Vec::new();
        while lines.len() < want && start.elapsed().as_secs() < 60 {
            if let Ok(l) = rx.recv_timeout(std::time::Duration::from_millis(50)) {
                lines.push(l);
            }
        }
        child.kill()?;
        child.wait()?;
        reader.join().unwrap();
        lines.extend(rx.try_iter());
        assert!(lines.len() >= want, "run {run}: child acknowledged only {}", lines.len());
        for l in &lines {
            let f: Vec<&str> = l.split(' ').collect();
            let (s, t, i): (usize, usize, u64) = (f[1].parse()?, f[3].parse()?, f[4].parse()?);
            for (p, id) in f[5..].iter().enumerate() {
                acked.push((s, id.to_string(), kill_text(run, t, i, p)));
            }
        }
        // Recover: every acknowledged record is back, and turns are whole.
        let log = SharedLog::open(&dir.join("log"), log_options())?;
        let stores: Vec<Nine> = (0..4).map(|s| store(&dir, &log, s)).collect::<Result<_>>()?;
        let recovered: usize = stores.iter().map(Nine::len).sum();
        eprintln!("kill run {run}: {} acknowledged turns, {recovered} records recovered", lines.len());
        assert!(recovered >= acked.len());
        for (s, id, text) in &acked {
            assert_eq!(stores[*s].read(id)?.as_deref(), Some(text.as_str()), "run {run}: lost {id}");
        }
        for db in &stores {
            let mut parts: HashMap<String, usize> = HashMap::new();
            for r in db.records_from(0, usize::MAX)? {
                let turn = r.text.split(" part").next().unwrap().to_string();
                *parts.entry(turn).or_default() += 1;
            }
            assert!(parts.values().all(|n| *n == 3), "run {run}: a turn was split");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
