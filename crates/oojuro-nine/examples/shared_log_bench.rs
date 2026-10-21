//! Shared-log micro-benchmark: N writer threads x M durable appends (one
//! commit each, `put_batch` of R records), against private per-store
//! journals and then against one shared log under the same stores. Prints
//! one JSON line per mode: commits/s, latency, and the journal `sync_data`
//! calls the process made (instrumented in the journal and the shared log),
//! so syncs per commit is exact even when timings are noisy.
//!
//! Usage:
//!   cargo run --release -p oojuro-nine --example shared_log_bench -- \
//!     [--writers 16] [--stores 16] [--appends 300] [--records 3] [--bytes 400] \
//!     [--dir $TMPDIR] [--modes private,shared] [--spread]
//! Writer w appends to store w % stores, or with `--spread` each append goes
//! to a pseudo-random store (TraceBench binds writers to sessions, not
//! agents, so writers collide on agents). `--dir` decides the device; on a
//! tmpfs directory no disk is touched and syncs are free, so leaders rarely
//! find others waiting (a floor for coalescing). To model a device's flush
//! latency there, build with `--features simulated-sync` and set
//! `OOJURO_NINE_SIMULATED_SYNC_US=1000`: sync counts then reflect that
//! latency; throughput does not (simulated syncs overlap, real ones queue).
use oojuro_nine::{LogBinding, Nine, Options, Receipts, SharedLog, SharedLogOptions, journal_syncs};
use std::{path::PathBuf, sync::Arc, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn main() -> Result<()> {
    let mut writers = 16usize;
    let mut stores = 0usize;
    let mut appends = 300usize;
    let mut records = 3usize;
    let mut bytes = 400usize;
    let mut dir = std::env::temp_dir();
    let mut modes = "private,shared".to_string();
    let mut spread = false;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--spread" {
            spread = true;
            i += 1;
            continue;
        }
        let v = args.get(i + 1).cloned().ok_or("missing value")?;
        match args[i].as_str() {
            "--writers" => writers = v.parse()?,
            "--stores" => stores = v.parse()?,
            "--appends" => appends = v.parse()?,
            "--records" => records = v.parse()?,
            "--bytes" => bytes = v.parse()?,
            "--dir" => dir = PathBuf::from(v),
            "--modes" => modes = v,
            x => return Err(format!("unknown flag {x}").into()),
        }
        i += 2;
    }
    let stores = if stores == 0 { writers } else { stores };
    let base = dir.join(format!("shared-log-bench-{}", std::process::id()));
    for mode in modes.split(',') {
        let root = base.join(mode);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        let log = match mode {
            "shared" => Some(SharedLog::open(&root.join("log"), SharedLogOptions::default())?),
            "private" => None,
            x => return Err(format!("unknown mode {x}").into()),
        };
        let dbs: Vec<Arc<Nine>> = (0..stores)
            .map(|s| {
                let options = Options {
                    receipts: Receipts::Off,
                    shared_log: log.as_ref().map(|l| LogBinding { log: Arc::clone(l), store: s as u64 }),
                    ..Options::default()
                };
                Ok(Arc::new(Nine::open_with(&root.join(format!("s{s:03}")), options)?))
            })
            .collect::<Result<_>>()?;
        let syncs0 = journal_syncs();
        let t0 = Instant::now();
        let mut lat: Vec<f64> = std::thread::scope(|scope| {
            let hs: Vec<_> = (0..writers)
                .map(|w| {
                    let dbs = &dbs;
                    scope.spawn(move || {
                        let mut lat = Vec::with_capacity(appends);
                        for a in 0..appends {
                            let pick = if spread {
                                let mut z = (w as u64) << 32 | a as u64;
                                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                                (z ^ (z >> 31)) as usize % stores
                            } else {
                                w % stores
                            };
                            let db = &dbs[pick];
                            let texts: Vec<String> = (0..records)
                                .map(|r| {
                                    let head = format!("writer {w} turn {a} record {r} ");
                                    let pad = bytes.saturating_sub(head.len());
                                    head + &"tool output line ".repeat(pad / 17 + 1)[..pad]
                                })
                                .collect();
                            let items: Vec<(i64, &str)> = texts.iter().map(|t| (a as i64, t.as_str())).collect();
                            let t = Instant::now();
                            db.put_batch(&items).expect("durable append");
                            lat.push(t.elapsed().as_secs_f64() * 1e3);
                        }
                        lat
                    })
                })
                .collect();
            hs.into_iter().flat_map(|h| h.join().expect("writer")).collect()
        });
        let wall = t0.elapsed().as_secs_f64();
        let syncs = journal_syncs() - syncs0;
        lat.sort_by(f64::total_cmp);
        let pct = |p: f64| lat[((lat.len() as f64 * p) as usize).min(lat.len() - 1)];
        let commits = (writers * appends) as u64;
        let (rounds, batches) = log.as_ref().map_or((None, None), |l| {
            let s = l.stats();
            (Some(s.rounds), Some(s.batches))
        });
        println!(
            "{{\"mode\":\"{mode}\",\"writers\":{writers},\"stores\":{stores},\"appends_per_writer\":{appends},\
             \"records_per_commit\":{records},\"spread\":{spread},\"commits\":{commits},\"wall_s\":{wall:.3},\
             \"commits_per_s\":{:.0},\"p50_ms\":{:.3},\"p99_ms\":{:.3},\"journal_syncs\":{syncs},\
             \"syncs_per_commit\":{:.4},\"log_rounds\":{},\"log_batches\":{},\"dir\":{:?}}}",
            commits as f64 / wall,
            pct(0.5),
            pct(0.99),
            syncs as f64 / commits as f64,
            rounds.map_or("null".into(), |r| r.to_string()),
            batches.map_or("null".into(), |b| b.to_string()),
            dir.display().to_string(),
        );
        drop(dbs);
        drop(log);
        std::fs::remove_dir_all(&root)?;
    }
    let _ = std::fs::remove_dir(&base);
    Ok(())
}
