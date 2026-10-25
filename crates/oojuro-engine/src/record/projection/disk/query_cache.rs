use super::*;
use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex},
};

pub(super) const RESERVED_BYTES: usize = 4 << 20;
const MAX_ENTRIES: usize = 128;
const MAX_INFLIGHT: usize = 32;
#[derive(Default)]
struct State {
    ready: BTreeMap<[u8; 32], (Arc<[Ranked]>, usize)>,
    order: VecDeque<[u8; 32]>,
    inflight: BTreeSet<[u8; 32]>,
    bytes: usize,
}
#[derive(Default)]
pub(super) struct QueryCache {
    state: Mutex<State>,
    changed: Condvar,
}
struct Claim<'a> {
    cache: &'a QueryCache,
    key: [u8; 32],
}
impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.cache.state.lock() {
            state.inflight.remove(&self.key);
        }
        self.cache.changed.notify_all();
    }
}
impl QueryCache {
    pub fn rank(
        &self,
        key: [u8; 32],
        compute: impl FnOnce() -> Result<Vec<Ranked>>,
    ) -> Result<Vec<Ranked>> {
        let mut compute = Some(compute);
        loop {
            let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
            if let Some((hits, _)) = state.ready.get(&key) {
                let hits = hits.clone();
                if let Some(at) = state.order.iter().position(|old| *old == key) {
                    state.order.remove(at);
                }
                state.order.push_back(key);
                drop(state);
                return Ok(hits.to_vec());
            }
            if state.inflight.contains(&key) {
                drop(self.changed.wait(state).map_err(|_| Error::Unavailable)?);
                continue;
            }
            if state.inflight.len() >= MAX_INFLIGHT {
                drop(state);
                return compute.take().unwrap()();
            }
            state.inflight.insert(key);
            drop(state);
            let _claim = Claim { cache: self, key };
            let found = compute.take().unwrap()()?;
            let bytes = found
                .iter()
                .try_fold(
                    256usize + found.len() * std::mem::size_of::<Ranked>(),
                    |bytes, hit| bytes.checked_add(hit.id.capacity()),
                )
                .ok_or(Error::Bounds)?;
            if bytes <= RESERVED_BYTES {
                let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
                while state.bytes + bytes > RESERVED_BYTES || state.ready.len() >= MAX_ENTRIES {
                    let oldest = state.order.pop_front().ok_or(Error::Unavailable)?;
                    if let Some((_, old_bytes)) = state.ready.remove(&oldest) {
                        state.bytes -= old_bytes;
                    }
                }
                state.bytes += bytes;
                state.order.push_back(key);
                state.ready.insert(key, (Arc::from(found.clone()), bytes));
            }
            return Ok(found);
        }
    }
}
pub(super) fn key(q: &Query<'_>) -> Result<[u8; 32]> {
    // Excerpt/output budgets do not affect ranking; every source original is
    // independently fetched and validated during every materialization.
    let encoded =
        serde_json::to_vec(&(q.text, q.k, q.from, q.to, &q.filter)).map_err(|_| Error::Invalid)?;
    Ok(*blake3::hash(&encoded).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            Barrier,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };
    #[test]
    fn simultaneous_identical_queries_compute_once_and_failure_releases_waiters() {
        let cache = Arc::new(QueryCache::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(10));
        let workers: Vec<_> = (0..10)
            .map(|_| {
                let (cache, calls, start) = (cache.clone(), calls.clone(), start.clone());
                thread::spawn(move || {
                    start.wait();
                    cache
                        .rank([1; 32], || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            thread::sleep(Duration::from_millis(10));
                            Ok(vec![Ranked {
                                id: "opaque".into(),
                                bits: 1.25,
                                continued: false,
                                ordinal: 7,
                            }])
                        })
                        .unwrap()
                })
            })
            .collect();
        for worker in workers {
            assert_eq!(worker.join().unwrap()[0].id, "opaque");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            cache.rank([2; 32], || Err(Error::Authority)),
            Err(Error::Authority)
        ));
        assert!(cache.rank([2; 32], || Ok(vec![])).unwrap().is_empty());
        for i in 0u32..2000 {
            let key = *blake3::hash(&i.to_be_bytes()).as_bytes();
            cache
                .rank(key, || {
                    Ok(vec![Ranked {
                        id: "x".repeat(1024),
                        bits: 0.,
                        continued: false,
                        ordinal: i,
                    }])
                })
                .unwrap();
        }
        let state = cache.state.lock().unwrap();
        assert!(
            state.bytes <= RESERVED_BYTES
                && state.ready.len() <= MAX_ENTRIES
                && state.inflight.is_empty()
        );
    }
}
