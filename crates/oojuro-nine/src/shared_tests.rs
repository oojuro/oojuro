use super::*;
use crate::{Nine, Options, Receipts};
use std::sync::atomic::AtomicUsize;

struct Scratch(PathBuf);
impl Scratch {
    fn under(base: PathBuf, tag: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let p = base.join(format!(
            "nine-shared-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    /// On disk under TMPDIR.
    fn new(tag: &str) -> Self {
        Self::under(std::env::temp_dir(), tag)
    }
    /// In RAM when available: crash simulations that open hundreds of logs
    /// test recovery logic, not the device, and should not load the disk.
    fn ram(tag: &str) -> Self {
        let shm = Path::new("/dev/shm");
        if shm.is_dir() && fs::metadata(shm).is_ok_and(|m| !m.permissions().readonly()) {
            Self::under(shm.into(), tag)
        } else {
            Self::new(tag)
        }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Test logs: 64 KiB files, so background spares stay small.
fn opts() -> SharedLogOptions {
    SharedLogOptions {
        segment_bytes: 64 << 10,
        ..SharedLogOptions::default()
    }
}

/// Wait until the background spare is ready.
fn spare_ready(log: &SharedLog) {
    let t = std::time::Instant::now();
    while relock(&log.spare).ready.is_none() {
        assert!(t.elapsed().as_secs() < 30, "spare never became ready");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn rows(store: u64, first: u64, n: u64) -> Vec<Vec<u8>> {
    (first..first + n)
        .map(|r| format!("s{store}r{r}-{}", "x".repeat((r as usize * 5) % 17)).into_bytes())
        .collect()
}

fn ticket(store: u64, first_row: u64, entries: &[Vec<u8>], max_frame: usize) -> Ticket {
    let (frames, starts) = encode_batch(store, first_row, entries, max_frame).unwrap();
    Ticket {
        store,
        first_row,
        rows: entries.len() as u64,
        frames,
        starts,
        slot: Arc::new(Slot {
            state: Mutex::new(SlotState::Waiting),
            cv: Condvar::new(),
        }),
    }
}

fn log_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("log-"))
        .collect();
    v.sort();
    v
}

/// Plaintext occurrences of `needle` in every file under `dir`.
fn grep(dir: &Path, needle: &[u8]) -> usize {
    let mut hits = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(data) = fs::read(&p) {
                hits += data.windows(needle.len()).filter(|w| *w == needle).count();
            }
        }
    }
    hits
}

#[test]
fn batches_of_many_stores_roundtrip_and_replay_after_reopen() -> Result<()> {
    let tmp = Scratch::ram("roundtrip");
    let dir = tmp.path("log");
    let mut want: HashMap<u64, Vec<Vec<u8>>> = HashMap::new();
    {
        let log = SharedLog::open(&dir, opts())?;
        for s in [1, 2, 7] {
            assert_eq!(log.attach(s, 0, true)?, (0, Vec::new()));
        }
        for (s, first, n) in [(1, 0, 3), (7, 0, 1), (1, 3, 2), (2, 0, 4), (7, 1, 0), (7, 1, 2)] {
            let e = rows(s, first, n);
            log.append(s, first, &e)?;
            want.entry(s).or_default().extend(e);
        }
        // Out-of-order rows and unattached stores are refused, not written.
        assert!(log.append(1, 9, &rows(1, 9, 1)).is_err());
        assert!(log.append(5, 0, &rows(5, 0, 1)).is_err());
        assert_eq!(log.stores(), vec![1, 2, 7]);
    }
    let log = SharedLog::open(&dir, opts())?;
    assert_eq!(log.stores(), vec![1, 2, 7]);
    for s in [1, 2, 7] {
        assert_eq!(log.attach(s, 0, false)?.1, want[&s], "store {s}");
    }
    // Replay starts at the store's durable rows, inside a batch if needed.
    log.detach(1);
    assert_eq!(log.attach(1, 2, false)?, (2, want[&1][2..].to_vec()));
    // An attached store continues where its rows end.
    log.append(1, 5, &rows(1, 5, 1))?;
    assert!(log.attach(2, 0, false).is_err(), "attached twice");
    Ok(())
}

#[test]
fn a_crash_at_every_byte_of_a_round_recovers_exactly_its_complete_batches() -> Result<()> {
    let tmp = Scratch::ram("cut");
    let dir = tmp.path("log");
    // Tiny frames: the round's batches of stores 1 and 2 span several frames.
    let small = SharedLogOptions {
        max_frame: 40,
        preallocate: false,
        ..opts()
    };
    let log = SharedLog::open(&dir, small.clone())?;
    for s in 1..=3 {
        log.attach(s, 0, true)?;
    }
    let mut committed_rows: HashMap<u64, Vec<Vec<u8>>> = HashMap::new();
    for (s, first, n) in [(1, 0, 3), (2, 0, 2)] {
        let e = rows(s, first, n);
        log.append(s, first, &e)?;
        committed_rows.entry(s).or_default().extend(e);
    }
    let path = log.files().last().unwrap().clone();
    let committed = fs::read(&path)?;
    // One round holding batches of three stores.
    let round = [(1u64, 3u64, rows(1, 3, 4)), (3, 0, rows(3, 0, 1)), (2, 2, rows(2, 2, 3))];
    let tickets: Vec<Ticket> = round.iter().map(|(s, f, e)| ticket(*s, *f, e, 40)).collect();
    assert!(log.write_round(&tickets).iter().all(Result::is_ok));
    assert_eq!(log.stats().rounds, 3);
    let full = fs::read(&path)?;
    let ends: Vec<u64> = round
        .iter()
        .map(|(s, f, _)| {
            let loc = log.locations(*s).into_iter().find(|l| l.first_row == *f).unwrap();
            loc.offset + loc.len
        })
        .collect();
    assert!(tickets[0].starts.len() > 1 && tickets[2].starts.len() > 1, "multi-frame batches");
    drop(log);
    let name = path.file_name().unwrap();
    let id = fs::read(dir.join(ID_FILE))?;
    let capacity = full.len() + 300;
    for (cut, padded) in (committed.len()..=full.len()).flat_map(|c| [(c, false), (c, true)]) {
        let case = tmp.path(&format!("cut-{cut}-{padded}"));
        fs::create_dir(&case)?;
        fs::write(case.join(ID_FILE), &id)?;
        let mut image = full[..cut].to_vec();
        if padded {
            image.resize(capacity, 0);
        }
        fs::write(case.join(name), &image)?;
        let log = SharedLog::open(&case, small.clone())?;
        let mut expect = committed_rows.clone();
        let mut boundary = committed.len() as u64;
        for ((s, _, e), end) in round.iter().zip(&ends) {
            if *end <= cut as u64 {
                expect.entry(*s).or_default().extend(e.iter().cloned());
                boundary = boundary.max(*end);
            }
        }
        for s in 1..=3 {
            let got = log.attach(s, 0, false)?.1;
            assert_eq!(got, expect.get(&s).cloned().unwrap_or_default(), "cut {cut} store {s}");
        }
        assert_eq!(fs::metadata(case.join(name))?.len(), boundary, "cut {cut}: truncated to a batch boundary");
        if cut == (committed.len() + full.len()) / 2 && !padded {
            // Appends continue after recovery and survive the next open.
            let next = expect.get(&3).map_or(0, |v| v.len() as u64);
            log.append(3, next, &rows(3, next, 2))?;
            drop(log);
            let log = SharedLog::open(&case, small.clone())?;
            assert_eq!(log.attach(3, 0, false)?.1.len() as u64, next + 2);
        }
        fs::remove_dir_all(&case)?;
    }
    Ok(())
}

#[test]
fn torn_tails_of_the_newest_file_are_truncated_and_committed_batches_kept() -> Result<()> {
    let tmp = Scratch::ram("torn");
    // Files that grow by appends; torn preallocated files are covered by the
    // padded cuts of the every-byte test.
    let opts = || SharedLogOptions { preallocate: false, ..opts() };
    // A frame that never completed: the start of a valid round, cut short.
    let partial = {
        let log = SharedLog::open(&tmp.path("donor"), opts())?;
        log.attach(1, 0, true)?;
        log.append(1, 0, &rows(1, 0, 2))?;
        let bytes = fs::read(log.files().last().unwrap())?;
        bytes[FILE_HEADER..FILE_HEADER + 30].to_vec()
    };
    for tail in [vec![1u8, 2, 3, 4, 5], vec![0u8; 300], partial] {
        let dir = tmp.path(&format!("log-{}", tail.len()));
        let path = {
            let log = SharedLog::open(&dir, opts())?;
            log.attach(1, 0, true)?;
            log.attach(2, 0, true)?;
            log.append(1, 0, &rows(1, 0, 3))?;
            log.append(2, 0, &rows(2, 0, 1))?;
            log.files().last().unwrap().clone()
        };
        let size = fs::metadata(&path)?.len();
        fs::OpenOptions::new().append(true).open(&path)?.write_all(&tail)?;
        let log = SharedLog::open(&dir, opts())?;
        assert_eq!(log.attach(1, 0, false)?.1, rows(1, 0, 3));
        assert_eq!(log.attach(2, 0, false)?.1, rows(2, 0, 1));
        assert_eq!(fs::metadata(&path)?.len(), size);
    }
    Ok(())
}

#[test]
fn damage_in_committed_history_fails_closed_and_changes_nothing() -> Result<()> {
    let tmp = Scratch::ram("damage");
    let dir = tmp.path("log");
    let path = {
        let log = SharedLog::open(&dir, opts())?;
        log.attach(4, 0, true)?;
        for i in 0..3 {
            log.append(4, i * 2, &rows(4, i * 2, 2))?;
        }
        log.files().last().unwrap().clone()
    };
    let good = fs::read(&path)?;
    let listing = log_files(&dir);
    // Every field of the first frame header, and its payload.
    let fh = FILE_HEADER;
    // len, len, flags, reserved, seq, round, store, first_row, payload CRC,
    // header CRC, payload.
    for offset in [fh, fh + 2, fh + 4, fh + 6, fh + 9, fh + 17, fh + 25, fh + 33, fh + 41, fh + 45, fh + FRAME_HEADER + 3] {
        let mut bad = good.clone();
        bad[offset] ^= 0x41;
        fs::write(&path, &bad)?;
        assert!(SharedLog::open(&dir, opts()).is_err(), "offset {offset}");
        assert_eq!(fs::read(&path)?, bad, "offset {offset}: file must be untouched");
        assert_eq!(log_files(&dir), listing, "offset {offset}: no file created");
    }
    // The file header and the log identity are checked too.
    let mut bad = good.clone();
    bad[12] ^= 1;
    fs::write(&path, &bad)?;
    assert!(SharedLog::open(&dir, opts()).is_err());
    fs::write(&path, &good)?;
    let id = fs::read(dir.join(ID_FILE))?;
    let mut bad_id = id.clone();
    bad_id[10] ^= 1;
    fs::write(dir.join(ID_FILE), &bad_id)?;
    assert!(SharedLog::open(&dir, opts()).is_err());
    fs::write(dir.join(ID_FILE), &id)?;
    // Damage to the final frame of the newest file is a torn write.
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 1;
    fs::write(&path, &bad)?;
    {
        let log = SharedLog::open(&dir, opts())?;
        assert_eq!(log.attach(4, 0, false)?.1, rows(4, 0, 4));
        log.append(4, 4, &rows(4, 4, 1))?;
    }
    // ... but the same damage in a sealed (older) file fails the open.
    let mut sealed = fs::read(&path)?;
    *sealed.last_mut().unwrap() ^= 1;
    fs::write(&path, &sealed)?;
    assert!(SharedLog::open(&dir, opts()).is_err());
    assert_eq!(fs::read(&path)?, sealed);
    Ok(())
}

#[test]
fn a_failed_write_poisons_the_log_until_reopen() -> Result<()> {
    let tmp = Scratch::ram("poison");
    let dir = tmp.path("log");
    {
        let log = SharedLog::open(&dir, opts())?;
        log.attach(1, 0, true)?;
        log.append(1, 0, &rows(1, 0, 2))?;
        log.fail_next_write(10);
        assert!(log.append(1, 2, &rows(1, 2, 2)).is_err());
        assert!(log.append(1, 2, &rows(1, 2, 2)).is_err());
        assert!(log.checkpoint().is_err() && log.scrub().is_err());
    }
    let log = SharedLog::open(&dir, opts())?;
    assert_eq!(log.attach(1, 0, false)?.1, rows(1, 0, 2));
    log.append(1, 2, &rows(1, 2, 1))?;
    Ok(())
}

#[test]
fn one_round_makes_the_batches_of_every_waiting_store_durable() -> Result<()> {
    let tmp = Scratch::ram("rounds");
    let log = SharedLog::open(&tmp.path("log"), opts())?;
    for s in 0..6 {
        log.attach(s, 0, true)?;
    }
    let wait = |what: &str, ok: &dyn Fn() -> bool| {
        let t = std::time::Instant::now();
        while !ok() {
            assert!(t.elapsed().as_secs() < 30, "timed out waiting for {what}");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    // Hold the log so the first writer leads a round that cannot finish.
    let hold = relock(&log.inner);
    let mut threads = Vec::new();
    let spawn = |s: u64| {
        let log = Arc::clone(&log);
        std::thread::spawn(move || log.append(s, 0, &rows(s, 0, 2)))
    };
    threads.push(spawn(0));
    wait("the leader to take its batch", &|| {
        let q = relock(&log.queue);
        q.leader && q.pending.is_empty()
    });
    for s in 1..6 {
        threads.push(spawn(s));
    }
    wait("five waiting batches", &|| relock(&log.queue).pending.len() == 5);
    drop(hold);
    for t in threads {
        t.join().unwrap()?;
    }
    let stats = log.stats();
    assert_eq!((stats.rounds, stats.batches), (2, 6), "six stores, two syncs");
    Ok(())
}

fn nine_options(log: &Arc<SharedLog>, store: u64, flush_text_bytes: usize) -> Options {
    Options {
        flush_text_bytes,
        flush_postings: usize::MAX,
        receipts: Receipts::Off,
        shared_log: Some(LogBinding {
            log: Arc::clone(log),
            store,
        }),
        ..Options::default()
    }
}

#[test]
fn stores_on_one_log_commit_concurrently_and_reopen_exactly() -> Result<()> {
    let tmp = Scratch::new("stores");
    let log_options = SharedLogOptions {
        segment_bytes: 16 << 10,
        ..opts()
    };
    let open = |log: &Arc<SharedLog>| -> Result<Vec<Arc<Nine>>> {
        (0..4)
            .map(|s| Ok(Arc::new(Nine::open_with(&tmp.path(&format!("s{s}")), nine_options(log, s, 1_000))?)))
            .collect()
    };
    let mut written: Vec<(usize, String, String)> = Vec::new();
    {
        let log = SharedLog::open(&tmp.path("log"), log_options.clone())?;
        let stores = open(&log)?;
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let db = Arc::clone(&stores[t % 4]);
                std::thread::spawn(move || -> Result<Vec<(usize, String, String)>> {
                    let mut out = Vec::new();
                    for i in 0..25 {
                        let token = format!("token{t}x{i}");
                        let texts = [format!("call {token} start"), format!("result {token} ok {}", "pad ".repeat(i % 9))];
                        let ids = db.put_batch(&[(i as i64, texts[0].as_str()), (i as i64, texts[1].as_str())])?;
                        assert_eq!(db.find(&token, i64::MIN, 1_000, 10)?.matches.len(), 2, "{token}");
                        for (id, text) in ids.into_iter().zip(texts) {
                            out.push((t % 4, id, text));
                        }
                    }
                    Ok(out)
                })
            })
            .collect();
        for t in threads {
            written.extend(t.join().unwrap()?);
        }
        let stats = log.stats();
        assert!(stats.batches >= 100 && stats.rounds <= stats.batches, "{stats:?}");
        for (s, db) in stores.iter().enumerate() {
            assert_eq!(db.len(), 50 * 2, "store {s}");
            assert!(db.files()?.iter().any(|p| p.extension().is_some_and(|e| e == "idx")), "flushed");
            assert!(!db.files()?.iter().any(|p| p.to_string_lossy().contains("wal-")), "no private journal");
        }
    }
    let log = SharedLog::open(&tmp.path("log"), log_options)?;
    let stores = open(&log)?;
    for (s, id, text) in &written {
        assert_eq!(stores[*s].read(id)?.as_deref(), Some(text.as_str()));
    }
    for t in 0..8 {
        assert_eq!(stores[t % 4].find(&format!("token{t}x7"), i64::MIN, 1_000, 10)?.matches.len(), 2);
    }
    // Once every store absorbs its rows the log shrinks to one empty file.
    for db in &stores {
        db.flush()?;
    }
    log.checkpoint()?;
    assert_eq!(log.files().len(), 1);
    assert_eq!(log.bytes(), FILE_HEADER as u64);
    Ok(())
}

#[test]
fn a_lagging_store_pins_log_files_until_it_absorbs_them() -> Result<()> {
    let tmp = Scratch::ram("pin");
    let log_options = SharedLogOptions {
        segment_bytes: 2048,
        ..opts()
    };
    let a_id;
    {
        let log = SharedLog::open(&tmp.path("log"), log_options.clone())?;
        let a = Nine::open_with(&tmp.path("a"), nine_options(&log, 1, usize::MAX))?;
        let b = Nine::open_with(&tmp.path("b"), nine_options(&log, 2, usize::MAX))?;
        a_id = a.put(1, "the lagging store wrote this once")?;
        for i in 0..60 {
            b.put(i, &format!("busy store row {i} {}", "y".repeat(40)))?;
        }
        b.flush()?;
        // B absorbed everything; A's one row keeps the oldest file.
        assert_eq!(log.pinning(), vec![1]);
        assert_eq!(log.files().len(), 2, "the pinned oldest file and the active one");
        drop((a, b));
        // Reopen while pinned: A replays its row, B replays nothing.
        drop(log);
        let log = SharedLog::open(&tmp.path("log"), log_options.clone())?;
        assert_eq!(log.stores(), vec![1, 2]);
        let a = Nine::open_with(&tmp.path("a"), nine_options(&log, 1, usize::MAX))?;
        let b = Nine::open_with(&tmp.path("b"), nine_options(&log, 2, usize::MAX))?;
        assert_eq!(a.read(&a_id)?.as_deref(), Some("the lagging store wrote this once"));
        assert_eq!(b.len(), 60);
        a.flush()?;
        assert!(log.pinning().is_empty());
        assert_eq!(log.files().len(), 1);
    }
    let log = SharedLog::open(&tmp.path("log"), log_options)?;
    assert!(log.stores().is_empty());
    let a = Nine::open_with(&tmp.path("a"), nine_options(&log, 1, usize::MAX))?;
    assert_eq!(a.read(&a_id)?.as_deref(), Some("the lagging store wrote this once"));
    Ok(())
}

#[test]
fn store_bindings_fail_closed() -> Result<()> {
    let tmp = Scratch::ram("bind");
    let log = SharedLog::open(&tmp.path("log"), opts())?;
    {
        let s = Nine::open_with(&tmp.path("shared"), nine_options(&log, 1, usize::MAX))?;
        s.put(1, "bound row")?;
        // The same store id cannot be attached twice.
        assert!(Nine::open_with(&tmp.path("other"), nine_options(&log, 1, usize::MAX)).is_err());
    }
    // A shared-log store needs its log; a private store cannot join one.
    assert!(Nine::open(&tmp.path("shared")).is_err());
    drop(Nine::open(&tmp.path("private"))?);
    assert!(Nine::open_with(&tmp.path("private"), nine_options(&log, 2, usize::MAX)).is_err());
    // Another log, or another store id, is refused.
    let other = SharedLog::open(&tmp.path("log2"), opts())?;
    assert!(Nine::open_with(&tmp.path("shared"), nine_options(&other, 1, usize::MAX)).is_err());
    assert!(Nine::open_with(&tmp.path("shared"), nine_options(&log, 3, usize::MAX)).is_err());
    // Deferred durability does not combine with a shared log.
    let mut deferred = nine_options(&log, 4, usize::MAX);
    deferred.deferred_sync = true;
    assert!(Nine::open_with(&tmp.path("deferred"), deferred).is_err());
    // A store directory that vanished while the log holds its rows.
    fs::remove_dir_all(tmp.path("shared"))?;
    assert!(Nine::open_with(&tmp.path("shared"), nine_options(&log, 1, usize::MAX)).is_err());
    Ok(())
}

#[test]
fn erasure_scrubs_the_shared_log_once_every_store_absorbs() -> Result<()> {
    let tmp = Scratch::new("erase");
    let needle = b"needle-qz7-erase-me";
    let log_dir = tmp.path("log");
    let (ids, other) = {
        let log = SharedLog::open(&log_dir, opts())?;
        let a = Nine::open_with(&tmp.path("a"), nine_options(&log, 1, usize::MAX))?;
        let b = Nine::open_with(&tmp.path("b"), nine_options(&log, 2, usize::MAX))?;
        // Rounds then stay in one zero-filled file (no rotation in between).
        spare_ready(&log);
        let ids = a.put_batch(&[(1, "alpha needle-qz7-erase-me secret"), (2, "alpha keep this")])?;
        let other = b.put(1, "beta lives on")?;
        assert!(grep(&log_dir, needle) > 0, "the log holds the text");
        a.erase_rows(&[0])?;
        assert_eq!(grep(&tmp.path("a"), needle), 0, "the store itself is clean");
        assert!(grep(&log_dir, needle) > 0, "B's unabsorbed rows keep the log file");
        // Scrubbing refuses while a store has unabsorbed rows.
        let before = log_files(&log_dir);
        assert!(log.scrub().is_err());
        assert_eq!(log_files(&log_dir), before);
        b.flush()?;
        log.scrub()?;
        assert_eq!(grep(&tmp.0, needle), 0, "erased text is in no file");
        assert_eq!(log_files(&log_dir).len(), 1);
        assert!(log_files(&log_dir).iter().all(|n| n.ends_with(".nine")));
        // Writes continue into the new file.
        b.put(2, "beta after scrub")?;
        (ids, other)
    };
    let log = SharedLog::open(&log_dir, opts())?;
    let a = Nine::open_with(&tmp.path("a"), nine_options(&log, 1, usize::MAX))?;
    let b = Nine::open_with(&tmp.path("b"), nine_options(&log, 2, usize::MAX))?;
    assert_eq!(a.read(&ids[0])?.as_deref(), Some(""));
    assert_eq!(a.read(&ids[1])?.as_deref(), Some("alpha keep this"));
    assert_eq!(b.read(&other)?.as_deref(), Some("beta lives on"));
    assert_eq!(b.len(), 2);
    assert_eq!(grep(&tmp.0, needle), 0);
    Ok(())
}

#[test]
fn an_interrupted_scrub_finishes_at_the_next_open() -> Result<()> {
    let tmp = Scratch::ram("dead");
    let dir = tmp.path("log");
    {
        let log = SharedLog::open(&dir, opts())?;
        log.attach(1, 0, true)?;
        log.append(1, 0, &[b"needle-dead-file".to_vec()])?;
        log.absorbed(1, 1);
        // Crash right after the rename of a scrub.
        let path = log.files().last().unwrap().clone();
        log.rotate(&mut relock(&log.inner))?;
        fs::rename(&path, path.with_extension("dead"))?;
    }
    assert_eq!(grep(&dir, b"needle-dead-file"), 1);
    let _log = SharedLog::open(&dir, opts())?;
    assert_eq!(grep(&dir, b"needle-dead-file"), 0);
    assert!(log_files(&dir).iter().all(|n| n.ends_with(".nine")));
    Ok(())
}

#[test]
fn snapshots_of_shared_log_stores_are_standalone() -> Result<()> {
    let tmp = Scratch::ram("snap");
    let log = SharedLog::open(&tmp.path("log"), opts())?;
    let db = Nine::open_with(&tmp.path("db"), nine_options(&log, 9, usize::MAX))?;
    let ids = db.put_batch(&[(1, "snapshot alpha"), (2, "snapshot beta")])?;
    db.snapshot(&tmp.path("copy"))?;
    db.put(3, "after the snapshot")?;
    let copy = Nine::open_with(&tmp.path("copy"), Options { receipts: Receipts::Off, ..Options::default() })?;
    assert_eq!(copy.len(), 2);
    assert_eq!(copy.read(&ids[1])?.as_deref(), Some("snapshot beta"));
    copy.put(4, "the copy has its own journal")?;
    Ok(())
}

#[test]
fn rounds_overwrite_zero_filled_files_whose_zero_tails_survive_reopen() -> Result<()> {
    let tmp = Scratch::ram("prealloc");
    let dir = tmp.path("log");
    let mut want = Vec::new();
    let sealed_path;
    {
        let log = SharedLog::open(&dir, opts())?;
        log.attach(1, 0, true)?;
        spare_ready(&log);
        // The first round moves from the growing file into the spare.
        let mut row = 0;
        let mut append = |n: u64, size: usize| -> Result<()> {
            let e: Vec<Vec<u8>> = (0..n).map(|i| vec![b'a' + ((row + i) % 26) as u8; size]).collect();
            log.append(1, row, &e)?;
            want.extend(e);
            row += n;
            Ok(())
        };
        append(2, 100)?;
        let first = log.files().last().unwrap().clone();
        assert_eq!(fs::metadata(&first)?.len(), 64 << 10, "zero-filled space");
        for _ in 0..20 {
            append(3, 200)?;
        }
        assert_eq!(fs::metadata(&first)?.len(), 64 << 10, "rounds overwrite, never append");
        // Overflow the file: the next round starts a new generation.
        spare_ready(&log);
        while log.files().last().unwrap() == &first {
            append(4, 1000)?;
        }
        sealed_path = first;
    }
    // The sealed file keeps its zero tail; the newest is truncated.
    let len = fs::metadata(&sealed_path)?.len();
    assert_eq!(len, 64 << 10);
    let log = SharedLog::open(&dir, opts())?;
    assert_eq!(log.attach(1, 0, false)?.1, want);
    drop(log);
    assert_eq!(fs::metadata(&sealed_path)?.len(), 64 << 10, "sealed files keep their zero tail");
    // A non-zero byte in a sealed file's zero tail fails the open.
    let mut bytes = fs::read(&sealed_path)?;
    *bytes.last_mut().unwrap() = 1;
    fs::write(&sealed_path, &bytes)?;
    assert!(SharedLog::open(&dir, opts()).is_err());
    Ok(())
}

#[test]
fn a_torn_round_whose_later_pages_persisted_is_dropped_but_synced_rounds_fail_closed() -> Result<()> {
    let tmp = Scratch::ram("order");
    let dir = tmp.path("log");
    let o = SharedLogOptions { preallocate: false, ..opts() };
    let log = SharedLog::open(&dir, o.clone())?;
    for s in 1..=3 {
        log.attach(s, 0, true)?;
    }
    log.append(1, 0, &rows(1, 0, 2))?;
    let path = log.files().last().unwrap().clone();
    let committed = fs::metadata(&path)?.len() as usize;
    // One round of three batches.
    let round = [(1u64, 2u64, rows(1, 2, 2)), (2, 0, rows(2, 0, 3)), (3, 0, rows(3, 0, 1))];
    let tickets: Vec<Ticket> = round.iter().map(|(s, f, e)| ticket(*s, *f, e, 1 << 20)).collect();
    assert!(log.write_round(&tickets).iter().all(Result::is_ok));
    drop(log);
    let full = fs::read(&path)?;
    let id = fs::read(dir.join(ID_FILE))?;
    let case = |name: &str, image: &[u8]| -> Result<PathBuf> {
        let d = tmp.path(name);
        fs::create_dir(&d)?;
        fs::write(d.join(ID_FILE), &id)?;
        fs::write(d.join(path.file_name().unwrap()), image)?;
        Ok(d)
    };
    // The round's first page never reached the disk, its later pages did:
    // the round was never synced, so it is dropped and the log opens.
    let mut lost_first = full.clone();
    lost_first[committed..committed + 60].fill(0);
    let d = case("lost-first", &lost_first)?;
    let log = SharedLog::open(&d, o.clone())?;
    assert_eq!(log.attach(1, 0, false)?.1, rows(1, 0, 2));
    assert!(log.attach(2, 0, false)?.1.is_empty() && log.attach(3, 0, false)?.1.is_empty());
    drop(log);
    // A hole in the middle of the round keeps the batches before it.
    let mut lost_middle = full.clone();
    let mid = committed + tickets[0].frames.len() + 10;
    lost_middle[mid..mid + 40].fill(0);
    let d = case("lost-middle", &lost_middle)?;
    let log = SharedLog::open(&d, o.clone())?;
    assert_eq!(log.attach(1, 0, false)?.1, rows(1, 0, 4));
    assert!(log.attach(2, 0, false)?.1.is_empty() && log.attach(3, 0, false)?.1.is_empty());
    drop(log);
    // The same hole in a synced round followed by a later round: corruption.
    let mut synced = full.clone();
    synced[FILE_HEADER + 10..FILE_HEADER + 50].fill(0);
    let d = case("synced", &synced)?;
    assert!(SharedLog::open(&d, o).is_err());
    assert_eq!(fs::read(d.join(path.file_name().unwrap()))?, synced, "untouched");
    Ok(())
}

#[test]
fn one_store_pipelines_several_batches_into_one_round() -> Result<()> {
    let tmp = Scratch::ram("pipeline");
    let log = SharedLog::open(&tmp.path("log"), opts())?;
    let db = Arc::new(Nine::open_with(&tmp.path("db"), nine_options(&log, 1, usize::MAX))?);
    let wait = |what: &str, ok: &dyn Fn() -> bool| {
        let t = std::time::Instant::now();
        while !ok() {
            assert!(t.elapsed().as_secs() < 30, "timed out waiting for {what}");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    let before = log.stats();
    let hold = relock(&log.inner);
    let insert = |id: &'static str| {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.insert_batch(vec![oojuro_core::Record::new(id, id, &format!("text of {id}"), 1)]))
    };
    let first = insert("a");
    wait("the leader to take its batch", &|| {
        let q = relock(&log.queue);
        q.leader && q.pending.is_empty()
    });
    // The store's later batches queue while its first one is in flight.
    let rest: Vec<_> = ["b", "c", "d"].into_iter().map(insert).collect();
    wait("three queued batches of the same store", &|| relock(&log.queue).pending.len() == 3);
    // An id still in flight is a duplicate.
    assert!(db.insert(oojuro_core::Record::new("c", "c", "again", 1)).is_err());
    drop(hold);
    first.join().unwrap()?;
    for t in rest {
        t.join().unwrap()?;
    }
    let stats = log.stats();
    assert_eq!((stats.rounds - before.rounds, stats.batches - before.batches), (2, 4));
    assert_eq!(db.len(), 4);
    // Rows follow the order batches took them (b, c, d raced for it).
    assert_eq!(db.record_at(0)?.id, "a");
    let mut later: Vec<String> = (1..4).map(|r| db.record_at(r).map(|x| x.id)).collect::<Result<_>>()?;
    later.sort();
    assert_eq!(later, ["b", "c", "d"]);
    assert_eq!(db.find("text", i64::MIN, 10, 10)?.matches.len(), 4);
    drop(db);
    drop(log);
    let log = SharedLog::open(&tmp.path("log"), opts())?;
    let db = Nine::open_with(&tmp.path("db"), nine_options(&log, 1, usize::MAX))?;
    assert_eq!(db.read("d")?.as_deref(), Some("text of d"));
    assert!(db.insert(oojuro_core::Record::new("c", "c", "again", 1)).is_err());
    Ok(())
}

#[test]
fn a_failed_round_fails_every_pipelined_batch_of_the_store_without_hanging() -> Result<()> {
    let tmp = Scratch::ram("pipefail");
    let log = SharedLog::open(&tmp.path("log"), opts())?;
    let db = Arc::new(Nine::open_with(&tmp.path("db"), nine_options(&log, 1, usize::MAX))?);
    db.put(1, "committed before the failure")?;
    let wait = |ok: &dyn Fn() -> bool| {
        let t = std::time::Instant::now();
        while !ok() {
            assert!(t.elapsed().as_secs() < 30, "timed out");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    let mut hold = relock(&log.inner);
    let put = |text: &'static str| {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.put(2, text))
    };
    let first = put("first in flight");
    wait(&|| {
        let q = relock(&log.queue);
        q.leader && q.pending.is_empty()
    });
    let later: Vec<_> = ["second", "third"].into_iter().map(put).collect();
    wait(&|| relock(&log.queue).pending.len() == 2);
    // Armed while the log is held: the very next round (the first batch's)
    // is the torn one.
    hold.fail_after_bytes = Some(5);
    drop(hold);
    // The first round is torn: every batch of the store fails, none hangs.
    assert!(first.join().unwrap().is_err());
    for t in later {
        assert!(t.join().unwrap().is_err());
    }
    assert!(db.put(3, "refused until reopen").is_err());
    drop(db);
    drop(log);
    let log = SharedLog::open(&tmp.path("log"), opts())?;
    let db = Nine::open_with(&tmp.path("db"), nine_options(&log, 1, usize::MAX))?;
    assert_eq!(db.len(), 1);
    db.put(4, "writes resume after reopen")?;
    Ok(())
}

#[test]
fn scrub_retires_untracked_log_files_through_dead_names() -> Result<()> {
    let tmp = Scratch::ram("leftover");
    let dir = tmp.path("log");
    let log = SharedLog::open(&dir, opts())?;
    log.attach(1, 0, true)?;
    log.append(1, 0, &[b"tracked needle-left-1".to_vec()])?;
    log.absorbed(1, 1);
    // A file whose retirement failed to remove it: no longer tracked.
    fs::write(dir.join(file_name(77)), b"stale needle-left-2")?;
    fs::write(dir.join("log-00000078.dead"), b"half-zeroed needle-left-3")?;
    log.scrub()?;
    assert_eq!(grep(&dir, b"needle-left"), 0);
    assert!(log_files(&dir).iter().all(|n| n.ends_with(".nine")));
    assert_eq!(log_files(&dir).len(), 1);
    Ok(())
}
