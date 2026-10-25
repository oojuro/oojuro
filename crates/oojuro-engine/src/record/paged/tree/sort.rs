//! Bounded encrypted run sorting. Run references live in an encrypted sequential spool,
//! never a population-sized Vec. At most32 cursors and4MiB input entries are resident.
use super::*;
use std::{cmp::Ordering, collections::BinaryHeap};
const RUN_BYTES: usize = 4 << 20;
const FAN_IN: usize = 32;
const SPOOL_BYTES: usize = 64 << 10;
#[derive(Clone, Copy)]
struct Spool {
    id: [u8; 32],
    count: u64,
}
fn spool_path(ctx: &Context<'_>, id: [u8; 32]) -> PathBuf {
    ctx.directory.join(format!("queue-{}.bin", hex(&id)))
}
fn spool_aad(ctx: &Context<'_>, id: [u8; 32], sequence: u64, binding: &[u8]) -> Vec<u8> {
    let mut a = b"oojuro/record-directory/sort-queue/v1\0".to_vec();
    a.push(ctx.purpose.byte());
    a.push(ctx.sort_of.map_or(0, Purpose::byte));
    a.push(u8::from(ctx.class == KeyClass::RetainedControl));
    a.extend(ctx.identity);
    a.extend(id);
    a.extend(sequence.to_le_bytes());
    a.extend(binding);
    a
}
struct SpoolWriter<'a> {
    ctx: Context<'a>,
    file: File,
    spool: Spool,
    binding: Vec<u8>,
    bytes: u64,
}
impl<'a> SpoolWriter<'a> {
    fn new(ctx: Context<'a>) -> Result<Self> {
        let id = random_key();
        let binding = ctx.check()?;
        ctx.authority.reserve_file(&spool_path(&ctx, id), 0)?;
        let file = file_new(&spool_path(&ctx, id))?;
        Ok(Self {
            ctx,
            file,
            spool: Spool { id, count: 0 },
            binding,
            bytes: 0,
        })
    }
    fn push(&mut self, root: &Root) -> Result<()> {
        if self.ctx.check()? != self.binding {
            return Err(Error::Conflict);
        }
        let plain = json(root, SPOOL_BYTES)?;
        let sealed = self.ctx.cipher.seal_frame(
            &spool_aad(&self.ctx, self.spool.id, self.spool.count, &self.binding),
            &plain,
        );
        self.bytes = self
            .bytes
            .checked_add(4 + sealed.len() as u64)
            .ok_or(Error::Bounds)?;
        self.ctx
            .authority
            .reserve_file(&spool_path(&self.ctx, self.spool.id), self.bytes)?;
        self.file
            .write_all(&(sealed.len() as u32).to_le_bytes())
            .and_then(|_| self.file.write_all(&sealed))
            .map_err(|_| Error::Unavailable)?;
        self.spool.count = self.spool.count.checked_add(1).ok_or(Error::Bounds)?;
        Ok(())
    }
    fn finish(self) -> Result<Spool> {
        if self.ctx.check()? != self.binding {
            return Err(Error::Conflict);
        }
        self.file.sync_all().map_err(|_| Error::Unavailable)?;
        sync_dir(self.ctx.directory)?;
        Ok(self.spool)
    }
}
struct SpoolReader<'a> {
    ctx: Context<'a>,
    file: File,
    spool: Spool,
    next: u64,
    binding: Vec<u8>,
}
impl<'a> SpoolReader<'a> {
    fn new(ctx: Context<'a>, spool: Spool) -> Result<Self> {
        let binding = ctx.check()?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(spool_path(&ctx, spool.id))
            .map_err(|_| Error::Unavailable)?;
        if !file.metadata().map_err(|_| Error::Unavailable)?.is_file() {
            return Err(Error::Invalid);
        }
        Ok(Self {
            ctx,
            file,
            spool,
            next: 0,
            binding,
        })
    }
    fn next(&mut self) -> Result<Option<Root>> {
        if self.ctx.check()? != self.binding {
            return Err(Error::Conflict);
        }
        if self.next == self.spool.count {
            let mut b = [0];
            if self.file.read(&mut b).map_err(|_| Error::Unavailable)? != 0 {
                return Err(Error::Authority);
            }
            return Ok(None);
        }
        let mut len = [0; 4];
        self.file
            .read_exact(&mut len)
            .map_err(|_| Error::Unavailable)?;
        let n = u32::from_le_bytes(len) as usize;
        if n <= SEAL_OVERHEAD || n > SPOOL_BYTES + SEAL_OVERHEAD {
            return Err(Error::Bounds);
        }
        let mut bytes = vec![0; n];
        self.file
            .read_exact(&mut bytes)
            .map_err(|_| Error::Unavailable)?;
        let plain = self
            .ctx
            .cipher
            .open_frame(
                &spool_aad(&self.ctx, self.spool.id, self.next, &self.binding),
                &bytes,
            )
            .map_err(|_| Error::Authority)?;
        let root: Root = serde_json::from_slice(&plain).map_err(|_| Error::Authority)?;
        root.check(&self.ctx)?;
        self.next += 1;
        Ok(Some(root))
    }
}
struct Head {
    entry: Entry,
    run: usize,
}
impl PartialEq for Head {
    fn eq(&self, other: &Self) -> bool {
        self.entry.key == other.entry.key && self.run == other.run
    }
}
impl Eq for Head {}
impl PartialOrd for Head {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Head {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .entry
            .key
            .cmp(&self.entry.key)
            .then_with(|| other.run.cmp(&self.run))
    }
}
pub(crate) struct Sorter<'a> {
    ctx: Context<'a>,
    sort: Context<'a>,
    spool: SpoolWriter<'a>,
    rows: Vec<Entry>,
    bytes: usize,
    #[cfg(test)]
    budget: usize,
}
impl<'a> Sorter<'a> {
    pub fn new(ctx: Context<'a>) -> Result<Self> {
        let sort = ctx.for_sort()?;
        Ok(Self {
            spool: SpoolWriter::new(sort.clone())?,
            ctx,
            sort,
            rows: vec![],
            bytes: 0,
            #[cfg(test)]
            budget: RUN_BYTES,
        })
    }
    #[cfg(test)]
    pub(crate) fn budget_for_test(&mut self, bytes: usize) {
        self.budget = bytes;
    }
    pub fn push(&mut self, entry: Entry) -> Result<()> {
        validate_entry(&entry, self.ctx.key_bound())?;
        let cap = {
            #[cfg(test)]
            {
                self.budget
            }
            #[cfg(not(test))]
            {
                RUN_BYTES
            }
        };
        if self
            .bytes
            .checked_add(entry.accounted())
            .is_none_or(|n| n > cap)
            && !self.rows.is_empty()
        {
            self.flush()?;
        }
        self.bytes = self
            .bytes
            .checked_add(entry.accounted())
            .ok_or(Error::Bounds)?;
        if self.bytes > RUN_BYTES {
            return Err(Error::Bounds);
        }
        self.rows.push(entry);
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        if self.rows.is_empty() {
            return Ok(());
        }
        self.rows.sort_by(|a, b| a.key.cmp(&b.key));
        let mut builder = Builder::new(self.sort.clone())?;
        for row in std::mem::take(&mut self.rows) {
            builder.push(row)?;
        }
        let root = builder.finish()?;
        self.spool.push(&root)?;
        self.bytes = 0;
        Ok(())
    }
    pub fn finish(mut self) -> Result<Root> {
        self.flush()?;
        let mut current = self.spool.finish()?;
        while current.count > FAN_IN as u64 {
            let mut source = SpoolReader::new(self.sort.clone(), current)?;
            let mut target = SpoolWriter::new(self.sort.clone())?;
            loop {
                let group = take_group(&mut source)?;
                if group.is_empty() {
                    break;
                }
                let root = merge(self.sort.clone(), self.sort.clone(), &group)?;
                target.push(&root)?;
                remove_runs(&self.sort, &group)?;
            }
            let old = current;
            current = target.finish()?;
            self.sort
                .authority
                .remove_file(&spool_path(&self.sort, old.id))?;
        }
        let mut source = SpoolReader::new(self.sort.clone(), current)?;
        let roots = take_group(&mut source)?;
        if source.next()?.is_some() {
            return Err(Error::Bounds);
        }
        let root = merge(self.sort.clone(), self.ctx, &roots)?;
        remove_runs(&self.sort, &roots)?;
        self.sort
            .authority
            .remove_file(&spool_path(&self.sort, current.id))?;
        Ok(root)
    }
}
fn take_group(source: &mut SpoolReader<'_>) -> Result<Vec<Root>> {
    let mut roots = vec![];
    for _ in 0..FAN_IN {
        if let Some(root) = source.next()? {
            roots.push(root);
        } else {
            break;
        }
    }
    Ok(roots)
}
fn merge<'a>(source: Context<'a>, target: Context<'a>, roots: &[Root]) -> Result<Root> {
    if roots.len() > FAN_IN {
        return Err(Error::Bounds);
    }
    let mut cursors = roots
        .iter()
        .map(|r| Cursor::seek(source.clone(), r, &[]))
        .collect::<Result<Vec<_>>>()?;
    let mut heap = BinaryHeap::new();
    for (run, cursor) in cursors.iter_mut().enumerate() {
        if let Some(entry) = cursor.next()? {
            heap.push(Head { entry, run });
        }
    }
    let mut builder = Builder::new(target)?;
    while let Some(head) = heap.pop() {
        builder.push(head.entry)?;
        if let Some(entry) = cursors[head.run].next()? {
            heap.push(Head {
                entry,
                run: head.run,
            });
        }
    }
    builder.finish()
}
fn remove_runs(ctx: &Context<'_>, roots: &[Root]) -> Result<()> {
    for root in roots {
        root.check(ctx)?;
        if let Some(node) = &root.node {
            let generation = root.fresh.ok_or(Error::Authority)?;
            if generation != node.generation {
                return Err(Error::Authority);
            }
            for number in 0..=node.file {
                ctx.authority
                    .remove_file(&path(ctx.directory, generation, number, false))?;
            }
        }
    }
    Ok(())
}
