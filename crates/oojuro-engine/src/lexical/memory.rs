//! Retained allocation estimate for cache admission, not an allocator or RSS limit.
use super::*;
use std::mem::size_of;

impl Index {
    #[cfg(test)]
    pub(crate) fn original_text_allocation(&self) -> (usize, usize) {
        self.documents.iter().fold((0, 0), |(len, capacity), d| {
            (len + d.event.text.len(), capacity + d.event.text.capacity())
        })
    }
    pub(crate) fn estimated_heap_bytes(&self) -> usize {
        // std hides tree/table layout. Charge each tree entry for structural
        // overhead and each hash slot for alignment/control bytes separately.
        let mut bytes = self
            .writer
            .capacity()
            .saturating_add(
                self.documents
                    .capacity()
                    .saturating_mul(size_of::<Document>()),
            )
            .saturating_add(
                self.seen
                    .capacity()
                    .saturating_mul(size_of::<([u8; 16], usize)>() + 32),
            );
        for document in &self.documents {
            bytes = bytes
                .saturating_add(document.event.tool.capacity())
                .saturating_add(document.event.args.capacity())
                .saturating_add(document.event.text.capacity())
                .saturating_add(document.normalized_args.capacity());
        }
        if !self.postings.is_empty() {
            bytes = bytes.saturating_add(1024);
        }
        for (term, postings) in &self.postings {
            bytes = bytes
                .saturating_add(256)
                .saturating_add(term.capacity())
                .saturating_add(postings.capacity().saturating_mul(size_of::<(u32, u32)>()));
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn charges_posting_document_and_text_spare_capacity() {
        let mut index =
            Index::from_record_rows("writer", 1, [(1, 1, "quartzneedle", true)].into_iter())
                .unwrap();
        let before = index.estimated_heap_bytes();
        let document_capacity = index.documents.capacity();
        index.documents.reserve(100);
        let document_growth =
            (index.documents.capacity() - document_capacity) * size_of::<Document>();
        let (_, postings) = index.postings.iter_mut().next().unwrap();
        let posting_capacity = postings.capacity();
        postings.reserve(100);
        let posting_growth = (postings.capacity() - posting_capacity) * size_of::<(u32, u32)>();
        let text = &mut index.documents[0].event.text;
        let text_capacity = text.capacity();
        text.reserve(100);
        let text_growth = text.capacity() - text_capacity;
        assert_eq!(
            index.estimated_heap_bytes() - before,
            document_growth + posting_growth + text_growth
        );
        assert!(index.estimated_heap_bytes() > index.documents[0].event.text.len());
    }
}
