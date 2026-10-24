//! Disposable authenticated visibility metadata; never original-record authority.
//! Full cache open checks these fields against the independently verified source.
use super::*;
pub(super) const MAX_BYTES: usize = 16 << 20;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reference {
    pub bytes: usize,
    pub decoded: usize,
    pub digest: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Page {
    format: u32,
    pub rows: Vec<Row>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Row {
    id: u64,
    valid_at: i64,
    recorded_at: i64,
    key: Option<String>,
    kind: Kind,
    tool: Option<String>,
    status: Option<Status>,
    agent_id: String,
    indexed: bool,
}
impl Row {
    fn from(r: &Assigned) -> Self {
        Self {
            id: r.id,
            valid_at: r.input.valid_at,
            recorded_at: r.recorded_at,
            key: r.input.key.clone(),
            kind: r.input.kind,
            tool: r.input.tool.clone(),
            status: r.input.status,
            agent_id: r.input.agent_id.clone(),
            indexed: r.input.indexed,
        }
    }
    pub(super) fn matches(&self, r: &Assigned) -> bool {
        self.id == r.id
            && self.valid_at == r.input.valid_at
            && self.recorded_at == r.recorded_at
            && self.key == r.input.key
            && self.kind == r.input.kind
            && self.tool == r.input.tool
            && self.status == r.input.status
            && self.agent_id == r.input.agent_id
            && self.indexed == r.input.indexed
    }
    pub(super) fn assigned(self) -> Assigned {
        Assigned {
            id: self.id,
            recorded_at: self.recorded_at,
            input: Input {
                valid_at: self.valid_at,
                key: self.key,
                kind: self.kind,
                tool: self.tool,
                status: self.status,
                agent_id: self.agent_id,
                parent_id: None,
                exit: None,
                text: String::new(),
                attrs: BTreeMap::new(),
                indexed: self.indexed,
            },
        }
    }
}
fn accounted(rows: &[Row]) -> Result<usize> {
    let mut bytes = rows
        .len()
        .checked_mul(std::mem::size_of::<Row>() * 2)
        .ok_or(Error::Bounds)?;
    for r in rows {
        // Charge spare vector/string capacity conservatively, independently of JSON compression.
        for s in [
            Some(r.agent_id.as_str()),
            r.key.as_deref(),
            r.tool.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            bytes = bytes
                .checked_add(
                    s.len()
                        .checked_mul(2)
                        .and_then(|n| n.checked_add(64))
                        .ok_or(Error::Bounds)?,
                )
                .ok_or(Error::Bounds)?;
        }
    }
    if bytes > MAX_BYTES {
        return Err(Error::Bounds);
    }
    Ok(bytes)
}
fn aad(m: &Manifest, r: &super::Reference, v: &Reference) -> Result<Vec<u8>> {
    let mut out = b"oojuro/record-paged/visibility/v1\0".to_vec();
    out.extend(segment_aad(m, r)?);
    out.extend((v.bytes as u64).to_le_bytes());
    out.extend((v.decoded as u64).to_le_bytes());
    Ok(out)
}
pub(super) fn build(
    reader: &Reader,
    m: &Manifest,
    r: &super::Reference,
) -> Result<(Reference, Vec<u8>)> {
    let mut page = Page {
        format: 1,
        rows: Vec::new(),
    };
    reader.scan(
        &r.prefix,
        r.start as usize,
        r.start as usize + r.rows as usize,
        |row| {
            page.rows.push(Row::from(row));
            Ok(true)
        },
    )?;
    if page.rows.len() != r.rows as usize {
        return Err(Error::Authority);
    }
    let bytes = json(&page, MAX_BYTES)?;
    let mut reference = Reference {
        bytes: bytes.len(),
        decoded: accounted(&page.rows)?,
        digest: [0; 32],
    };
    let sealed = reader.seal(&aad(m, r, &reference)?, &bytes);
    reference.digest = *blake3::hash(&sealed).as_bytes();
    Ok((reference, sealed))
}
pub(super) fn load(
    reader: &Reader,
    path: &Path,
    m: &Manifest,
    r: &super::Reference,
) -> Result<Option<Page>> {
    let Some(v) = &r.visibility else {
        return Ok(None);
    };
    if v.bytes == 0 || v.bytes > MAX_BYTES || v.decoded > MAX_BYTES {
        return Err(Error::Bounds);
    }
    let sealed = read_bounded(
        &path.join(format!("vis-{}.idx", r.id)),
        MAX_BYTES + SEAL_OVERHEAD,
    )?;
    if sealed.len() != v.bytes + SEAL_OVERHEAD || *blake3::hash(&sealed).as_bytes() != v.digest {
        return Err(Error::Authority);
    }
    let plain = reader.unseal(&aad(m, r, v)?, &sealed)?;
    let page: Page = serde_json::from_slice(&plain).map_err(|_| Error::Authority)?;
    if page.format != 1
        || page.rows.len() != r.rows as usize
        || page.rows.len() > codec::MAX_ROWS
        || accounted(&page.rows)? != v.decoded
    {
        return Err(Error::Authority);
    }
    for (at, row) in page.rows.iter().enumerate() {
        if row.id
            != reader
                .identity()
                .first_id
                .checked_add(r.start as u64)
                .and_then(|n| n.checked_add(at as u64))
                .ok_or(Error::Bounds)?
            || !name(&row.agent_id, 256)
            || row.key.as_ref().is_some_and(|v| !name(v, 1024))
            || row.tool.as_ref().is_some_and(|v| !name(v, 1024))
        {
            return Err(Error::Authority);
        }
    }
    Ok(Some(page))
}
pub(super) fn check(
    reader: &Reader,
    path: &Path,
    m: &Manifest,
    r: &super::Reference,
) -> Result<()> {
    let Some(page) = load(reader, path, m, r)? else {
        return Ok(());
    };
    let mut at = 0;
    reader.scan(
        &r.prefix,
        r.start as usize,
        r.start as usize + r.rows as usize,
        |row| {
            if !page.rows.get(at).ok_or(Error::Authority)?.matches(row) {
                return Err(Error::Authority);
            }
            at += 1;
            Ok(true)
        },
    )?;
    if at != page.rows.len() {
        return Err(Error::Authority);
    }
    Ok(())
}
