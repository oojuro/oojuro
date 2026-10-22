//! DATA roots only. Managed control owns publication, ancestry and original ACKs.
use crate::record::paged::tree::{self, Context, Entry, KeyClass, Purpose, Root};
use crate::record::{Error, Result};
use oojuro_store::managed_pack::{DictionaryRef, Locator, PackRef};
use serde::{Deserialize, Serialize};

/// Small authenticated bundle; all per-frame/file/dictionary entries remain on disk.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::record) struct Roots {
    pub format: u32,
    pub frame: Root,
    pub file: Root,
    pub dictionary: Root,
}
#[derive(Clone)]
pub(in crate::record) struct Contexts<'a> {
    pub frame: Context<'a>,
    pub file: Context<'a>,
    pub dictionary: Context<'a>,
}
impl Contexts<'_> {
    fn check(&self) -> Result<Vec<u8>> {
        let mut binding = None;
        for (c, p) in [
            (&self.frame, Purpose::Frame),
            (&self.file, Purpose::File),
            (&self.dictionary, Purpose::Dictionary),
        ] {
            if c.purpose != p || c.class != KeyClass::Data || c.sort_of.is_some() {
                return Err(Error::Scope);
            }
            let b = c.check()?;
            if binding.as_ref().is_some_and(|v| v != &b) {
                return Err(Error::Conflict);
            }
            binding = Some(b);
        }
        if self.frame.identity == self.file.identity
            || self.frame.identity == self.dictionary.identity
            || self.file.identity == self.dictionary.identity
        {
            return Err(Error::Scope);
        }
        Ok(binding.unwrap())
    }
}
impl Roots {
    pub fn empty(c: &Contexts<'_>) -> Result<Self> {
        c.check()?;
        Ok(Self {
            format: 1,
            frame: tree::Builder::new(c.frame.clone())?.finish()?,
            file: tree::Builder::new(c.file.clone())?.finish()?,
            dictionary: tree::Builder::new(c.dictionary.clone())?.finish()?,
        })
    }
    fn check(&self, c: &Contexts<'_>) -> Result<Vec<u8>> {
        if self.format != 1 {
            return Err(Error::Authority);
        }
        let b = c.check()?;
        for (root, ctx) in [
            (&self.frame, &c.frame),
            (&self.file, &c.file),
            (&self.dictionary, &c.dictionary),
        ] {
            tree::validate_ancestor_root(ctx, root)?;
        }
        Ok(b)
    }
    pub fn locate(&self, c: &Contexts<'_>, physical_seq: u64) -> Result<Locator> {
        let b = self.check(c)?;
        let e = tree::floor_ancestor(c.frame.clone(), &self.frame, &physical_seq.to_be_bytes())?
            .ok_or(Error::Invalid)?;
        let l = Locator::decode(&e.value).map_err(|_| Error::Authority)?;
        if e.key != l.first_physical_seq.to_be_bytes()
            || physical_seq < l.first_physical_seq
            || physical_seq >= l.first_physical_seq + u64::from(l.count)
        {
            return Err(Error::Invalid);
        }
        let p = self.pack(c, l.file_id)?;
        if l.frame_ordinal >= p.frame_count
            || l.first_physical_seq < p.first_physical_seq
            || l.first_physical_seq + u64::from(l.count) > p.first_physical_seq + p.record_count
            || l.offset + 8 + u64::from(l.payload_len) > p.byte_len
        {
            return Err(Error::Authority);
        }
        if c.check()? != b {
            return Err(Error::Conflict);
        }
        Ok(l)
    }
    pub fn pack(&self, c: &Contexts<'_>, file_id: u64) -> Result<PackRef> {
        let b = self.check(c)?;
        let bytes = tree::get_ancestor(c.file.clone(), &self.file, &file_id.to_be_bytes())?
            .ok_or(Error::Invalid)?;
        let p = PackRef::decode(&bytes).map_err(|_| Error::Authority)?;
        if p.file_id != file_id {
            return Err(Error::Authority);
        }
        if c.check()? != b {
            return Err(Error::Conflict);
        }
        Ok(p)
    }
    pub fn dictionary(&self, c: &Contexts<'_>, id: u32) -> Result<Option<DictionaryRef>> {
        let b = self.check(c)?;
        let out = tree::get_ancestor(c.dictionary.clone(), &self.dictionary, &id.to_be_bytes())?
            .map(|b| DictionaryRef::decode(&b).map_err(|_| Error::Authority))
            .transpose()?;
        if out.as_ref().is_some_and(|d| d.id != id) {
            return Err(Error::Authority);
        }
        if c.check()? != b {
            return Err(Error::Conflict);
        }
        Ok(out)
    }
    /// Streams complete data-file/dictionary inventory; it never authorizes unlink.
    /// Managed owner must first publish exclusion/tombstone and prune DATA trees.
    pub fn inventory(
        &self,
        c: &Contexts<'_>,
        visit: &mut dyn FnMut(Inventory) -> Result<()>,
    ) -> Result<()> {
        let b = self.check(c)?;
        let mut cursor = tree::Cursor::seek_ancestor(c.file.clone(), &self.file, &[])?;
        while let Some(e) = cursor.next()? {
            let p = PackRef::decode(&e.value).map_err(|_| Error::Authority)?;
            if e.key != p.file_id.to_be_bytes() {
                return Err(Error::Authority);
            }
            visit(Inventory::Pack(p))?;
        }
        let mut cursor = tree::Cursor::seek_ancestor(c.dictionary.clone(), &self.dictionary, &[])?;
        while let Some(e) = cursor.next()? {
            let d = DictionaryRef::decode(&e.value).map_err(|_| Error::Authority)?;
            if e.key != d.id.to_be_bytes() {
                return Err(Error::Authority);
            }
            visit(Inventory::Dictionary(d))?;
        }
        if c.check()? != b {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    /// Includes reused COW pages, not only pages born in the newest generation.
    pub fn metadata_pages(
        &self,
        c: &Contexts<'_>,
        visit: &mut dyn FnMut(Purpose, &tree::Reference) -> Result<()>,
    ) -> Result<()> {
        let b = self.check(c)?;
        for (root, ctx) in [
            (&self.frame, &c.frame),
            (&self.file, &c.file),
            (&self.dictionary, &c.dictionary),
        ] {
            tree::visit_ancestor_pages(ctx.clone(), root, |r: &tree::Reference| {
                visit(ctx.purpose, r)
            })?;
        }
        if c.check()? != b {
            return Err(Error::Conflict);
        }
        Ok(())
    }
}
pub(in crate::record) enum Inventory {
    Pack(PackRef),
    Dictionary(DictionaryRef),
}
pub(in crate::record) struct Editor<'a> {
    contexts: Contexts<'a>,
    frame: tree::Editor<'a>,
    file: tree::Editor<'a>,
    dictionary: tree::Editor<'a>,
    binding: Vec<u8>,
    pending: Option<Pending>,
    poisoned: bool,
}
struct Pending {
    file: u64,
    first: u64,
    next: u64,
    frames: u64,
    end: u64,
}
impl<'a> Editor<'a> {
    pub fn new(c: Contexts<'a>, roots: Roots) -> Result<Self> {
        let binding = roots.check(&c)?;
        Ok(Self {
            frame: tree::Editor::new(c.frame.clone(), roots.frame)?,
            file: tree::Editor::new(c.file.clone(), roots.file)?,
            dictionary: tree::Editor::new(c.dictionary.clone(), roots.dictionary)?,
            contexts: c,
            binding,
            pending: None,
            poisoned: false,
        })
    }
    fn check(&self) -> Result<()> {
        if self.poisoned {
            return Err(Error::Unavailable);
        }
        if self.contexts.check()? != self.binding {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    pub fn insert_frame(&mut self, l: &Locator) -> Result<()> {
        let result = (|| {
            self.check()?;
            let value = l.encode().map_err(|_| Error::Bounds)?;
            if let Some(p) = &self.pending {
                if p.file != l.file_id
                    || p.next != l.first_physical_seq
                    || p.frames != l.frame_ordinal
                    || p.end != l.offset
                {
                    return Err(Error::Conflict);
                }
            } else if l.frame_ordinal != 0 || l.offset != 304 {
                return Err(Error::Conflict);
            }
            if self.pending.is_none() {
                if let Some(e) = self.frame.lookup_floor(&u64::MAX.to_be_bytes())? {
                    let last = Locator::decode(&e.value).map_err(|_| Error::Authority)?;
                    if l.first_physical_seq < last.first_physical_seq + u64::from(last.count) {
                        return Err(Error::Conflict);
                    }
                }
            }
            if self
                .frame
                .lookup(&l.first_physical_seq.to_be_bytes())?
                .is_some()
            {
                return Err(Error::Conflict);
            }
            self.frame.insert_immutable(Entry::new(
                l.first_physical_seq.to_be_bytes().to_vec(),
                value,
            )?)?;
            let p = self.pending.get_or_insert(Pending {
                file: l.file_id,
                first: l.first_physical_seq,
                next: l.first_physical_seq,
                frames: 0,
                end: l.offset,
            });
            p.next = l.first_physical_seq + u64::from(l.count);
            p.frames += 1;
            p.end = l.offset + 8 + u64::from(l.payload_len);
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    pub fn insert_pack(&mut self, p: &PackRef) -> Result<()> {
        let result = (|| {
            self.check()?;
            p.validate().map_err(|_| Error::Bounds)?;
            let pending = self.pending.as_ref().ok_or(Error::Conflict)?;
            if pending.file != p.file_id
                || pending.first != p.first_physical_seq
                || pending.next != p.first_physical_seq + p.record_count
                || pending.frames != p.frame_count
                || pending.end != p.byte_len
            {
                return Err(Error::Conflict);
            }
            if let Some(d) = &p.dictionary {
                if self.dictionary.lookup(&d.id.to_be_bytes())?.as_deref()
                    != Some(d.encode().map_err(|_| Error::Bounds)?.as_slice())
                {
                    return Err(Error::Authority);
                }
            }
            self.file.insert_immutable(Entry::new(
                p.file_id.to_be_bytes().to_vec(),
                p.encode().map_err(|_| Error::Bounds)?,
            )?)?;
            self.pending = None;
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    pub fn insert_dictionary(&mut self, d: &DictionaryRef) -> Result<()> {
        let result = (|| {
            self.check()?;
            self.dictionary.insert_immutable(Entry::new(
                d.id.to_be_bytes().to_vec(),
                d.encode().map_err(|_| Error::Bounds)?,
            )?)
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    pub fn finish(self) -> Result<Roots> {
        self.check()?;
        if self.pending.is_some() {
            return Err(Error::Conflict);
        }
        let roots = Roots {
            format: 1,
            frame: self.frame.finish()?,
            file: self.file.finish()?,
            dictionary: self.dictionary.finish()?,
        };
        roots.check(&self.contexts)?;
        Ok(roots)
    }
}
#[cfg(test)]
#[path = "physical/tests.rs"]
mod tests;
