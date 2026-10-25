//! Bounded sorted COW edits. Existing single-edit source authorities retain their
//! original path; disposable projections can write a touched node once per batch.
use super::*;

pub(crate) const MAX_BATCH_ENTRIES: usize = 64;
pub(crate) const MAX_BATCH_BYTES: usize = 1 << 20;

impl Editor<'_> {
    pub fn insert_batch(&mut self, entries: Vec<Entry>) -> Result<()> {
        if entries.is_empty() || entries.len() > MAX_BATCH_ENTRIES {
            return Err(Error::Bounds);
        }
        let mut bytes = 0usize;
        for entry in &entries {
            validate_entry(entry, self.pack.ctx.key_bound())?;
            bytes = bytes
                .checked_add(entry.key.len() + entry.value.len() + 8)
                .ok_or(Error::Bounds)?;
            if bytes > MAX_BATCH_BYTES {
                return Err(Error::Bounds);
            }
        }
        if entries.windows(2).any(|pair| pair[0].key >= pair[1].key) {
            return Err(Error::Conflict);
        }
        let mut references = if let Some(root) = &self.root.node {
            insert_many(&mut self.pack, root, entries)?
        } else {
            store_nodes(&mut self.pack, Node::Leaf(entries), 0)?
        };
        while references.len() > 1 {
            let level = references[0].level.checked_add(1).ok_or(Error::Bounds)?;
            references = store_nodes(&mut self.pack, Node::Branch(references), level)?;
        }
        let reference = references.pop().ok_or(Error::Authority)?;
        if self.pack.ctx.check()? != self.pack.binding {
            return Err(Error::Conflict);
        }
        self.root = Root {
            format: 1,
            purpose: self.pack.ctx.purpose,
            binding: self.pack.binding.clone(),
            count: reference.count,
            node: Some(reference),
            fresh: None,
        };
        Ok(())
    }
}

fn insert_many(
    pack: &mut Pack<'_>,
    reference: &Reference,
    entries: Vec<Entry>,
) -> Result<Vec<Reference>> {
    let node = match pack.load(reference)? {
        Node::Leaf(mut old) => {
            old.extend(entries);
            old.sort_unstable_by(|a, b| a.key.cmp(&b.key));
            if old.windows(2).any(|pair| pair[0].key == pair[1].key) {
                return Err(Error::Conflict);
            }
            Node::Leaf(old)
        }
        Node::Branch(children) => {
            let mut pending = entries.into_iter().peekable();
            let mut merged = Vec::new();
            for (index, child) in children.iter().enumerate() {
                let mut edits = Vec::new();
                while pending
                    .peek()
                    .is_some_and(|entry| index + 1 == children.len() || entry.key <= child.last)
                {
                    edits.push(pending.next().unwrap());
                }
                if edits.is_empty() {
                    merged.push(child.clone());
                } else {
                    merged.extend(insert_many(pack, child, edits)?);
                }
            }
            if pending.next().is_some() {
                return Err(Error::Authority);
            }
            Node::Branch(merged)
        }
    };
    store_nodes(pack, node, reference.level)
}

fn store_nodes(pack: &mut Pack<'_>, mut node: Node, level: u8) -> Result<Vec<Reference>> {
    if level > MAX_DEPTH {
        return Err(Error::Bounds);
    }
    match encode_node_bound(&node, level, pack.ctx.key_bound()) {
        Ok(_) => return Ok(vec![pack.put(&node, level)?]),
        Err(Error::Bounds) if node_len(&node) > 1 => {}
        Err(error) => return Err(error),
    }
    let second = match &mut node {
        Node::Leaf(rows) => Node::Leaf(rows.split_off(rows.len() / 2)),
        Node::Branch(rows) => Node::Branch(rows.split_off(rows.len() / 2)),
    };
    let mut first = store_nodes(pack, node, level)?;
    first.extend(store_nodes(pack, second, level)?);
    Ok(first)
}
