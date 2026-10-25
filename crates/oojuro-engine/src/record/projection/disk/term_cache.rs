use super::*;
use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex},
};

pub(super) const RESERVED_BYTES: usize = 12 << 20;
pub(super) const GROUP_TOP: usize = 32;
const GROUP_BYTES: usize = 2 << 20;
const MAX_GROUPS: usize = 1024;
const MAX_ENTRIES: usize = 128;
#[derive(Clone, Copy, Default)]
pub(super) struct Statistics {
    pub df: u64,
    pub scope: u64,
}
pub(super) struct Group {
    pub shape: (u32, u32, u32),
    pub top: Vec<GroupHit>,
}
pub(super) struct TermList {
    pub pairs: Pairs,
    pub statistics: Statistics,
    pub groups: Option<Vec<Group>>,
    bytes: usize,
}
pub(super) enum Pairs {
    Plain(Vec<(u32, u32)>),
    Compact {
        blocks: Vec<compact::TfBlock>,
        count: usize,
    },
}
impl Pairs {
    pub fn frequency(&self, mask: &[u64]) -> Result<u64> {
        let mut total = 0u64;
        match self {
            Self::Plain(pairs) => {
                for &(row, tf) in pairs {
                    let word = mask.get(row as usize / 64).ok_or(Error::Authority)?;
                    if word & (1 << (row % 64)) != 0 {
                        total = total.checked_add(u64::from(tf)).ok_or(Error::Bounds)?;
                    }
                }
            }
            Self::Compact { blocks, .. } => {
                for block in blocks {
                    let begin = block.base() as usize / 64;
                    if begin >= mask.len() {
                        return Err(Error::Authority);
                    }
                    let selected =
                        std::array::from_fn(|at| mask.get(begin + at).copied().unwrap_or(0));
                    total = total
                        .checked_add(block.frequency(selected))
                        .ok_or(Error::Bounds)?;
                }
            }
        }
        Ok(total)
    }
    fn new(compact: bool, upper: usize) -> Self {
        if compact {
            Self::Compact {
                blocks: Vec::with_capacity(upper),
                count: 0,
            }
        } else {
            Self::Plain(Vec::with_capacity(upper))
        }
    }
    fn bytes(&self) -> usize {
        match self {
            Self::Plain(pairs) => pairs.capacity() * 8,
            Self::Compact { blocks, .. } => {
                blocks.capacity() * std::mem::size_of::<compact::TfBlock>()
                    + blocks.iter().map(compact::TfBlock::bytes).sum::<usize>()
            }
        }
    }
    fn len(&self) -> usize {
        match self {
            Self::Plain(pairs) => pairs.len(),
            Self::Compact { count, .. } => *count,
        }
    }
    pub fn next(&self, block: &mut usize, at: &mut usize) -> Option<(u32, u32)> {
        match self {
            Self::Plain(pairs) => {
                let pair = pairs.get(*at).copied();
                *at += usize::from(pair.is_some());
                pair
            }
            Self::Compact { blocks, .. } => loop {
                let current = blocks.get(*block)?;
                if let Some(pair) = current.next(*at) {
                    *at = (pair.0 - current.base()) as usize + 1;
                    return Some(pair);
                }
                *block += 1;
                *at = 0;
            },
        }
    }
    pub fn at_least(&self, block: &mut usize, at: &mut usize, wanted: u32) -> Option<(u32, u32)> {
        match self {
            Self::Plain(pairs) => *at += pairs[*at..].partition_point(|(row, _)| *row < wanted),
            Self::Compact { blocks, .. } => {
                let skip = blocks[*block..]
                    .partition_point(|block| u64::from(block.base()) + 256 <= u64::from(wanted));
                *block += skip;
                if skip != 0 {
                    *at = 0;
                }
                if let Some(current) = blocks.get(*block) {
                    *at = (*at).max(wanted.saturating_sub(current.base()) as usize);
                }
            }
        }
        self.next(block, at)
    }
    pub fn maximum(&self, base: u32) -> Option<u32> {
        let Self::Compact { blocks, .. } = self else {
            return None;
        };
        Some(
            blocks
                .binary_search_by_key(&base, compact::TfBlock::base)
                .ok()
                .map_or(0, |at| blocks[at].maximum()),
        )
    }
    pub fn maximum_scope_mass(&self, base: u32) -> Option<f64> {
        let Self::Compact { blocks, .. } = self else {
            return None;
        };
        Some(
            blocks
                .binary_search_by_key(&base, compact::TfBlock::base)
                .ok()
                .map_or(0., |at| blocks[at].maximum_scope_mass()),
        )
    }
    pub fn tf(&self, row: u32) -> Option<u32> {
        let Self::Compact { blocks, .. } = self else {
            return None;
        };
        Some(
            blocks
                .binary_search_by_key(&(row & !255), compact::TfBlock::base)
                .ok()
                .map_or(0, |at| blocks[at].tf(row as u8)),
        )
    }
    pub fn presence(
        &self,
        union: &mut [u64],
        live: &[u64],
        selected: Option<&[u64]>,
    ) -> Result<u64> {
        if union.len() != live.len() || selected.is_some_and(|bits| bits.len() != live.len()) {
            return Err(Error::Authority);
        }
        let mut count = 0u64;
        let mut add = |word: usize, mask: u64| -> Result<()> {
            let active = mask
                & *live.get(word).ok_or(Error::Authority)?
                & selected.map_or(u64::MAX, |bits| bits[word]);
            union[word] |= active;
            count = count
                .checked_add(u64::from(active.count_ones()))
                .ok_or(Error::Bounds)?;
            Ok(())
        };
        match self {
            Self::Plain(pairs) => {
                for &(row, _) in pairs {
                    add(row as usize / 64, 1 << (row % 64))?;
                }
            }
            Self::Compact { blocks, .. } => {
                for block in blocks {
                    for (offset, mask) in block.presence().into_iter().enumerate() {
                        if mask != 0 {
                            add(block.base() as usize / 64 + offset, mask)?;
                        }
                    }
                }
            }
        }
        Ok(count)
    }
    pub fn iter(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let (mut block, mut at) = (0, 0);
        std::iter::from_fn(move || self.next(&mut block, &mut at))
    }
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct GroupHit {
    pub ordinal: u32,
    pub time: i64,
    pub label: u128,
}
impl Ord for GroupHit {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .time
            .cmp(&self.time)
            .then_with(|| self.label.cmp(&other.label))
    }
}
impl PartialOrd for GroupHit {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
    kind: u8,
    hash: [u8; 32],
}
#[derive(Clone)]
enum Entry {
    Postings(Arc<TermList>),
    Selection(Arc<Selected>),
    Literal(Arc<literal::Proof>),
    Joint(Arc<joint::Groups>),
}
impl Entry {
    fn bytes(&self) -> usize {
        match self {
            Self::Postings(list) => list.bytes,
            Self::Selection(view) => view.bits.capacity() * 8 + 512,
            Self::Literal(proof) => proof.bytes(),
            Self::Joint(groups) => groups.bytes(),
        }
    }
    fn idle(&self) -> bool {
        match self {
            Self::Postings(list) => Arc::strong_count(list) == 1,
            Self::Selection(view) => Arc::strong_count(view) == 1,
            Self::Literal(proof) => Arc::strong_count(proof) == 1,
            Self::Joint(groups) => Arc::strong_count(groups) == 1,
        }
    }
}
#[derive(Default)]
struct State {
    ready: BTreeMap<CacheKey, Entry>,
    order: VecDeque<CacheKey>,
    inflight: BTreeSet<CacheKey>,
    used: usize,
}
pub(super) struct TermCache {
    state: Mutex<State>,
    changed: Condvar,
    capacity: usize,
}
impl Default for TermCache {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            capacity: RESERVED_BYTES,
        }
    }
}
struct Claim<'a> {
    cache: &'a TermCache,
    key: CacheKey,
    reserved: usize,
    active: bool,
}
impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if self.active
            && let Ok(mut state) = self.cache.state.lock()
        {
            state.inflight.remove(&self.key);
            state.used -= self.reserved;
        }
        self.cache.changed.notify_all();
    }
}
impl TermCache {
    pub fn joint(&self, index: &Index, plan: &scorer::Plan) -> Result<Option<Arc<joint::Groups>>> {
        if !plan.scope_model() || plan.terms.len() > 16 {
            return Err(Error::Invalid);
        }
        let entry = self.entry(
            CacheKey {
                kind: 2,
                hash: joint::key(plan),
            },
            || {
                let bytes = joint::Groups::estimate(index, plan);
                Ok((bytes, bytes))
            },
            |budget| {
                Ok(Entry::Joint(Arc::new(joint::Groups::build(
                    index, plan, budget, None,
                )?)))
            },
        )?;
        match entry {
            Some(Entry::Joint(groups)) => Ok(Some(groups)),
            None => Ok(None),
            _ => Err(Error::Authority),
        }
    }
    pub fn literal_groups(
        &self,
        index: &Index,
        plan: &scorer::Plan,
        literal: &[u64],
    ) -> Result<Option<Arc<joint::Groups>>> {
        if literal.len() != (index.manifest.rows as usize).div_ceil(64) {
            return Err(Error::Authority);
        }
        let Some(needle) = plan
            .literal
            .as_ref()
            .filter(|_| !plan.scope_model() && plan.terms.len() <= 16)
        else {
            return Err(Error::Invalid);
        };
        let mut key = blake3::Hasher::new_derive_key("oojuro exact literal numeric groups v1");
        key.update(&joint::key(plan));
        key.update(needle.as_bytes());
        let entry = self.entry(
            CacheKey {
                kind: 4,
                hash: *key.finalize().as_bytes(),
            },
            || {
                let bytes = joint::Groups::estimate(index, plan);
                Ok((bytes, bytes))
            },
            |budget| {
                Ok(Entry::Joint(Arc::new(joint::Groups::build(
                    index,
                    plan,
                    budget,
                    Some(literal),
                )?)))
            },
        )?;
        match entry {
            Some(Entry::Joint(groups)) => Ok(Some(groups)),
            None => Ok(None),
            _ => Err(Error::Authority),
        }
    }
    pub fn literal(&self, index: &Index, needle: &str) -> Result<Option<Arc<literal::Proof>>> {
        if index.manifest.positions.is_none() {
            return Ok(None);
        }
        let mut key = blake3::Hasher::new_derive_key("oojuro exact raw literal proof v1");
        key.update(needle.as_bytes());
        let entry = self.entry(
            CacheKey {
                kind: 3,
                hash: *key.finalize().as_bytes(),
            },
            || {
                let bytes = literal::Proof::estimate(index.manifest.rows)?;
                Ok((bytes, bytes))
            },
            |_| {
                Ok(Entry::Literal(Arc::new(literal::Proof {
                    bits: index.literal_candidates_uncached(needle)?,
                })))
            },
        )?;
        match entry {
            Some(Entry::Literal(proof)) => Ok(Some(proof)),
            None => Ok(None),
            _ => Err(Error::Authority),
        }
    }
    fn entry(
        &self,
        key: CacheKey,
        estimate: impl FnOnce() -> Result<(usize, usize)>,
        load: impl FnOnce(usize) -> Result<Entry>,
    ) -> Result<Option<Entry>> {
        let mut estimate = Some(estimate);
        let mut estimated = None;
        let mut load = Some(load);
        loop {
            let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
            if let Some(entry) = state.ready.get(&key) {
                let entry = entry.clone();
                if let Some(at) = state.order.iter().position(|old| *old == key) {
                    state.order.remove(at);
                }
                state.order.push_back(key);
                return Ok(Some(entry));
            }
            if state.inflight.contains(&key) {
                drop(self.changed.wait(state).map_err(|_| Error::Unavailable)?);
                continue;
            }
            drop(state);
            if estimated.is_none() {
                estimated = Some(estimate.take().unwrap()()?);
            }
            let (reserved, argument) = estimated.unwrap();
            if reserved > self.capacity {
                return Ok(None);
            }
            let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
            if state.ready.contains_key(&key) || state.inflight.contains(&key) {
                drop(state);
                continue;
            }
            while state.used + reserved > self.capacity
                || state.ready.len() + state.inflight.len() >= MAX_ENTRIES
            {
                let Some(at) = state
                    .order
                    .iter()
                    .position(|key| state.ready.get(key).is_some_and(Entry::idle))
                else {
                    return Ok(None);
                };
                let old = state.order.remove(at).unwrap();
                if let Some(entry) = state.ready.remove(&old) {
                    state.used -= entry.bytes();
                }
            }
            state.used += reserved;
            state.inflight.insert(key);
            drop(state);
            let mut claim = Claim {
                cache: self,
                key,
                reserved,
                active: true,
            };
            let entry = load.take().unwrap()(argument)?;
            if entry.bytes() > reserved {
                return Err(Error::Bounds);
            }
            let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
            state.inflight.remove(&key);
            state.used -= reserved;
            state.used += entry.bytes();
            state.order.push_back(key);
            state.ready.insert(key, entry.clone());
            claim.active = false;
            drop(state);
            self.changed.notify_all();
            return Ok(Some(entry));
        }
    }
    pub fn get(&self, index: &Index, term: [u8; 32]) -> Result<Option<Arc<TermList>>> {
        let entry = self.entry(
            CacheKey {
                kind: 0,
                hash: term,
            },
            || {
                let chunks = tree::count_prefix(
                    index.context(tree::Purpose::ProjectionPostings),
                    &index.manifest.postings,
                    &term,
                )?;
                if compact_format(index.manifest.format) {
                    let upper = usize::try_from(chunks).map_err(|_| Error::Bounds)?;
                    let reserved = upper
                        .checked_mul(std::mem::size_of::<compact::TfBlock>() + 1283)
                        .and_then(|bytes| bytes.checked_add(GROUP_BYTES + 512))
                        .ok_or(Error::Bounds)?;
                    return Ok((reserved, upper));
                }
                let upper = chunks.saturating_mul(256).min(index.manifest.rows) as usize;
                let reserved = upper
                    .checked_mul(8)
                    .and_then(|n| n.checked_add(GROUP_BYTES + 512))
                    .ok_or(Error::Bounds)?;
                Ok((reserved, upper))
            },
            |upper| Ok(Entry::Postings(Arc::new(load(index, term, upper)?))),
        )?;
        match entry {
            Some(Entry::Postings(list)) => Ok(Some(list)),
            None => Ok(None),
            _ => Err(Error::Authority),
        }
    }
    pub fn selection(
        &self,
        index: &Index,
        q: &Query<'_>,
        selection: &Selection,
    ) -> Result<Option<Arc<Selected>>> {
        let bytes = serde_json::to_vec(&(q.from, q.to, &q.filter)).map_err(|_| Error::Invalid)?;
        let key = CacheKey {
            kind: 1,
            hash: *blake3::hash(&bytes).as_bytes(),
        };
        let entry = self.entry(
            key,
            || Ok((((index.manifest.rows as usize).div_ceil(64)) * 8 + 512, 0)),
            |_| Ok(Entry::Selection(Arc::new(index.select(selection)?))),
        )?;
        match entry {
            Some(Entry::Selection(view)) => Ok(Some(view)),
            None => Ok(None),
            _ => Err(Error::Authority),
        }
    }
}
fn load(index: &Index, term: [u8; 32], upper: usize) -> Result<TermList> {
    let mut input = DiskPostings::new_hash(index, term)?;
    let mut pairs = Pairs::new(compact_format(index.manifest.format), upper);
    let mut pending = Vec::with_capacity(256);
    let mut maximum_scope_mass = 0.0f64;
    let mut statistics = Statistics::default();
    let mut groups = Some(BTreeMap::<(u32, u32, u32), BinaryHeap<GroupHit>>::new());
    while let Some((ordinal, tf)) = input.next()? {
        if !compact_format(index.manifest.format) && pairs.len() == upper {
            return Err(Error::Authority);
        }
        let column = index.column(ordinal)?;
        if !column.indexed() {
            return Err(Error::Authority);
        }
        if column.live() {
            statistics.df = statistics.df.checked_add(1).ok_or(Error::Bounds)?;
            statistics.scope = statistics
                .scope
                .checked_add(u64::from(column.scope))
                .ok_or(Error::Bounds)?;
            let shape = (column.length, column.scope, tf);
            if groups
                .as_ref()
                .is_some_and(|groups| groups.len() >= MAX_GROUPS && !groups.contains_key(&shape))
            {
                groups = None;
            }
            if let Some(groups) = &mut groups {
                let top = groups.entry(shape).or_default();
                let row = GroupHit {
                    ordinal,
                    time: column.valid_at,
                    label: column.label,
                };
                if top.len() < GROUP_TOP {
                    top.push(row);
                } else if top.peek().is_some_and(|worst| row < *worst) {
                    top.pop();
                    top.push(row);
                }
            }
        }
        match &mut pairs {
            Pairs::Plain(pairs) => pairs.push((ordinal, tf)),
            Pairs::Compact { blocks, count } => {
                if pending
                    .first()
                    .is_some_and(|(row, _)| row / 256 != ordinal / 256)
                    || pending.len() == 256
                {
                    let base = pending[0].0 & !255;
                    blocks.push(compact::TfBlock::decode(
                        base,
                        index.manifest.rows,
                        compact::TfBlock::encode(base, &pending)?,
                    )?);
                    blocks
                        .last_mut()
                        .unwrap()
                        .set_scope_mass(maximum_scope_mass);
                    pending.clear();
                    maximum_scope_mass = 0.;
                }
                if column.live() {
                    if column.length == 0 || column.scope == 0 {
                        return Err(Error::Authority);
                    }
                    maximum_scope_mass = maximum_scope_mass
                        .max(tf as f64 * column.scope as f64 / column.length as f64);
                }
                pending.push((ordinal, tf));
                *count += 1;
            }
        }
    }
    if let Pairs::Compact { blocks, .. } = &mut pairs
        && !pending.is_empty()
    {
        let base = pending[0].0 & !255;
        blocks.push(compact::TfBlock::decode(
            base,
            index.manifest.rows,
            compact::TfBlock::encode(base, &pending)?,
        )?);
        blocks
            .last_mut()
            .unwrap()
            .set_scope_mass(maximum_scope_mass);
    }
    let groups = groups.map(|groups| {
        groups
            .into_iter()
            .map(|(shape, top)| Group {
                shape,
                top: top.into_vec(),
            })
            .collect::<Vec<_>>()
    });
    let bytes = pairs.bytes()
        + 512
        + groups.as_ref().map_or(0, |groups| {
            groups.capacity() * std::mem::size_of::<Group>()
                + groups
                    .iter()
                    .map(|group| group.top.capacity() * std::mem::size_of::<GroupHit>())
                    .sum::<usize>()
        });
    Ok(TermList {
        pairs,
        statistics,
        groups,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn compact_pair_seeks_match_sorted_reference_across_block_jumps_and_end() {
        let rows: Vec<_> = (0..4096u32)
            .filter(|n| n % 7 != 0)
            .map(|n| (n, 1 + n % 17))
            .collect();
        let mut blocks = Vec::new();
        for base in (0..4096u32).step_by(256) {
            let pairs: Vec<_> = rows
                .iter()
                .copied()
                .filter(|(row, _)| *row >= base && *row < base + 256)
                .collect();
            blocks.push(
                compact::TfBlock::decode(
                    base,
                    4096,
                    compact::TfBlock::encode(base, &pairs).unwrap(),
                )
                .unwrap(),
            );
        }
        let pairs = Pairs::Compact {
            blocks,
            count: rows.len(),
        };
        assert_eq!(pairs.iter().collect::<Vec<_>>(), rows);
        for step in [1, 3, 64, 257, 513, 4096] {
            let (mut block, mut at) = (0, 0);
            let mut previous = None;
            for wanted in (0..5000).step_by(step) {
                let expected = rows.iter().copied().find(|(row, _)| {
                    *row >= wanted && previous.is_none_or(|previous| *row > previous)
                });
                let found = pairs.at_least(&mut block, &mut at, wanted);
                assert_eq!(found, expected, "step={step} wanted={wanted}");
                if let Some((row, _)) = found {
                    previous = Some(row);
                }
            }
        }
    }
    #[test]
    fn active_posting_leases_stay_charged_until_the_last_reader_releases_them() {
        let root = Scratch(std::env::temp_dir().join(format!(
            "term-cache-{}",
            hex(&oojuro_store::crypto::random_key())
        )));
        fs::create_dir(&root.0).unwrap();
        let binding = Binding {
            namespace: [1; 32],
            writer: "numeric-fixture".into(),
            view: [2; 32],
        };
        let mut builder =
            Builder::create(root.0.join("index"), &[9; 32], binding, Limits::default()).unwrap();
        for i in 0..128 {
            builder
                .push(Row {
                    id: format!("{i:08x}"),
                    key: None,
                    text: "alpha beta".into(),
                    valid_at: i,
                    reported_recorded_at: i,
                    kind: Kind::Raw,
                    tool: None,
                    status: None,
                    agent_id: String::new(),
                    indexed: true,
                })
                .unwrap();
        }
        let mut index = builder.finish().unwrap();
        index.terms.capacity = GROUP_BYTES + 2048;
        let alpha = *blake3::hash(b"alpha").as_bytes();
        let beta = *blake3::hash(b"beta").as_bytes();
        let lease = index.terms.get(&index, alpha).unwrap().unwrap();
        let charged = index.terms.state.lock().unwrap().used;
        assert!(index.terms.get(&index, beta).unwrap().is_none());
        assert_eq!(index.terms.state.lock().unwrap().used, charged);
        assert_eq!(lease.pairs.len(), 128);
        drop(lease);
        assert!(index.terms.get(&index, beta).unwrap().is_some());
        let state = index.terms.state.lock().unwrap();
        assert!(!state.ready.contains_key(&CacheKey {
            kind: 0,
            hash: alpha
        }));
        assert!(state.used <= index.terms.capacity && state.inflight.is_empty());
    }
    #[test]
    fn literal_proof_singleflight_leases_and_admission_share_the_existing_policy() {
        let root = Scratch(std::env::temp_dir().join(format!(
            "literal-cache-{}",
            hex(&oojuro_store::crypto::random_key())
        )));
        fs::create_dir(&root.0).unwrap();
        let binding = Binding {
            namespace: [1; 32],
            writer: "literal-fixture".into(),
            view: [2; 32],
        };
        let mut builder =
            Builder::create_positional(root.0.join("index"), &[9; 32], binding, Limits::default())
                .unwrap();
        for i in 0..128 {
            builder
                .push(Row {
                    id: format!("{i:08x}"),
                    key: None,
                    text: "alpha beta".into(),
                    valid_at: i,
                    reported_recorded_at: i,
                    kind: Kind::Raw,
                    tool: None,
                    status: None,
                    agent_id: String::new(),
                    indexed: true,
                })
                .unwrap();
        }
        let mut index = builder.finish().unwrap();
        index.terms.capacity = literal::Proof::estimate(index.manifest.rows).unwrap();
        let index = Arc::new(index);
        let barrier = Arc::new(std::sync::Barrier::new(10));
        let leases: Vec<_> = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..10)
                .map(|_| {
                    let index = index.clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        index.terms.literal(&index, "alpha beta").unwrap().unwrap()
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect()
        });
        let lease = leases[0].clone();
        assert!(leases.iter().all(|other| Arc::ptr_eq(other, &lease)));
        drop(leases);
        assert_eq!(lease.bits.as_deref(), Some([u64::MAX; 2].as_slice()));
        let mut copy = index.literal_candidates("alpha beta").unwrap().unwrap();
        copy.fill(0);
        assert_eq!(lease.bits.as_deref(), Some([u64::MAX; 2].as_slice()));
        let charged = index.terms.state.lock().unwrap().used;
        assert_eq!(charged, lease.bytes());
        assert!(index.terms.literal(&index, "beta alpha").unwrap().is_none());
        assert_eq!(
            index.literal_candidates("beta alpha").unwrap(),
            Some(vec![0; 2])
        );
        assert_eq!(index.terms.state.lock().unwrap().used, charged);
        drop(lease);
        let replacement = index.terms.literal(&index, "beta alpha").unwrap().unwrap();
        assert_eq!(replacement.bits.as_deref(), Some([0; 2].as_slice()));
        let state = index.terms.state.lock().unwrap();
        assert_eq!(state.ready.len(), 1);
        assert!(state.used <= index.terms.capacity && state.inflight.is_empty());
    }
    #[test]
    fn joint_shape_leases_share_budget_and_rejected_proofs_remain_incomplete() {
        let root = Scratch(std::env::temp_dir().join(format!(
            "joint-cache-{}",
            hex(&oojuro_store::crypto::random_key())
        )));
        fs::create_dir(&root.0).unwrap();
        let binding = Binding {
            namespace: [1; 32],
            writer: "numeric-fixture".into(),
            view: [2; 32],
        };
        let mut builder =
            Builder::create_compact(root.0.join("index"), &[9; 32], binding, Limits::default())
                .unwrap();
        for i in 0..128 {
            builder
                .push(Row {
                    id: format!("{i:08x}"),
                    key: None,
                    text: "alpha beta".into(),
                    valid_at: i,
                    reported_recorded_at: i,
                    kind: Kind::Raw,
                    tool: None,
                    status: None,
                    agent_id: String::new(),
                    indexed: true,
                })
                .unwrap();
        }
        let mut index = builder.finish().unwrap();
        let alpha = scorer::Plan::new("alpha", false, None, &BTreeSet::new(), &BTreeMap::new());
        let beta = scorer::Plan::new("beta", false, None, &BTreeSet::new(), &BTreeMap::new());
        let literal =
            scorer::Plan::new("\"alpha\"", false, None, &BTreeSet::new(), &BTreeMap::new());
        assert!(matches!(
            index.terms.joint(&index, &literal),
            Err(Error::Invalid)
        ));
        index.terms.capacity = joint::Groups::estimate(&index, &alpha);
        let lease = index.terms.joint(&index, &alpha).unwrap().unwrap();
        assert!(lease.complete && lease.groups.len() == 1);
        let charged = index.terms.state.lock().unwrap().used;
        assert!(index.terms.joint(&index, &beta).unwrap().is_none());
        assert_eq!(index.terms.state.lock().unwrap().used, charged);
        let incomplete = joint::Groups::build(&index, &alpha, 1024, None).unwrap();
        assert!(!incomplete.complete && incomplete.groups.is_empty());
        drop(lease);
        assert!(index.terms.joint(&index, &beta).unwrap().is_some());
        index.terms.capacity = RESERVED_BYTES;
        let pair = scorer::Plan::new(
            "alpha beta",
            false,
            None,
            &BTreeSet::new(),
            &BTreeMap::new(),
        );
        let concurrent = std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| index.terms.joint(&index, &pair).unwrap().unwrap()))
                .collect();
            jobs.into_iter()
                .map(|job| job.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(
            concurrent
                .iter()
                .all(|entry| Arc::ptr_eq(entry, &concurrent[0]))
        );
        let state = index.terms.state.lock().unwrap();
        assert!(!state.ready.contains_key(&CacheKey {
            kind: 2,
            hash: joint::key(&alpha)
        }));
        assert!(state.used <= index.terms.capacity && state.inflight.is_empty());
    }
}
