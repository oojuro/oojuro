//! Exact raw UTF-8 run positions for a separately versioned disposable cache.
//! Runs retain case and every separator byte. They never normalize or stem.
use super::*;

const MAX_ATOMS: usize = 128;
const MAX_POSITIONS: usize = 4096;
const MAX_BYTES: usize = 28 << 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Atom {
    pub hash: [u8; 32],
    pub offset: u32,
}
fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
fn runs(text: &str, mut visit: impl FnMut(bool, usize, &str)) {
    let mut start = 0;
    let mut previous = None;
    for (offset, character) in text.char_indices() {
        let current = word(character);
        if previous.is_some_and(|previous| previous != current) {
            visit(previous.unwrap(), start, &text[start..offset]);
            start = offset;
        }
        previous = Some(current);
    }
    if let Some(previous) = previous {
        visit(previous, start, &text[start..]);
    }
}
fn hash(is_word: bool, text: &str) -> [u8; 32] {
    let mut state = blake3::Hasher::new_derive_key("oojuro raw literal run v1");
    state.update(&[u8::from(is_word)]);
    state.update(text.as_bytes());
    *state.finalize().as_bytes()
}
/// Complete proof is available when both needle edges are word characters.
/// Other phrases use the existing exact-original fallback. This restriction
/// avoids silently changing partial separator or word-boundary semantics.
pub(super) fn query(needle: &str) -> Option<Vec<Atom>> {
    if !needle.chars().next().is_some_and(word) || !needle.chars().next_back().is_some_and(word) {
        return None;
    }
    let mut atoms = Vec::new();
    runs(needle, |kind, offset, text| {
        if atoms.len() <= MAX_ATOMS {
            atoms.push(Atom {
                hash: hash(kind, text),
                offset: offset as u32,
            });
        }
    });
    (atoms.len() <= MAX_ATOMS).then_some(atoms)
}
pub(super) fn analyze(text: &str) -> BTreeMap<[u8; 32], Vec<u32>> {
    let mut atoms = BTreeMap::new();
    runs(text, |kind, offset, text| {
        atoms
            .entry(hash(kind, text))
            .or_insert_with(Vec::new)
            .push(offset as u32);
    });
    atoms
}

pub(super) fn push(sorter: &mut tree::Sorter<'_>, ordinal: u32, text: &str) -> Result<()> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(Error::Bounds);
    }
    for (hash, positions) in analyze(text) {
        for fragment in positions.chunks(1024) {
            let mut key = hash.to_vec();
            key.extend(ordinal.to_be_bytes());
            key.extend(fragment[0].to_be_bytes());
            let mut value = Vec::with_capacity(fragment.len() * 4);
            for position in fragment {
                value.extend(position.to_le_bytes());
            }
            sorter.push(tree::Entry::new(key, value)?)?;
        }
    }
    Ok(())
}

pub(super) fn fold_sorted(
    context: tree::Context<'_>,
    root: &tree::Root,
    mut emit: impl FnMut(tree::Entry) -> Result<()>,
) -> Result<()> {
    let mut input = tree::Cursor::seek(context, root, b"")?;
    let mut term = None::<[u8; 32]>;
    let mut base = 0;
    let mut rows = Vec::<(u32, Vec<u32>)>::new();
    let mut count = 0;
    let flush = |term: Option<[u8; 32]>,
                 base,
                 rows: &mut Vec<(u32, Vec<u32>)>,
                 emit: &mut dyn FnMut(tree::Entry) -> Result<()>|
     -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut key = term.ok_or(Error::Authority)?.to_vec();
        key.extend(rows[0].0.to_be_bytes());
        key.extend(rows[0].1[0].to_be_bytes());
        emit(tree::Entry::new(key, Block::encode(base, rows)?)?)?;
        rows.clear();
        Ok(())
    };
    while let Some(entry) = input.next()? {
        if entry.key.len() != 40
            || entry.value.is_empty()
            || entry.value.len() % 4 != 0
            || entry.value.len() > 4096
        {
            return Err(Error::Authority);
        }
        let hash = entry.key[..32].try_into().unwrap();
        let ordinal = parse_u32(&entry.key[32..36])?;
        let positions: Vec<_> = entry
            .value
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u32::from_le_bytes(*bytes))
            .collect();
        if positions[0] != parse_u32(&entry.key[36..])? {
            return Err(Error::Authority);
        }
        if term != Some(hash)
            || base != ordinal & !255
            || count + positions.len() > MAX_POSITIONS
            || rows.last().is_some_and(|(row, _)| *row >= ordinal)
        {
            flush(term, base, &mut rows, &mut emit)?;
            count = 0;
        }
        term = Some(hash);
        base = ordinal & !255;
        count += positions.len();
        rows.push((ordinal, positions));
    }
    flush(term, base, &mut rows, &mut emit)
}

/// A bounded fragment of one aligned 256-row position block. Equal exact
/// position lists share one row mask. Fragments may repeat rows when one row's
/// run occurs very often; readers union those fragments before joining runs.
pub(super) struct Block {
    pub base: u32,
    groups: Vec<Group>,
}
struct Group {
    rows: [u64; 4],
    positions: Vec<u32>,
}
impl Block {
    pub fn encode(base: u32, rows: &[(u32, Vec<u32>)]) -> Result<Vec<u8>> {
        if base & 255 != 0 || rows.is_empty() || rows.len() > 256 {
            return Err(Error::Invalid);
        }
        let mut grouped = BTreeMap::<Vec<u32>, [u64; 4]>::new();
        let mut last = None;
        let mut count = 0;
        for (ordinal, positions) in rows {
            let offset = ordinal.checked_sub(base).ok_or(Error::Invalid)? as usize;
            if offset >= 256
                || last.is_some_and(|last| last >= *ordinal)
                || positions.is_empty()
                || positions.windows(2).any(|pair| pair[0] >= pair[1])
                || positions
                    .last()
                    .is_some_and(|position| *position as usize >= MAX_TEXT_BYTES)
            {
                return Err(Error::Invalid);
            }
            count += positions.len();
            if count > MAX_POSITIONS {
                return Err(Error::Bounds);
            }
            let mask = grouped.entry(positions.clone()).or_default();
            mask[offset / 64] |= 1 << (offset % 64);
            last = Some(*ordinal);
        }
        let mut bytes = vec![2];
        bytes.extend((grouped.len() as u16).to_le_bytes());
        for (positions, rows) in grouped {
            let population: u32 = rows.iter().map(|word| word.count_ones()).sum();
            if population <= 28 {
                bytes.push(population as u8);
                for (at, mut word) in rows.into_iter().enumerate() {
                    while word != 0 {
                        let offset = word.trailing_zeros();
                        bytes.push((at * 64 + offset as usize) as u8);
                        word &= word - 1;
                    }
                }
            } else {
                bytes.push(0);
                for mask in rows {
                    bytes.extend(mask.to_le_bytes());
                }
            }
            bytes.extend((positions.len() as u16).to_le_bytes());
            let mut previous = 0;
            for position in positions {
                let mut delta = position - previous;
                previous = position;
                while delta >= 128 {
                    bytes.push((delta as u8 & 127) | 128);
                    delta >>= 7;
                }
                bytes.push(delta as u8);
            }
        }
        if bytes.len() > MAX_BYTES {
            return Err(Error::Bounds);
        }
        Ok(bytes)
    }
    /// Include group containers, decoded offsets and the temporary canonical
    /// ordering check before allocating. Delta bytes can expand into u32s.
    pub fn decoded_charge(bytes: &[u8]) -> Result<usize> {
        if !matches!(bytes.first(), Some(1 | 2)) || bytes.len() > MAX_BYTES {
            return Err(Error::Authority);
        }
        let count = u16::from_le_bytes(bytes.get(1..3).ok_or(Error::Authority)?.try_into().unwrap())
            as usize;
        if count == 0 || count > 256 {
            return Err(Error::Authority);
        }
        Ok(bytes.len() * 8 + count * 128 + 4096)
    }
    pub fn decode(base: u32, rows: u64, bytes: &[u8]) -> Result<Self> {
        if base & 255 != 0
            || u64::from(base) >= rows
            || bytes.len() > MAX_BYTES
            || !matches!(bytes.first(), Some(1 | 2))
        {
            return Err(Error::Authority);
        }
        let version = bytes[0];
        let mut input = bytes.get(1..).ok_or(Error::Authority)?;
        fn take<const N: usize>(input: &mut &[u8]) -> Result<[u8; N]> {
            let value = input.get(..N).ok_or(Error::Authority)?.try_into().unwrap();
            *input = &input[N..];
            Ok(value)
        }
        let count = u16::from_le_bytes(take(&mut input)?) as usize;
        if count == 0 || count > 256 {
            return Err(Error::Authority);
        }
        let mut groups = Vec::with_capacity(count);
        let mut seen = [0u64; 4];
        let mut total = 0;
        let mut previous: Option<Vec<u32>> = None;
        for _ in 0..count {
            let mut mask = [0u64; 4];
            let sparse = if version == 1 {
                0
            } else {
                take::<1>(&mut input)?[0]
            };
            if sparse == 0 {
                for word in &mut mask {
                    *word = u64::from_le_bytes(take(&mut input)?);
                }
                if version == 2 && mask.iter().map(|word| word.count_ones()).sum::<u32>() <= 28 {
                    return Err(Error::Authority);
                }
            } else {
                if sparse > 28 {
                    return Err(Error::Authority);
                }
                let mut previous = None;
                for _ in 0..sparse {
                    let at = take::<1>(&mut input)?[0];
                    if previous.is_some_and(|last| last >= at) {
                        return Err(Error::Authority);
                    }
                    mask[at as usize / 64] |= 1 << (at % 64);
                    previous = Some(at);
                }
            }
            for (at, word) in mask.iter().enumerate() {
                if seen[at] & *word != 0 {
                    return Err(Error::Authority);
                }
                seen[at] |= *word;
            }
            if mask.iter().all(|word| *word == 0) {
                return Err(Error::Authority);
            }
            let length = u16::from_le_bytes(take(&mut input)?) as usize;
            total += length
                * mask
                    .iter()
                    .map(|word| word.count_ones() as usize)
                    .sum::<usize>();
            if length == 0 || total > MAX_POSITIONS {
                return Err(Error::Authority);
            }
            let mut positions = Vec::with_capacity(length);
            for _ in 0..length {
                let position = if version == 1 {
                    u32::from_le_bytes(take(&mut input)?)
                } else {
                    let mut delta = 0u32;
                    let mut complete = false;
                    for shift in (0..28).step_by(7) {
                        let byte = take::<1>(&mut input)?[0];
                        delta |= u32::from(byte & 127) << shift;
                        if byte & 128 == 0 {
                            if shift != 0 && byte == 0 {
                                return Err(Error::Authority);
                            }
                            complete = true;
                            break;
                        }
                    }
                    if !complete {
                        return Err(Error::Authority);
                    }
                    positions
                        .last()
                        .copied()
                        .unwrap_or(0u32)
                        .checked_add(delta)
                        .ok_or(Error::Authority)?
                };
                if position as usize >= MAX_TEXT_BYTES
                    || positions.last().is_some_and(|last| *last >= position)
                {
                    return Err(Error::Authority);
                }
                positions.push(position);
            }
            if previous.as_ref().is_some_and(|last| *last >= positions) {
                return Err(Error::Authority);
            }
            previous = Some(positions.clone());
            groups.push(Group {
                rows: mask,
                positions,
            });
        }
        if !input.is_empty() {
            return Err(Error::Authority);
        }
        for offset in 0..256 {
            if seen[offset / 64] & (1 << (offset % 64)) != 0
                && u64::from(base) + offset as u64 >= rows
            {
                return Err(Error::Authority);
            }
        }
        Ok(Self { base, groups })
    }
    pub fn positions(&self, ordinal: u32) -> Option<&[u32]> {
        let offset = ordinal.checked_sub(self.base)? as usize;
        if offset >= 256 {
            return None;
        }
        self.groups.iter().find_map(|group| {
            (group.rows[offset / 64] & (1 << (offset % 64)) != 0)
                .then_some(group.positions.as_slice())
        })
    }
}

/// Join exact offsets through a bounded sparse position-to-row-mask map. One
/// work unit is an input group or an offset plus bounded map lookup (at most
/// 8192 keys). Budget refusal discards the whole proof and uses exact text.
/// Two maps can coexist; charging 128 bytes per entry reserves at most 2 MiB
/// for map nodes, separately from the 8 MiB decoded-fragment allowance.
pub(super) fn matches(
    base: u32,
    atoms: &[(u32, &[Block])],
    work: &mut usize,
) -> Result<Option<[u64; 4]>> {
    const MAX_STARTS: usize = 8192;
    if atoms.is_empty() || atoms[0].0 != 0 || atoms.len() > MAX_ATOMS || base & 255 != 0 {
        return Err(Error::Invalid);
    }
    if atoms
        .iter()
        .any(|(_, blocks)| blocks.iter().any(|block| block.base != base))
    {
        return Err(Error::Authority);
    }
    fn charge(work: &mut usize) -> bool {
        if let Some(remaining) = work.checked_sub(1) {
            *work = remaining;
            true
        } else {
            false
        }
    }
    fn insert(starts: &mut BTreeMap<u32, [u64; 4]>, at: u32, rows: [u64; 4]) -> bool {
        if let Some(previous) = starts.get_mut(&at) {
            for (previous, rows) in previous.iter_mut().zip(rows) {
                *previous |= rows;
            }
        } else {
            if starts.len() == MAX_STARTS {
                return false;
            }
            starts.insert(at, rows);
        }
        true
    }
    let mut starts = BTreeMap::new();
    for block in atoms[0].1 {
        for group in &block.groups {
            if !charge(work) {
                return Ok(None);
            }
            for position in &group.positions {
                if !charge(work) || !insert(&mut starts, *position, group.rows) {
                    return Ok(None);
                }
            }
        }
    }
    for (offset, fragments) in &atoms[1..] {
        if starts.is_empty() {
            return Ok(Some([0; 4]));
        }
        let mut allowed = [0u64; 4];
        let mut ranges = [(u32::MAX, 0u32); 256];
        // Up to 16384 logical row starts. Vec growth plus 256 containers is
        // conservatively bounded by 144 KiB; admission precedes every push.
        let mut row_starts: Option<[Vec<u32>; 256]> = Some(std::array::from_fn(|_| Vec::new()));
        let mut row_start_count = 0usize;
        for (position, masks) in &starts {
            for (word, mask) in masks.iter().copied().enumerate() {
                allowed[word] |= mask;
                let mut active = mask;
                while active != 0 {
                    if !charge(work) {
                        return Ok(None);
                    }
                    let at = word * 64 + active.trailing_zeros() as usize;
                    active &= active - 1;
                    ranges[at].0 = ranges[at].0.min(*position);
                    ranges[at].1 = ranges[at].1.max(*position);
                    if row_start_count == 16384 {
                        row_starts = None;
                    } else if let Some(rows) = &mut row_starts {
                        rows[at].push(*position);
                        row_start_count += 1;
                    }
                }
            }
        }
        let mut next = BTreeMap::new();
        for fragment in *fragments {
            for group in &fragment.groups {
                if !charge(work) {
                    return Ok(None);
                }
                let mut minimum = u32::MAX;
                let mut maximum = 0;
                let mut checks = 0usize;
                for (word, mask) in group.rows.iter().copied().enumerate() {
                    let mut active = mask & allowed[word];
                    while active != 0 {
                        if !charge(work) {
                            return Ok(None);
                        }
                        let at = word * 64 + active.trailing_zeros() as usize;
                        active &= active - 1;
                        minimum = minimum.min(ranges[at].0);
                        maximum = maximum.max(ranges[at].1);
                        checks += row_starts.as_ref().map_or(0, |rows| rows[at].len());
                    }
                }
                if minimum == u32::MAX {
                    continue;
                }
                let Some(first) = minimum.checked_add(*offset) else {
                    continue;
                };
                let last = maximum.saturating_add(*offset);
                let begin = group.positions.partition_point(|at| *at < first);
                let end = group.positions.partition_point(|at| *at <= last);
                if let Some(rows) = row_starts.as_ref().filter(|_| checks < end - begin) {
                    for (word, mask) in group.rows.iter().copied().enumerate() {
                        let mut active = mask & allowed[word];
                        while active != 0 {
                            let bit = active.trailing_zeros();
                            active &= active - 1;
                            for start in &rows[word * 64 + bit as usize] {
                                if !charge(work) {
                                    return Ok(None);
                                }
                                let Some(wanted) = start.checked_add(*offset) else {
                                    continue;
                                };
                                if group.positions.binary_search(&wanted).is_ok() {
                                    let mut matched = [0u64; 4];
                                    matched[word] = 1 << bit;
                                    if !insert(&mut next, *start, matched) {
                                        return Ok(None);
                                    }
                                }
                            }
                        }
                    }
                } else {
                    for position in &group.positions[begin..end] {
                        if !charge(work) {
                            return Ok(None);
                        }
                        let at = *position - offset;
                        if let Some(previous) = starts.get(&at) {
                            let rows =
                                std::array::from_fn(|word| previous[word] & group.rows[word]);
                            if rows.iter().any(|word| *word != 0) && !insert(&mut next, at, rows) {
                                return Ok(None);
                            }
                        }
                    }
                }
            }
        }
        starts = next;
    }
    Ok(Some(starts.values().fold([0u64; 4], |mut mask, rows| {
        for (mask, rows) in mask.iter_mut().zip(rows) {
            *mask |= rows;
        }
        mask
    })))
}

struct Cursor<'a> {
    cursor: tree::Cursor<'a>,
    hash: [u8; 32],
    codec: u8,
    head: Option<tree::Entry>,
}
impl<'a> Cursor<'a> {
    fn new(index: &'a Index, hash: [u8; 32]) -> Result<Self> {
        let mut cursor = tree::Cursor::seek(
            index.context(tree::Purpose::ProjectionPostings),
            index.manifest.positions.as_ref().ok_or(Error::Authority)?,
            &hash,
        )?;
        let head = cursor.next()?;
        Ok(Self {
            cursor,
            hash,
            codec: index.manifest.position_codec.unwrap_or(1),
            head,
        })
    }
    fn base(&self) -> Result<Option<u32>> {
        let Some(entry) = &self.head else {
            return Ok(None);
        };
        if !entry.key.starts_with(&self.hash) {
            return Ok(None);
        }
        if entry.key.len() != 40 {
            return Err(Error::Authority);
        }
        Ok(Some(parse_u32(&entry.key[32..36])? & !255))
    }
    fn blocks(
        &mut self,
        base: u32,
        rows: u64,
        remaining: &mut usize,
    ) -> Result<Option<Vec<Block>>> {
        while self.base()?.is_some_and(|current| current < base) {
            self.head = self.cursor.next()?;
        }
        let mut blocks = Vec::new();
        while self.base()? == Some(base) {
            let entry = self.head.take().ok_or(Error::Authority)?;
            if entry.value.first().is_none_or(|codec| *codec > self.codec) {
                return Err(Error::Authority);
            }
            let Some(left) = remaining.checked_sub(Block::decoded_charge(&entry.value)?) else {
                return Ok(None);
            };
            *remaining = left;
            let block = Block::decode(base, rows, &entry.value)?;
            let ordinal = parse_u32(&entry.key[32..36])?;
            let first = block
                .groups
                .iter()
                .flat_map(|group| {
                    group
                        .rows
                        .iter()
                        .enumerate()
                        .filter(|(_, mask)| **mask != 0)
                        .map(|(word, mask)| base + (word * 64) as u32 + mask.trailing_zeros())
                })
                .min()
                .ok_or(Error::Authority)?;
            if first != ordinal
                || block
                    .positions(ordinal)
                    .and_then(|positions| positions.first())
                    .copied()
                    != Some(parse_u32(&entry.key[36..])?)
            {
                return Err(Error::Authority);
            }
            blocks.push(block);
            self.head = self.cursor.next()?;
        }
        Ok(Some(blocks))
    }
}
impl Index {
    pub(super) fn literal_candidates(&self, needle: &str) -> Result<Option<Vec<u64>>> {
        if self.manifest.positions.is_none() {
            return Ok(None);
        }
        let Some(atoms) = query(needle) else {
            return Ok(None);
        };
        let mut cursors = atoms
            .iter()
            .map(|atom| Cursor::new(self, atom.hash))
            .collect::<Result<Vec<_>>>()?;
        let mut found = vec![0u64; (self.manifest.rows as usize).div_ceil(64)];
        let mut work = 8_000_000usize;
        let mut read = 0usize;
        while let Some(base) = cursors[0].base()? {
            let mut memory = 8 << 20;
            let mut lists = Vec::with_capacity(atoms.len());
            for cursor in &mut cursors {
                let before = memory;
                let Some(blocks) = cursor.blocks(base, self.manifest.rows, &mut memory)? else {
                    return Ok(None);
                };
                read = read.checked_add(before - memory).ok_or(Error::Bounds)?;
                if read > self.manifest.limits.original_bytes_per_query {
                    return Ok(None);
                }
                lists.push(blocks);
            }
            let refs: Vec<_> = atoms
                .iter()
                .zip(&lists)
                .map(|(atom, blocks)| (atom.offset, blocks.as_slice()))
                .collect();
            let Some(matched) = matches(base, &refs, &mut work)? else {
                return Ok(None);
            };
            for (offset, mask) in matched.into_iter().enumerate() {
                let at = base as usize / 64 + offset;
                if let Some(word) = found.get_mut(at) {
                    *word |= mask
                } else if mask != 0 {
                    return Err(Error::Authority);
                }
            }
        }
        Ok(Some(found))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_runs_prove_exact_unicode_case_and_separators() {
        let texts = [
            "battery electrode",
            "Battery electrode",
            "battery  electrode",
            "battery\telectrode",
            "x_battery electrode",
            "battery electrode_x",
            "λ_電池\r\n終",
            "λ_電池\n終",
            "___ ___",
            "battery electrode battery electrode",
            "e\u{301} exact\0終",
            "終\0exact",
        ];
        for needle in texts {
            let Some(atoms) = query(needle) else { continue };
            for text in texts {
                let indexed = analyze(text);
                let proved = indexed.get(&atoms[0].hash).is_some_and(|starts| {
                    starts.iter().any(|start| {
                        atoms.iter().all(|atom| {
                            indexed.get(&atom.hash).is_some_and(|positions| {
                                positions.binary_search(&(start + atom.offset)).is_ok()
                            })
                        })
                    })
                });
                let plan = scorer::Plan::new(
                    &format!("\"{needle}\""),
                    false,
                    None,
                    &BTreeSet::new(),
                    &BTreeMap::new(),
                );
                assert_eq!(
                    proved,
                    plan.candidate(0, true, text, true, &vec![1; plan.terms.len()]),
                    "{text:?} / {needle:?}"
                );
            }
        }
        for fallback in [" battery", "battery ", ".", "\0終", "\"battery\""] {
            assert!(query(fallback).is_none());
        }
    }
    #[test]
    fn bounded_position_fragments_round_trip_and_refuse_corruption() {
        let rows = vec![
            (256, vec![0, 8, 16]),
            (257, vec![0, 8, 16]),
            (511, vec![3, 99]),
        ];
        let encoded = Block::encode(256, &rows).unwrap();
        let block = Block::decode(256, 512, &encoded).unwrap();
        for (row, positions) in rows {
            assert_eq!(block.positions(row), Some(positions.as_slice()));
        }
        assert!(block.positions(258).is_none());
        for length in 0..encoded.len() {
            assert!(Block::decode(256, 512, &encoded[..length]).is_err());
        }
        let mut bad = encoded.clone();
        bad.push(0);
        assert!(Block::decode(256, 512, &bad).is_err());
        assert!(Block::decode(256, 511, &encoded).is_err());
        assert!(Block::encode(256, &[(257, vec![1, 1])]).is_err());
        assert!(Block::encode(256, &[(257, vec![MAX_TEXT_BYTES as u32])]).is_err());
    }

    #[test]
    fn mask_join_is_complete_across_fragments_and_refuses_partial_budget_proofs() {
        let texts = [
            "battery electrode",
            "Battery electrode",
            "battery  electrode",
            "battery electrode battery electrode",
            "λ_電池\r\n終",
            "λ_電池\n終",
        ];
        for needle in texts {
            let atoms = query(needle).unwrap();
            let mut lists = Vec::new();
            for atom in &atoms {
                let rows: Vec<_> = texts
                    .iter()
                    .enumerate()
                    .filter_map(|(row, text)| {
                        analyze(text)
                            .remove(&atom.hash)
                            .map(|positions| (row as u32, positions))
                    })
                    .collect();
                let mut blocks = Vec::new();
                // Separate fragments exercise repeated bases and row-mask union.
                for row in rows {
                    blocks.push(
                        Block::decode(0, texts.len() as u64, &Block::encode(0, &[row]).unwrap())
                            .unwrap(),
                    );
                }
                lists.push(blocks);
            }
            let refs: Vec<_> = atoms
                .iter()
                .zip(&lists)
                .map(|(atom, blocks)| (atom.offset, blocks.as_slice()))
                .collect();
            let matched = matches(0, &refs, &mut 1_000_000).unwrap().unwrap();
            for (row, text) in texts.iter().enumerate() {
                let plan = scorer::Plan::new(
                    &format!("\"{needle}\""),
                    false,
                    None,
                    &BTreeSet::new(),
                    &BTreeMap::new(),
                );
                assert_eq!(
                    matched[row / 64] & (1 << (row % 64)) != 0,
                    plan.candidate(0, true, text, true, &vec![1; plan.terms.len()])
                );
            }
            assert!(matches(0, &refs, &mut 0).unwrap().is_none());
        }
    }
    #[test]
    fn offset_join_caps_maps_and_handles_many_shared_fragment_positions_exactly() {
        let mut blocks = Vec::new();
        for chunk in 0..9 {
            let positions: Vec<_> = (chunk * 1000..(chunk + 1) * 1000).collect();
            blocks
                .push(Block::decode(0, 1, &Block::encode(0, &[(0, positions)]).unwrap()).unwrap());
        }
        assert!(
            matches(0, &[(0, &blocks)], &mut 1_000_000)
                .unwrap()
                .is_none()
        );
        let positions = |offset: u32| {
            (0..10)
                .map(|row| {
                    let list = vec![row + offset, 100 + row + offset, 900 + row + offset];
                    Block::decode(0, 10, &Block::encode(0, &[(row, list)]).unwrap()).unwrap()
                })
                .collect::<Vec<_>>()
        };
        let a = positions(0);
        let b = positions(8);
        let c = positions(19);
        let mut budget = 1_000_000;
        assert_eq!(
            matches(0, &[(0, &a), (8, &b), (19, &c)], &mut budget).unwrap(),
            Some([1023, 0, 0, 0])
        );
        assert!(budget >= 1_000_000 - 400);
    }
    #[test]
    fn offset_map_masks_match_exhaustive_randomized_row_position_joins() {
        let mut seed = 29u64;
        for _ in 0..16 {
            let mut lists = [Vec::new(), Vec::new(), Vec::new()];
            let mut expected = [0u64; 4];
            for row in 0..256u32 {
                let positions: [Vec<u32>; 3] = std::array::from_fn(|_| {
                    let mut values = Vec::new();
                    for at in 0..64u32 {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        if seed & 7 == 0 {
                            values.push(at);
                        }
                    }
                    if values.is_empty() {
                        values.push(0);
                    }
                    values
                });
                if positions[0].iter().any(|start| {
                    positions[1].binary_search(&(start + 3)).is_ok()
                        && positions[2].binary_search(&(start + 8)).is_ok()
                }) {
                    expected[row as usize / 64] |= 1 << (row % 64);
                }
                for (list, positions) in lists.iter_mut().zip(positions) {
                    list.push(
                        Block::decode(0, 256, &Block::encode(0, &[(row, positions)]).unwrap())
                            .unwrap(),
                    );
                }
            }
            let mut work = 1_000_000;
            assert_eq!(
                matches(
                    0,
                    &[(0, &lists[0]), (3, &lists[1]), (8, &lists[2])],
                    &mut work
                )
                .unwrap(),
                Some(expected)
            );
            assert!(work >= 900_000);
        }
    }
    #[test]
    fn sparse_mask_delta_codec_reads_legacy_and_refuses_noncanonical_values() {
        let mut legacy = vec![1, 2, 0];
        for word in [3u64, 0, 0, 0] {
            legacy.extend(word.to_le_bytes());
        }
        legacy.extend(3u16.to_le_bytes());
        for position in [0u32, 8, 16] {
            legacy.extend(position.to_le_bytes());
        }
        for word in [0u64, 0, 0, 1 << 63] {
            legacy.extend(word.to_le_bytes());
        }
        legacy.extend(2u16.to_le_bytes());
        for position in [99u32, 10_000] {
            legacy.extend(position.to_le_bytes());
        }
        let rows = vec![
            (0, vec![0, 8, 16]),
            (1, vec![0, 8, 16]),
            (255, vec![99, 10_000]),
        ];
        let compact = Block::encode(0, &rows).unwrap();
        assert_eq!(compact[0], 2);
        assert!(compact.len() < legacy.len() / 2);
        for bytes in [&legacy, &compact] {
            let block = Block::decode(0, 256, bytes).unwrap();
            for (row, positions) in &rows {
                assert_eq!(block.positions(*row), Some(positions.as_slice()));
            }
            assert!(Block::decoded_charge(bytes).unwrap() >= bytes.len() * 8 + 4096);
        }
        // An overlong absolute zero, duplicate sparse row offsets, a noncanonical
        // dense single-row mask, an unterminated delta, and an unknown codec.
        let mut dense = vec![2, 1, 0, 0];
        for word in [1u64, 0, 0, 0] {
            dense.extend(word.to_le_bytes());
        }
        dense.extend([1, 0, 0]);
        for bytes in [
            vec![2, 1, 0, 1, 0, 1, 0, 128, 0],
            vec![2, 1, 0, 2, 0, 0, 1, 0, 0],
            dense,
            vec![2, 1, 0, 1, 0, 1, 0, 128, 128, 128, 128],
            vec![3, 1, 0],
        ] {
            assert!(Block::decode(0, 256, &bytes).is_err());
        }
        for bytes in [vec![3, 1, 0], vec![2, 0, 0], vec![2, 1]] {
            assert!(Block::decoded_charge(&bytes).is_err());
        }
    }
    #[test]
    fn per_row_start_ranges_prove_heterogeneous_fragments_within_the_original_work_cap() {
        let rows: Vec<_> = (0..256u32)
            .map(|row| (row, if row == 0 { vec![0, 50_000] } else { vec![0] }))
            .collect();
        let first = vec![Block::decode(0, 256, &Block::encode(0, &rows).unwrap()).unwrap()];
        let others: Vec<_> = (0..256u32)
            .map(|row| {
                let positions = std::iter::once(8).chain(1000 + row..3000 + row).collect();
                Block::decode(0, 256, &Block::encode(0, &[(row, positions)]).unwrap()).unwrap()
            })
            .collect();
        let mut work = 20_000;
        assert_eq!(
            matches(0, &[(0, &first), (8, &others)], &mut work).unwrap(),
            Some([u64::MAX; 4])
        );
        assert!(work >= 10_000);
    }
    #[test]
    fn row_start_lookup_avoids_irrelevant_positions_inside_each_rows_span() {
        let rows: Vec<_> = (0..256u32).map(|row| (row, vec![0, 10_000])).collect();
        let first = vec![Block::decode(0, 256, &Block::encode(0, &rows).unwrap()).unwrap()];
        let others: Vec<_> = (0..256u32)
            .map(|row| {
                let positions = std::iter::once(8)
                    .chain(1000 + row..3000 + row)
                    .chain(std::iter::once(10_008))
                    .collect();
                Block::decode(0, 256, &Block::encode(0, &[(row, positions)]).unwrap()).unwrap()
            })
            .collect();
        let mut work = 20_000;
        assert_eq!(
            matches(0, &[(0, &first), (8, &others)], &mut work).unwrap(),
            Some([u64::MAX; 4])
        );
        assert!(work >= 10_000);
    }
}
