//! Record-only codec seam: no capture dedupe, authority writes or exported carriers.
use super::segments::codec;
use super::*;

pub(crate) fn fingerprint(id: u64) -> [u8; 12] {
    blake3::hash(id.to_string().as_bytes()).as_bytes()[..12]
        .try_into()
        .unwrap()
}
pub(crate) fn candidate_bytes(index: &Index) -> Result<usize> {
    index.postings.iter().try_fold(
        index
            .documents
            .len()
            .checked_mul(32)
            .ok_or(Error::Invalid)?,
        |n, (t, p)| {
            n.checked_add(64)
                .and_then(|n| n.checked_add(t.len()))
                .and_then(|n| p.len().checked_mul(8).and_then(|v| n.checked_add(v)))
                .ok_or(Error::Invalid)
        },
    )
}
pub(crate) fn segment_bytes(segment: &codec::Segment) -> Result<usize> {
    segment.postings.iter().try_fold(
        segment.rows.len().checked_mul(32).ok_or(Error::Invalid)?,
        |n, (t, p)| {
            n.checked_add(64)
                .and_then(|n| n.checked_add(t.len()))
                .and_then(|n| p.len().checked_mul(8).and_then(|v| n.checked_add(v)))
                .ok_or(Error::Invalid)
        },
    )
}
pub(crate) fn snapshot(index: &Index, start: usize, count: usize) -> Result<codec::Segment> {
    let end = start.checked_add(count).ok_or(Error::Invalid)?;
    let docs = index.documents.get(start..end).ok_or(Error::Invalid)?;
    let rows = docs
        .iter()
        .map(|d| codec::Row {
            id: d.event.id,
            length: d.length,
            scope: d.scope,
            fingerprint: fingerprint(d.event.id),
        })
        .collect();
    let mut postings = BTreeMap::new();
    for (term, list) in &index.postings {
        let selected: Vec<_> = list
            .iter()
            .skip_while(|(r, _)| (*r as usize) < start)
            .take_while(|(r, _)| (*r as usize) < end)
            .map(|(r, tf)| ((*r as usize - start) as u32, *tf))
            .collect();
        if !selected.is_empty() {
            postings.insert(term.clone(), selected);
        }
    }
    Ok(codec::Segment {
        start: start as u32,
        rows,
        postings,
    })
}
pub(crate) fn hydrate(index: &mut Index, segment: codec::Segment) -> Result<()> {
    let start = segment.start as usize;
    let end = start
        .checked_add(segment.rows.len())
        .ok_or(Error::Invalid)?;
    if end > index.documents.len() {
        return Err(Error::Authority);
    }
    let mut lengths = vec![0u64; segment.rows.len()];
    let mut scopes = vec![0u64; segment.rows.len()];
    for (offset, row) in segment.rows.iter().enumerate() {
        let doc = &mut index.documents[start + offset];
        if row.id != doc.event.id || row.fingerprint != fingerprint(doc.event.id) {
            return Err(Error::Authority);
        }
        doc.length = row.length;
        doc.scope = row.scope;
    }
    for (term, list) in segment.postings {
        let output = index.postings.entry(term).or_default();
        for (local, tf) in list {
            let offset = local as usize;
            if offset >= lengths.len() || tf == 0 {
                return Err(Error::Invalid);
            }
            lengths[offset] = lengths[offset]
                .checked_add(u64::from(tf))
                .ok_or(Error::Invalid)?;
            scopes[offset] = scopes[offset].checked_add(1).ok_or(Error::Invalid)?;
            output.push((u32::try_from(start + offset).map_err(|_| Error::Invalid)?, tf));
        }
    }
    for (offset, doc) in index.documents[start..end].iter().enumerate() {
        if lengths[offset] != u64::from(doc.length)
            || scopes[offset] != u64::from(doc.scope)
            || ((!doc.event.indexed || doc.event.text.is_empty())
                && (doc.length != 0 || doc.scope != 0))
        {
            return Err(Error::Invalid);
        }
    }
    Ok(())
}
