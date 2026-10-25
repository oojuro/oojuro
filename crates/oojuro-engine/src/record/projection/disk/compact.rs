//! Version 2 disposable term blocks; source wire/profile formats are unrelated.
use super::*;

const ROWS: usize = 256;
pub(super) struct TfBlock {
    base: u32,
    bytes: Vec<u8>,
    present: [u64; 4],
    count: usize,
    maximum: u32,
    maximum_scope_mass: f64,
}
impl TfBlock {
    pub(super) fn encode(base: u32, pairs: &[(u32, u32)]) -> Result<Vec<u8>> {
        if base & 255 != 0 || pairs.is_empty() || pairs.len() > ROWS {
            return Err(Error::Invalid);
        }
        let mut last = None;
        let mut present = [0u64; 4];
        let mut maximum = 0u32;
        let mut uniform = true;
        for &(ordinal, tf) in pairs {
            let offset = ordinal.checked_sub(base).ok_or(Error::Invalid)? as usize;
            if offset >= ROWS || tf == 0 || last.is_some_and(|previous| previous >= ordinal) {
                return Err(Error::Invalid);
            }
            last = Some(ordinal);
            present[offset / 64] |= 1 << (offset % 64);
            maximum = maximum.max(tf);
            uniform &= tf == pairs[0].1;
        }
        let bits = (u32::BITS - maximum.leading_zeros()) as usize;
        let sparse_bytes = 3 + pairs.len() * 5;
        let dense_bytes = 2 + bits * 32;
        if sparse_bytes <= dense_bytes && (!uniform || sparse_bytes <= 37) {
            let mut bytes = Vec::with_capacity(sparse_bytes);
            bytes.push(0);
            bytes.extend((pairs.len() as u16).to_le_bytes());
            for &(ordinal, tf) in pairs {
                bytes.push((ordinal - base) as u8);
                bytes.extend(tf.to_le_bytes());
            }
            return Ok(bytes);
        }
        if uniform && 37 <= dense_bytes {
            let mut bytes = vec![2];
            bytes.extend(maximum.to_le_bytes());
            for word in present {
                bytes.extend(word.to_le_bytes());
            }
            return Ok(bytes);
        }
        let mut planes = vec![[0u64; 4]; bits];
        for &(ordinal, tf) in pairs {
            let offset = (ordinal - base) as usize;
            for (bit, plane) in planes.iter_mut().enumerate() {
                if tf & (1 << bit) != 0 {
                    plane[offset / 64] |= 1 << (offset % 64);
                }
            }
        }
        let mut bytes = vec![1, bits as u8];
        for plane in planes {
            for word in plane {
                bytes.extend(word.to_le_bytes());
            }
        }
        Ok(bytes)
    }
    pub(super) fn decode(base: u32, rows: u64, bytes: Vec<u8>) -> Result<Self> {
        if base & 255 != 0
            || u64::from(base) >= rows
            || rows > u64::from(u32::MAX)
            || bytes.is_empty()
        {
            return Err(Error::Authority);
        }
        let mut present = [0u64; 4];
        match bytes[0] {
            0 => {
                let raw: [u8; 2] = bytes.get(1..3).ok_or(Error::Authority)?.try_into().unwrap();
                let count = usize::from(u16::from_le_bytes(raw));
                if count == 0 || count > ROWS || bytes.len() != 3 + count * 5 {
                    return Err(Error::Authority);
                }
                let mut last = None;
                for pair in bytes[3..].as_chunks::<5>().0 {
                    let offset = pair[0] as usize;
                    if last.is_some_and(|previous| previous >= offset)
                        || u32::from_le_bytes(pair[1..].try_into().unwrap()) == 0
                    {
                        return Err(Error::Authority);
                    }
                    present[offset / 64] |= 1 << (offset % 64);
                    last = Some(offset);
                }
            }
            1 => {
                let bits = *bytes.get(1).ok_or(Error::Authority)? as usize;
                if !(1..=32).contains(&bits) || bytes.len() != 2 + bits * 32 {
                    return Err(Error::Authority);
                }
                for plane in bytes[2..].as_chunks::<32>().0 {
                    for (word, value) in present.iter_mut().zip(plane.as_chunks::<8>().0) {
                        *word |= u64::from_le_bytes(*value);
                    }
                }
                if bytes[bytes.len() - 32..].iter().all(|byte| *byte == 0) {
                    return Err(Error::Authority);
                }
            }
            2 => {
                if bytes.len() != 37 || u32::from_le_bytes(bytes[1..5].try_into().unwrap()) == 0 {
                    return Err(Error::Authority);
                }
                for (word, value) in present.iter_mut().zip(bytes[5..].as_chunks::<8>().0) {
                    *word = u64::from_le_bytes(*value);
                }
            }
            _ => return Err(Error::Authority),
        }
        let count = present.iter().map(|word| word.count_ones() as usize).sum();
        if count == 0 {
            return Err(Error::Authority);
        }
        for offset in 0..ROWS {
            if present[offset / 64] & (1 << (offset % 64)) != 0
                && u64::from(base) + offset as u64 >= rows
            {
                return Err(Error::Authority);
            }
        }
        let maximum = match bytes[0] {
            0 => bytes[3..]
                .as_chunks::<5>()
                .0
                .iter()
                .map(|pair| u32::from_le_bytes(pair[1..].try_into().unwrap()))
                .max()
                .unwrap(),
            1 => u32::MAX >> (32 - bytes[1]),
            2 => u32::from_le_bytes(bytes[1..5].try_into().unwrap()),
            _ => unreachable!(),
        };
        Ok(Self {
            base,
            bytes,
            present,
            count,
            maximum,
            maximum_scope_mass: f64::INFINITY,
        })
    }
    pub(super) fn pairs(&self) -> Vec<(u32, u32)> {
        (0..256u32)
            .filter_map(|offset| {
                let tf = self.tf(offset as u8);
                (tf != 0).then_some((self.base + offset, tf))
            })
            .collect()
    }
    pub(super) fn bytes(&self) -> usize {
        self.bytes.capacity()
    }
    pub(super) fn next(&self, offset: usize) -> Option<(u32, u32)> {
        if offset >= ROWS {
            return None;
        }
        let start = offset / 64;
        for word in start..4 {
            let bits = self.present[word]
                & if word == start {
                    u64::MAX << (offset % 64)
                } else {
                    u64::MAX
                };
            if bits != 0 {
                let at = word * 64 + bits.trailing_zeros() as usize;
                return Some((self.base + at as u32, self.tf(at as u8)));
            }
        }
        None
    }
    pub(super) fn set_scope_mass(&mut self, maximum: f64) {
        self.maximum_scope_mass = maximum;
    }
    pub(super) fn maximum_scope_mass(&self) -> f64 {
        self.maximum_scope_mass
    }
    pub(super) fn maximum(&self) -> u32 {
        self.maximum
    }
    pub(super) fn presence(&self) -> [u64; 4] {
        self.present
    }
    pub(super) fn base(&self) -> u32 {
        self.base
    }
    pub(super) fn tf(&self, offset: u8) -> u32 {
        let at = usize::from(offset);
        if self.present[at / 64] & (1 << (at % 64)) == 0 {
            return 0;
        }
        match self.bytes[0] {
            0 => {
                let mut left = 0;
                let mut right = self.count;
                while left < right {
                    let middle = left + (right - left) / 2;
                    let start = 3 + middle * 5;
                    match self.bytes[start].cmp(&offset) {
                        std::cmp::Ordering::Less => left = middle + 1,
                        std::cmp::Ordering::Greater => right = middle,
                        std::cmp::Ordering::Equal => {
                            return u32::from_le_bytes(
                                self.bytes[start + 1..start + 5].try_into().unwrap(),
                            );
                        }
                    }
                }
                unreachable!("validated presence belongs to its sparse entry")
            }
            1 => self.bytes[2..].as_chunks::<32>().0.iter().enumerate().fold(
                0,
                |value, (bit, plane)| {
                    let start = (at / 64) * 8;
                    value
                        | ((((u64::from_le_bytes(plane[start..start + 8].try_into().unwrap())
                            >> (at % 64))
                            & 1) as u32)
                            << bit)
                },
            ),
            2 => u32::from_le_bytes(self.bytes[1..5].try_into().unwrap()),
            _ => unreachable!("validated encoding mode"),
        }
    }
}

// Query-local memoization keeps the exact scorer's operation order. Metadata
// and every TF participate in the key; time/label only order equal scores.
// Bounded shape diversity falls back to ordinary exact scoring after 1024 keys.
pub(super) struct Scores {
    shape: Vec<u32>,
    ready: crate::lexical::FastMap<Vec<u32>, f64>,
}
impl Scores {
    pub(super) fn new(terms: usize) -> Option<Self> {
        (terms <= 16).then(|| Self {
            shape: Vec::with_capacity(terms + 2),
            ready: Default::default(),
        })
    }
    pub(super) fn score(
        &mut self,
        model: &scorer::Model,
        length: u32,
        scope: u32,
        tf: &[u32],
    ) -> f64 {
        self.shape.clear();
        self.shape.extend([length, scope]);
        self.shape.extend(tf);
        if let Some(score) = self.ready.get(self.shape.as_slice()) {
            return *score;
        }
        let score = model.score(length, scope, tf);
        if self.ready.len() < 1024 {
            self.ready.insert(self.shape.clone(), score);
        }
        score
    }
}
#[test]
fn exact_score_memoization_includes_all_shape_fields_and_bounds_diversity() {
    let plan = scorer::Plan::new(
        "battery electrode transaction",
        false,
        None,
        &BTreeSet::new(),
        &BTreeMap::new(),
    );
    let model = scorer::Stats::scope_counts(&plan, vec![80, 40, 20], 1000, 2000)
        .unwrap()
        .finish(plan);
    let mut scores = Scores::new(3).unwrap();
    for row in 0..3000u32 {
        let (length, scope) = (1 + row % 1100, 1 + row % 21);
        let tf = [row % 11, row % 3, row % 7];
        for _ in 0..2 {
            assert_eq!(
                scores.score(&model, length, scope, &tf).to_bits(),
                model.score(length, scope, &tf).to_bits()
            );
        }
    }
    assert_eq!(scores.ready.len(), 1024);
    assert!(Scores::new(17).is_none());
}

// Exact integer mass over arbitrary candidate unions without rescoring every
// numeric row. Allocation is admitted before any full-width plane is created.
#[derive(Clone)]
pub(super) struct Mass {
    rows: usize,
    pub(super) live: Vec<u64>,
    length: Vec<(u8, Vec<u64>)>,
    scope: Vec<(u8, Vec<u64>)>,
    pub(super) shapes: Vec<BlockShape>,
}

// Under a fixed exact (length,scope) shape, score is monotone in every TF.
// Scope also appears positively as TF*scope/length, so independently minimizing
// length/scope is unsafe. Heterogeneous/empty shapes deliberately do not prune.
pub(super) fn shape_bound(
    model: &scorer::Model,
    shapes: &[(u32, u32)],
    maximum_tf: &[u32],
) -> Option<f64> {
    if shapes.is_empty() || shapes.len() > 16 || maximum_tf.len() != model.plan.terms.len() {
        return None;
    }
    let mut bound = f64::NEG_INFINITY;
    for &(length, scope) in shapes {
        if length == 0 && model.plan.scope_model() && maximum_tf.iter().any(|tf| *tf != 0) {
            return None;
        }
        let score = model.score(length, scope, maximum_tf);
        if !score.is_finite() {
            return None;
        }
        if score.total_cmp(&bound).is_gt() {
            bound = score;
        }
    }
    Some(bound)
}

#[test]
fn heterogeneous_scope_envelopes_bound_exact_multi_term_scores_and_reject_invalid_proofs() {
    let plan = scorer::Plan::new(
        "battery electrode original",
        false,
        None,
        &BTreeSet::new(),
        &BTreeMap::new(),
    );
    let model = scorer::Stats::scope_counts(&plan, vec![80, 40, 20], 1000, 2000)
        .unwrap()
        .finish(plan);
    let mut state = 0xabcde9137u64;
    for generation in 0..100 {
        let mut rows = Vec::new();
        for row in 0..257 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let length = 1 + (state % 1024) as u32;
            let scope = 1 + ((state >> 17) % u64::from(length)) as u32;
            let mut remaining = length;
            let mut tf = [0u32; 3];
            for (at, n) in tf.iter_mut().enumerate() {
                *n = ((state >> (at * 11)) % (u64::from(remaining) + 1)) as u32;
                remaining -= *n;
            }
            // Recompute a live envelope after changing retirement membership.
            if (row + generation) % 7 != 0 {
                rows.push((length, scope, tf));
            }
        }
        let minimum = rows.iter().map(|(_, scope, _)| *scope).min().unwrap();
        let mut maximum = [0.0f64; 3];
        for (length, scope, tf) in &rows {
            for at in 0..3 {
                maximum[at] = maximum[at].max(tf[at] as f64 * *scope as f64 / *length as f64);
            }
        }
        let bound = model.scope_bound(minimum, &maximum).unwrap();
        for (length, scope, tf) in rows {
            assert!(model.score(length, scope, &tf).total_cmp(&bound).is_le());
        }
    }
    for bad in [f64::NAN, f64::INFINITY, -1.] {
        assert!(model.scope_bound(1, &[bad, 1., 1.]).is_none());
    }
    assert!(model.scope_bound(1, &[1., 1.]).is_none());
}

#[test]
fn scope_bounds_preserve_positive_scope_mass_and_exact_shape_arithmetic() {
    let plan = scorer::Plan::new("battery", false, None, &BTreeSet::new(), &BTreeMap::new());
    let model = scorer::Stats::single_scope(&plan, 1, 100, 1_000_000)
        .unwrap()
        .finish(plan);
    let shapes = [(100, 1), (100, 100)];
    let unsafe_minima = model.score(100, 1, &[1]);
    assert!(
        model.score(100, 100, &[1]) > unsafe_minima,
        "scope contributes positively to TF mass"
    );
    let bound = shape_bound(&model, &shapes, &[7]).unwrap();
    for &(length, scope) in &shapes {
        for tf in 0..=7 {
            assert!(model.score(length, scope, &[tf]).total_cmp(&bound).is_le());
        }
    }
    assert!(shape_bound(&model, &[(0, 0)], &[1]).is_none());
    assert!(shape_bound(&model, &[(100, 10); 17], &[1]).is_none());
}
#[derive(Clone)]
pub(super) struct BlockShape {
    values: [(u32, u32); 16],
    count: usize,
    pub(super) minimum_scope: u32,
    pub(super) time: i64,
    pub(super) label: u128,
}
impl Default for BlockShape {
    fn default() -> Self {
        Self {
            values: [(0, 0); 16],
            count: 0,
            minimum_scope: u32::MAX,
            time: i64::MIN,
            label: u128::MAX,
        }
    }
}
impl BlockShape {
    pub(super) fn values(&self) -> &[(u32, u32)] {
        if self.count > 16 {
            &[]
        } else {
            &self.values[..self.count]
        }
    }
    fn add(&mut self, column: &Column) {
        self.minimum_scope = self.minimum_scope.min(column.scope);
        self.time = self.time.max(column.valid_at);
        self.label = self.label.min(column.label);
        if self.count <= 16 && !self.values[..self.count].contains(&(column.length, column.scope)) {
            if self.count < 16 {
                self.values[self.count] = (column.length, column.scope);
            }
            self.count += 1;
        }
    }
}
impl Mass {
    pub(super) fn required(rows: usize, length: u32, scope: u32) -> Result<usize> {
        let planes = (length.count_ones() + scope.count_ones()) as usize;
        rows.div_ceil(64)
            .checked_mul(8)
            .and_then(|bytes| bytes.checked_mul(1 + planes))
            .and_then(|bytes| {
                bytes.checked_add(
                    planes * std::mem::size_of::<(u8, Vec<u64>)>()
                        + std::mem::size_of::<Self>()
                        + rows.div_ceil(256) * std::mem::size_of::<BlockShape>(),
                )
            })
            .ok_or(Error::Bounds)
    }
    pub(super) fn new(columns: &[Arc<[Column]>], limit: usize) -> Result<Self> {
        let rows = columns.iter().try_fold(0usize, |rows, block| {
            rows.checked_add(block.len()).ok_or(Error::Bounds)
        })?;
        if rows > u32::MAX as usize {
            return Err(Error::Bounds);
        }
        let words = rows.div_ceil(64);
        let mut length = 0u32;
        let mut scope = 0u32;
        for column in columns.iter().flat_map(|block| block.iter()) {
            if column.live() && column.indexed() {
                length |= column.length;
                scope |= column.scope;
            }
        }
        let bytes = Self::required(rows, length, scope)?;
        if bytes > limit {
            return Err(Error::Bounds);
        }
        let make = |mask: u32| {
            let mut planes = Vec::with_capacity(mask.count_ones() as usize);
            for bit in 0..32u8 {
                if mask & (1 << bit) != 0 {
                    planes.push((bit, vec![0u64; words]));
                }
            }
            planes
        };
        let mut result = Self {
            rows,
            live: vec![0; words],
            length: make(length),
            scope: make(scope),
            shapes: std::iter::repeat_with(BlockShape::default)
                .take(rows.div_ceil(256))
                .collect(),
        };
        for (ordinal, column) in columns.iter().flat_map(|block| block.iter()).enumerate() {
            result.add(ordinal, column);
        }
        Ok(result)
    }
    fn add(&mut self, ordinal: usize, column: &Column) {
        if !column.live() || !column.indexed() {
            return;
        }
        self.shapes[ordinal / 256].add(column);
        let word = ordinal / 64;
        let flag = 1u64 << (ordinal % 64);
        self.live[word] |= flag;
        for (value, planes) in [
            (column.length, &mut self.length),
            (column.scope, &mut self.scope),
        ] {
            for (bit, plane) in planes.iter_mut() {
                if value & (1 << *bit) != 0 {
                    plane[word] |= flag;
                }
            }
        }
    }
    /// Copy already-admitted compact masks and revisit only changed numeric
    /// blocks. Existing zeroed planes may remain after retirement; no old row
    /// acquires visibility or a numeric contribution from that extra capacity.
    pub(super) fn refresh(
        mut self,
        columns: &[Arc<[Column]>],
        changed: &BTreeSet<usize>,
        limit: usize,
    ) -> Result<Self> {
        let rows = columns.iter().try_fold(0usize, |rows, block| {
            rows.checked_add(block.len()).ok_or(Error::Bounds)
        })?;
        if columns.iter().enumerate().any(|(at, block)| {
            block.is_empty()
                || block.len() > COLUMN_ROWS
                || (at + 1 < columns.len() && block.len() != COLUMN_ROWS)
        }) || rows < self.rows
            || rows > u32::MAX as usize
            || changed.iter().any(|block| *block >= columns.len())
        {
            return Err(Error::Authority);
        }
        if rows > self.rows
            && (self.rows / COLUMN_ROWS..rows.div_ceil(COLUMN_ROWS))
                .any(|block| !changed.contains(&block))
        {
            return Err(Error::Authority);
        }
        let mut length = self
            .length
            .iter()
            .fold(0u32, |mask, (bit, _)| mask | (1 << *bit));
        let mut scope = self
            .scope
            .iter()
            .fold(0u32, |mask, (bit, _)| mask | (1 << *bit));
        for block in changed {
            for column in columns[*block]
                .iter()
                .filter(|column| column.live() && column.indexed())
            {
                length |= column.length;
                scope |= column.scope;
            }
        }
        if Self::required(rows, length, scope)? > limit {
            return Err(Error::Bounds);
        }
        let words = rows.div_ceil(64);
        fn grow(words: usize, mask: u32, planes: &mut Vec<(u8, Vec<u64>)>) {
            let count = mask.count_ones() as usize;
            planes.reserve_exact(count.saturating_sub(planes.len()));
            for (_, plane) in planes.iter_mut() {
                plane.reserve_exact(words.saturating_sub(plane.len()));
                plane.resize(words, 0);
            }
            for bit in 0..32u8 {
                if mask & (1 << bit) != 0 && !planes.iter().any(|(old, _)| *old == bit) {
                    planes.push((bit, vec![0; words]));
                }
            }
            planes.sort_unstable_by_key(|(bit, _)| *bit);
        }
        self.live
            .reserve_exact(words.saturating_sub(self.live.len()));
        self.live.resize(words, 0);
        grow(words, length, &mut self.length);
        grow(words, scope, &mut self.scope);
        let blocks = rows.div_ceil(256);
        self.shapes
            .reserve_exact(blocks.saturating_sub(self.shapes.len()));
        self.shapes.resize_with(blocks, BlockShape::default);
        self.rows = rows;
        for block in changed {
            let start = block * COLUMN_ROWS;
            let end = start + columns[*block].len();
            let range = start / 64..end.div_ceil(64);
            self.live[range.clone()].fill(0);
            for (_, plane) in self.length.iter_mut().chain(&mut self.scope) {
                plane[range.clone()].fill(0);
            }
            for shape in &mut self.shapes[start / 256..end.div_ceil(256)] {
                *shape = BlockShape::default();
            }
            for (offset, column) in columns[*block].iter().enumerate() {
                self.add(start + offset, column);
            }
        }
        if self.bytes() > limit {
            return Err(Error::Bounds);
        }
        Ok(self)
    }
    fn validate(&self, mask: &[u64]) -> Result<()> {
        if mask.len() != self.live.len()
            || mask
                .iter()
                .zip(&self.live)
                .any(|(mask, live)| mask & !live != 0)
        {
            return Err(Error::Authority);
        }
        Ok(())
    }
    pub(super) fn sum(&self, mask: &[u64], scope: bool) -> Result<u64> {
        self.validate(mask)?;
        let mut total = 0u64;
        for (bit, plane) in if scope { &self.scope } else { &self.length } {
            let count: u64 = mask
                .iter()
                .zip(plane)
                .map(|(a, b)| u64::from((a & b).count_ones()))
                .sum();
            total = total
                .checked_add(count.checked_mul(1u64 << bit).ok_or(Error::Bounds)?)
                .ok_or(Error::Bounds)?;
        }
        Ok(total)
    }
    pub(super) fn bytes(&self) -> usize {
        self.live.capacity() * 8
            + self.shapes.capacity() * std::mem::size_of::<BlockShape>()
            + (self.length.capacity() + self.scope.capacity())
                * std::mem::size_of::<(u8, Vec<u64>)>()
            + self
                .length
                .iter()
                .chain(&self.scope)
                .map(|(_, plane)| plane.capacity() * 8)
                .sum::<usize>()
            + std::mem::size_of::<Self>()
    }
}

#[test]
fn bitplane_mass_matches_complete_integer_oracle_and_admits_before_allocation() {
    let columns: Vec<_> = (0..1025u32)
        .map(|n| Column {
            valid_at: i64::from(n),
            length: if n.is_multiple_of(17) {
                u32::MAX
            } else {
                n * 17
            },
            scope: n,
            kind: 0,
            status: 0,
            flags: if n.is_multiple_of(7) { 0 } else { 3 },
            tool: [0; 32],
            agent: [0; 32],
            label: u128::from(n) + 1,
        })
        .collect();
    let blocks: Vec<_> = columns
        .chunks(COLUMN_ROWS)
        .map(Arc::<[Column]>::from)
        .collect();
    let mass = Mass::new(&blocks, 1 << 20).unwrap();
    assert!(mass.bytes() < 1 << 20);
    assert_eq!(
        Mass::new(&blocks, mass.bytes()).unwrap().bytes(),
        mass.bytes()
    );
    assert!(Mass::new(&blocks, mass.bytes() - 1).is_err());
    assert!(matches!(Mass::new(&blocks, 1), Err(Error::Bounds)));
    for selection in 0..100u32 {
        let mut mask = vec![0u64; columns.len().div_ceil(64)];
        let mut expected = (0u64, 0u64);
        for (ordinal, column) in columns.iter().enumerate() {
            if column.live()
                && column.indexed()
                && (ordinal as u32 + selection).is_multiple_of(3 + selection % 17)
            {
                mask[ordinal / 64] |= 1 << (ordinal % 64);
                expected.0 += u64::from(column.length);
                expected.1 += u64::from(column.scope);
            }
        }
        assert_eq!(mass.sum(&mask, false).unwrap(), expected.0);
        assert_eq!(mass.sum(&mask, true).unwrap(), expected.1);
    }
    let mut bad = mass.live.clone();
    bad[0] |= 1;
    assert!(mass.sum(&bad, true).is_err());
    assert!(mass.sum(&[], true).is_err());
}

#[test]
fn incremental_mass_refresh_matches_full_rebuild_through_growth_retirement_and_new_bits() {
    let make = |n: usize| Column {
        valid_at: (n as i64 % 71) - 35,
        length: 1 + (n % 37) as u32,
        scope: 1 + (n % 11) as u32,
        kind: 0,
        status: 0,
        flags: if n.is_multiple_of(7) { 0 } else { 3 },
        tool: [0; 32],
        agent: [0; 32],
        label: n as u128 + 1,
    };
    let mut rows: Vec<_> = (0..2305).map(make).collect();
    let blocks = |rows: &[Column]| {
        rows.chunks(COLUMN_ROWS)
            .map(Arc::<[Column]>::from)
            .collect::<Vec<_>>()
    };
    let mut mass = Mass::new(&blocks(&rows), 1 << 20).unwrap();
    for step in 0..8usize {
        let previous_rows = rows.len();
        let mut changed = BTreeSet::new();
        for offset in [0, 1023, 1024, previous_rows - 1] {
            rows[offset].flags &= !2;
            changed.insert(offset / COLUMN_ROWS);
        }
        for n in previous_rows..previous_rows + 83 + step * 17 {
            let mut column = make(n);
            if n == previous_rows {
                column.length = 1 << (16 + step);
                column.scope = 1 << (12 + step);
                column.valid_at = i64::MAX - step as i64;
            }
            rows.push(column);
            changed.insert(n / COLUMN_ROWS);
        }
        let columns = blocks(&rows);
        assert!(matches!(
            mass.clone().refresh(&columns, &BTreeSet::new(), 1 << 20),
            Err(Error::Authority)
        ));
        assert!(matches!(
            mass.clone().refresh(&columns, &changed, 1),
            Err(Error::Bounds)
        ));
        let old_live = mass.live.clone();
        let updated = mass.clone().refresh(&columns, &changed, 1 << 20).unwrap();
        assert_eq!(mass.live, old_live);
        let fresh = Mass::new(&columns, 1 << 20).unwrap();
        assert_eq!(updated.live, fresh.live);
        assert_eq!(updated.rows, fresh.rows);
        for (actual, expected) in updated.shapes.iter().zip(&fresh.shapes) {
            assert_eq!(actual.values(), expected.values());
            assert_eq!((actual.time, actual.label), (expected.time, expected.label));
        }
        for filter in 0..10usize {
            let mut mask = fresh.live.clone();
            for ordinal in 0..rows.len() {
                if !(ordinal + filter).is_multiple_of(3 + filter) {
                    mask[ordinal / 64] &= !(1 << (ordinal % 64));
                }
            }
            assert_eq!(
                updated.sum(&mask, false).unwrap(),
                fresh.sum(&mask, false).unwrap()
            );
            assert_eq!(
                updated.sum(&mask, true).unwrap(),
                fresh.sum(&mask, true).unwrap()
            );
        }
        mass = updated;
    }
}

#[test]
fn adaptive_blocks_preserve_every_tf_and_ordinal_with_bounded_payloads() {
    let mut state = 42u64;
    for iteration in 0..1000 {
        let base = (iteration as u32) * 256;
        let mut pairs = Vec::new();
        for offset in 0..256u32 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            if state & 3 == 0 {
                continue;
            }
            let tf = match iteration % 5 {
                0 => 1,
                1 => 2,
                2 => u32::MAX,
                3 => (state as u32).max(1),
                _ => 1 + (state % 16) as u32,
            };
            pairs.push((base + offset, tf));
        }
        let bytes = TfBlock::encode(base, &pairs).unwrap();
        assert!(bytes.len() <= 1026);
        let block = TfBlock::decode(base, u64::from(base) + 256, bytes).unwrap();
        assert_eq!(block.base, base);
        assert_eq!(block.count, pairs.len());
        for offset in 0..256u32 {
            assert_eq!(
                block.tf(offset as u8),
                pairs
                    .iter()
                    .find(|(row, _)| *row == base + offset)
                    .map_or(0, |(_, tf)| *tf)
            );
        }
    }
}
#[test]
fn malformed_and_out_of_range_blocks_refuse_before_any_query() {
    let mut zero_tf = vec![0; 37];
    zero_tf[0] = 2;
    for bytes in [
        vec![],
        vec![3],
        vec![0, 0, 0],
        vec![1, 0],
        vec![1, 33],
        zero_tf,
    ] {
        assert!(TfBlock::decode(0, 256, bytes).is_err());
    }
    let pairs = [(0, 7), (255, u32::MAX)];
    let bytes = TfBlock::encode(0, &pairs).unwrap();
    assert!(TfBlock::decode(0, 255, bytes.clone()).is_err());
    for end in 0..bytes.len() {
        assert!(TfBlock::decode(0, 256, bytes[..end].to_vec()).is_err());
    }
    assert!(TfBlock::encode(1, &pairs).is_err());
    assert!(TfBlock::encode(0, &[(1, 0)]).is_err());
    assert!(TfBlock::encode(0, &[(1, 1), (1, 1)]).is_err());
    assert!(TfBlock::encode(0, &[(256, 1)]).is_err());
    let block = TfBlock::decode(0, 256, bytes).unwrap();
    assert_eq!(block.tf(255), u32::MAX);
}
