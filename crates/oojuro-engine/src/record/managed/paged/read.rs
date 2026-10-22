//! Bounded cursor pages against one independently current managed root.
use super::*;
use driver::{ManagedPagedLog, read_assigned};
use schema::RootName as N;
const CURSOR_AAD: &[u8] = b"oojuro/managed/paged/read-cursor/v1\0";
#[derive(Clone, Copy)]
pub struct ReadLimits {
    pub entries: usize,
    pub bytes: usize,
    /// Bounds examined records and key-version probes, including filtered-out rows.
    pub work: usize,
}
impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            entries: 100,
            bytes: 1 << 20,
            work: 10000,
        }
    }
}
#[derive(Serialize)]
pub struct PagedRecords {
    pub binding: Binding,
    pub records: Vec<Record>,
    pub next: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    binding: Binding,
    as_of: AsOf,
    filter: Filter,
    versions: Versions,
    next_id: u64,
}
impl ManagedPagedLog {
    /// Canonical key winner; a stored-only winner remains visible and never reveals an older version.
    pub fn get_key(
        &self,
        scope: Scope<'_>,
        key: &str,
        as_of: AsOf,
        max_bytes: usize,
        max_work: usize,
    ) -> Result<Option<Record>> {
        if scope.writer != self.identity.writer || scope.owner != self.identity.agent {
            return Err(Error::Scope);
        }
        if !name(key, 1024)
            || as_of.from > as_of.valid_at
            || max_bytes == 0
            || max_bytes > MAX_OUTPUT_BYTES
            || max_work == 0
            || max_work > 1_000_000
        {
            return Err(Error::Bounds);
        }
        let owner = self.control.lock()?;
        self.current(&owner)?;
        let root = schema::load(
            &self.control.directory,
            owner.control_cipher(),
            &owner.state,
        )?;
        let source = self.source(&owner, &root)?;
        let mut work = 0;
        let row = winner(&source, &self.directory, key, as_of, &mut work, max_work)?
            .map(|id| read_assigned(&source, &self.directory, id))
            .transpose()?
            .flatten()
            .filter(|r| r.input.valid_at >= as_of.from)
            .map(|r| r.record(&self.identity.writer, &self.identity.adapter));
        json(&row, max_bytes)?;
        self.current(&owner)?;
        Ok(row)
    }
    pub fn read(
        &self,
        scope: Scope<'_>,
        as_of: AsOf,
        filter: &Filter,
        versions: Versions,
        after: Option<&str>,
        limits: ReadLimits,
    ) -> Result<PagedRecords> {
        if scope.writer != self.identity.writer || scope.owner != self.identity.agent {
            return Err(Error::Scope);
        }
        filter.validate()?;
        if as_of.from > as_of.valid_at
            || limits.entries == 0
            || limits.entries > 1000
            || limits.bytes == 0
            || limits.bytes > MAX_OUTPUT_BYTES
            || limits.work == 0
            || limits.work > 1_000_000
        {
            return Err(Error::Bounds);
        }
        let owner = self.control.lock()?;
        self.current(&owner)?;
        let root = schema::load(
            &self.control.directory,
            owner.control_cipher(),
            &owner.state,
        )?;
        let pin = binding(
            &owner.state,
            owner.checkpoint.floor().map_err(|_| Error::Authority)?,
        );
        let start = if let Some(raw) = after {
            if raw.len() > 32 << 10
                || raw.len() % 2 != 0
                || !raw.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(Error::Bounds);
            }
            let bytes = (0..raw.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&raw[i..i + 2], 16).map_err(|_| Error::Invalid))
                .collect::<Result<Vec<_>>>()?;
            let value: Cursor =
                serde_json::from_slice(&self.cipher.open_dict(CURSOR_AAD, &bytes).map_err(io)?)
                    .map_err(|_| Error::Authority)?;
            if value.binding != pin
                || value.as_of != as_of
                || value.filter != *filter
                || value.versions != versions
                || value.next_id < self.identity.first_id
                || value.next_id > root.next_id
            {
                return Err(Error::Authority);
            }
            value.next_id
        } else {
            self.identity.first_id
        };
        let source = self.source(&owner, &root)?;
        let data = root.data.as_ref().ok_or(Error::Authority)?;
        let mut cursor =
            tree::Cursor::seek_ancestor(source.ctx(N::Record), &data.record, &start.to_be_bytes())?;
        let mut records = Vec::new();
        let mut bytes = 0usize;
        let mut work = 0usize;
        let mut next_id = start;
        let mut done = false;
        while work < limits.work && records.len() < limits.entries {
            let Some(entry) = cursor.next()? else {
                done = true;
                break;
            };
            let id = u64::from_be_bytes(entry.key.try_into().map_err(|_| Error::Authority)?);
            charge(&mut work, limits.work)?;
            let Some(row) = read_assigned(&source, &self.directory, id)? else {
                next_id = id.checked_add(1).ok_or(Error::Bounds)?;
                continue;
            };
            let v = &row.input;
            let visible = v.valid_at <= as_of.valid_at && row.recorded_at <= as_of.known_at;
            // Select key winner before lower-bound/filter/indexed eligibility.
            let current = if visible && versions == Versions::Current {
                if let Some(key) = &v.key {
                    winner(&source, &self.directory, key, as_of, &mut work, limits.work)?
                        == Some(id)
                } else {
                    true
                }
            } else {
                true
            };
            if visible && current && v.valid_at >= as_of.from && filter.keep(&row) {
                let r = row.record(&self.identity.writer, &self.identity.adapter);
                let n = json(&r, limits.bytes)?.len();
                if bytes.checked_add(n).is_none_or(|n| n > limits.bytes) {
                    if records.is_empty() {
                        return Err(Error::Bounds);
                    }
                    break;
                }
                bytes += n;
                records.push(r);
            }
            next_id = id.checked_add(1).ok_or(Error::Bounds)?;
        }
        self.current(&owner)?;
        let next = if done || next_id >= root.next_id {
            None
        } else {
            let value = Cursor {
                binding: pin.clone(),
                as_of,
                filter: filter.clone(),
                versions,
                next_id,
            };
            let bytes = self.cipher.seal_dict(CURSOR_AAD, &json(&value, 12 << 10)?);
            Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
        };
        let page = PagedRecords {
            binding: pin,
            records,
            next,
        };
        json(&page, limits.bytes)?;
        #[cfg(test)]
        self.phase(Phase::BeforeAck);
        self.current(&owner)?;
        Ok(page)
    }
}
fn charge(work: &mut usize, max: usize) -> Result<()> {
    *work = work.checked_add(1).ok_or(Error::Bounds)?;
    if *work > max {
        return Err(Error::Bounds);
    }
    Ok(())
}
fn winner(
    source: &source::Source<'_>,
    directory: &Path,
    key: &str,
    as_of: AsOf,
    work: &mut usize,
    max: usize,
) -> Result<Option<u64>> {
    let data = source.root.data.as_ref().ok_or(Error::Authority)?;
    let prefix = *blake3::keyed_hash(
        &source.root.intent,
        &json(&("key-v3", key), MAX_BATCH_BYTES)?,
    )
    .as_bytes();
    let mut upper = prefix.to_vec();
    upper.extend((as_of.valid_at as u64 ^ (1 << 63)).to_be_bytes());
    upper.extend(u64::MAX.to_be_bytes());
    loop {
        charge(work, max)?;
        let Some(e) = tree::floor_ancestor(source.ctx(N::KeyVersion), &data.key_version, &upper)?
        else {
            return Ok(None);
        };
        if !e.key.starts_with(&prefix) {
            return Ok(None);
        }
        if e.key.len() != 48 || e.value.len() != 8 {
            return Err(Error::Authority);
        }
        let id = u64::from_be_bytes(e.value.try_into().map_err(|_| Error::Authority)?);
        if let Some(row) = read_assigned(source, directory, id)? {
            if row.input.key.as_deref() != Some(key) {
                return Err(Error::Authority);
            }
            if row.recorded_at <= as_of.known_at && row.input.valid_at <= as_of.valid_at {
                return Ok(Some(id));
            }
        }
        upper = e.key;
        let mut decremented = false;
        for b in upper.iter_mut().rev() {
            if *b != 0 {
                *b -= 1;
                decremented = true;
                break;
            }
            *b = 255;
        }
        if !decremented || !upper.starts_with(&prefix) {
            return Ok(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::driver::tests::{control, log};
    use super::*;
    use crate::record::managed::tests::{Fixture, identity, input};
    fn scope() -> Scope<'static> {
        Scope {
            writer: "synthetic-managed-owner",
            owner: 7,
        }
    }
    #[test]
    fn winner_precedes_filter_from_and_indexed_and_cursor_is_exact_current() {
        let f = Fixture::new();
        let c = control(&f, true);
        let mut l = log(&f, c, true);
        let mut old = input("visible", "old key version");
        old.key = Some("real".into());
        old.valid_at = 5;
        let mut newer = input("hidden", "current key winner");
        newer.key = old.key.clone();
        newer.valid_at = 10;
        newer.indexed = false;
        let mut other = input("visible", "unkeyed");
        other.valid_at = 20;
        l.append("versions", &[old, newer, other]).unwrap();
        let filtered = Filter {
            agent_id: Some("visible".into()),
            ..Filter::default()
        };
        let page = l
            .read(
                scope(),
                AsOf::default(),
                &filtered,
                Versions::Current,
                None,
                ReadLimits::default(),
            )
            .unwrap();
        assert_eq!(
            page.records.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![11]
        );
        let all = l
            .read(
                scope(),
                AsOf::default(),
                &filtered,
                Versions::All,
                None,
                ReadLimits::default(),
            )
            .unwrap();
        assert_eq!(
            all.records.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![9, 11]
        );
        let page = l
            .read(
                scope(),
                AsOf::default(),
                &Filter::default(),
                Versions::All,
                None,
                ReadLimits {
                    entries: 1,
                    ..ReadLimits::default()
                },
            )
            .unwrap();
        let cursor = page.next.unwrap();
        let next = l
            .read(
                scope(),
                AsOf::default(),
                &Filter::default(),
                Versions::All,
                Some(&cursor),
                ReadLimits::default(),
            )
            .unwrap();
        assert_eq!(
            next.records.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![10, 11]
        );
        assert!(
            l.read(
                Scope {
                    writer: scope().writer,
                    owner: 8
                },
                AsOf::default(),
                &Filter::default(),
                Versions::All,
                None,
                ReadLimits::default()
            )
            .is_err()
        );
        assert!(
            l.read(
                scope(),
                AsOf::default(),
                &Filter::default(),
                Versions::All,
                Some("é"),
                ReadLimits::default()
            )
            .is_err()
        );
        l.append("tail", &[input("visible", "new tail")]).unwrap();
        assert!(
            l.read(
                scope(),
                AsOf::default(),
                &Filter::default(),
                Versions::All,
                Some(&cursor),
                ReadLimits::default()
            )
            .is_err()
        );
    }
    #[test]
    fn known_time_selects_original_version_and_output_bound_fails_before_return() {
        let f = Fixture::new();
        let c = control(&f, true);
        let mut l = log(&f, c, true);
        let mut old = input("one", "first");
        old.key = Some("k".into());
        old.valid_at = 10;
        let ack = l.append("one", &[old]).unwrap();
        let mut new = input("two", "second");
        new.key = Some("k".into());
        new.valid_at = 20;
        l.append("two", &[new]).unwrap();
        let at = AsOf {
            known_at: ack.commit.recorded_at[0],
            ..AsOf::default()
        };
        let p = l
            .read(
                scope(),
                at,
                &Filter::default(),
                Versions::Current,
                None,
                ReadLimits::default(),
            )
            .unwrap();
        assert_eq!(p.records.iter().map(|r| r.id).collect::<Vec<_>>(), vec![9]);
        assert!(
            l.read(
                scope(),
                at,
                &Filter::default(),
                Versions::Current,
                None,
                ReadLimits {
                    bytes: 1,
                    ..ReadLimits::default()
                }
            )
            .is_err()
        );
        assert!(
            l.read(
                scope(),
                at,
                &Filter::default(),
                Versions::Current,
                None,
                ReadLimits {
                    work: 1,
                    ..ReadLimits::default()
                }
            )
            .is_err()
        );
        let _ = identity();
    }
    #[test]
    fn retained_witness_change_at_final_serialized_page_boundary_refuses_egress() {
        let f = Fixture::new();
        let c = control(&f, true);
        let mut l = log(&f, c.clone(), true);
        l.append("original", &[input("private", "protected final page")])
            .unwrap();
        let witness = c.witness_directory.join("CURRENT");
        l.hook = Some(Arc::new(move |at| {
            if at == Phase::BeforeAck {
                let mut bytes = fs::read(&witness).unwrap();
                bytes[0] ^= 1;
                fs::write(&witness, bytes).unwrap();
            }
        }));
        assert!(
            l.read(
                scope(),
                AsOf::default(),
                &Filter::default(),
                Versions::All,
                None,
                ReadLimits::default()
            )
            .is_err()
        );
    }
}
