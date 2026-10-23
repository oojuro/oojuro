//! Tokenization and query parsing shared by the v3 reader and the v4 engine.
//! Changing any function here changes the derived index of every format.
#[cfg(test)]
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}
use std::{borrow::Cow, collections::BTreeMap};

pub const QUERY_STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "to", "of", "and", "or", "in", "on", "for",
    "with", "what", "which", "how", "did", "does", "do", "can", "should", "i", "we", "you", "it",
    "this", "that", "as", "at", "be", "by",
];

#[cfg(test)]
pub(crate) fn retrieval_terms(text: &str) -> Vec<String> {
    let mut terms = tokens(text);
    // Preserve exact snake_case names alongside their searchable components.
    // This derived index is reconstructed from originals, with no format change.
    terms.extend(
        text.split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|s| s.contains('_') && s.chars().any(char::is_alphanumeric))
            .map(str::to_lowercase),
    );
    terms
}

/// Same components and snake_case features without allocating every occurrence.
pub(crate) fn for_each_retrieval_term<'a>(text: &'a str, mut visit: impl FnMut(&'a str)) {
    for_each_word(text, |word| {
        let mut any = false;
        for part in word.split('_').filter(|part| !part.is_empty()) {
            any = true;
            visit(part);
        }
        if any && word.contains('_') {
            visit(word);
        }
    });
}

pub(crate) fn literal_query(text: &str) -> Option<&str> {
    text.trim()
        .strip_prefix('"')?
        .strip_suffix('"')
        .filter(|s| !s.is_empty())
}

pub(crate) fn phrase_start(text: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let left = needle.chars().next().is_some_and(word);
    let right = needle.chars().next_back().is_some_and(word);
    let mut offset = 0;
    while let Some(found) = text[offset..].find(needle) {
        let at = offset + found;
        if (!left || !text[..at].chars().next_back().is_some_and(word))
            && (!right || !text[at + needle.len()..].chars().next().is_some_and(word))
        {
            return Some(at);
        }
        offset = if left {
            // A later match inside this same word cannot satisfy its left edge.
            at + text[at..]
                .char_indices()
                .find(|(_, c)| !word(*c))
                .map_or(text.len() - at, |(i, _)| i)
        } else {
            at + text[at..].chars().next()?.len_utf8()
        };
        if offset == text.len() {
            break;
        }
    }
    None
}

/// Normalized query terms with their counts: `query_words`, then
/// `normalize_word`, merged. Equal to that composition (tested against it);
/// counts lowercase words in a hash map that borrows tokens which are
/// already lowercase ASCII, and stems each distinct word through a
/// per-thread cache (queries of one agent repeat most of their words).
pub(crate) fn query_terms(text: &str) -> BTreeMap<String, usize> {
    let mut words: FastMap<Cow<'_, str>, usize> = FastMap::default();
    let mut stops: FastMap<Cow<'_, str>, usize> = FastMap::default();
    for_each_word(text, |s| {
        // Every char of a word is alphanumeric or '_'; skip '_'-only runs.
        if !s.bytes().any(|b| b != b'_') {
            return;
        }
        let w = if s.bytes().all(|b| b.is_ascii() && !b.is_ascii_uppercase()) {
            Cow::Borrowed(s)
        } else {
            Cow::Owned(s.to_lowercase())
        };
        let stop = w.len() <= 6 && QUERY_STOP_WORDS.contains(&w.as_ref());
        *(if stop { &mut stops } else { &mut words })
            .entry(w)
            .or_default() += 1;
    });
    let words = if words.is_empty() { stops } else { words };
    let mut terms = BTreeMap::new();
    STEMS.with(|cell| {
        let mut cache = cell.borrow_mut();
        if cache.len() > STEM_CACHE_ENTRIES {
            cache.clear();
        }
        for (word, count) in words {
            // `normalize_word` stems exactly the ASCII-alphabetic words.
            let term = if word.bytes().all(|c| c.is_ascii_alphabetic()) {
                match cache.get(word.as_ref()) {
                    Some(stem) => stem.to_string(),
                    None => {
                        let stem = normalize_word(&word).into_owned();
                        cache.insert(word.as_ref().into(), stem.as_str().into());
                        stem
                    }
                }
            } else {
                word.into_owned()
            };
            *terms.entry(term).or_default() += count;
        }
    });
    terms
}

/// Call `f` on every maximal run of alphanumeric or '_' chars: the
/// non-empty pieces of `text.split(|c| !c.is_alphanumeric() && c != '_')`,
/// in order, without decoding ASCII bytes as chars.
fn for_each_word<'a>(text: &'a str, mut f: impl FnMut(&'a str)) {
    let bytes = text.as_bytes();
    let mut start: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (word, len) = if b < 0x80 {
            (b.is_ascii_alphanumeric() || b == b'_', 1)
        } else {
            let c = text[i..].chars().next().expect("char boundary");
            (c.is_alphanumeric(), c.len_utf8())
        };
        if word {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            f(&text[s..i]);
        }
        i += len;
    }
    if let Some(s) = start {
        f(&text[s..]);
    }
}

/// A small multiply-rotate hasher for short word keys (the default SipHash
/// costs more than the rest of query parsing).
#[derive(Default, Clone, Copy)]
pub(crate) struct FastHasher(u64);

impl std::hash::Hasher for FastHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        let (chunks, remainder) = bytes.as_chunks::<8>();
        for c in chunks {
            let v = u64::from_le_bytes(*c);
            self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(K);
        }
        let mut tail = 0u64;
        for (i, b) in remainder.iter().enumerate() {
            tail |= u64::from(*b) << (8 * i);
        }
        self.0 = (self.0.rotate_left(5) ^ tail ^ (bytes.len() as u64) << 56).wrapping_mul(K);
    }
    #[inline]
    fn write_u8(&mut self, b: u8) {
        self.write(&[b]);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

pub(crate) type FastMap<K, V> =
    std::collections::HashMap<K, V, std::hash::BuildHasherDefault<FastHasher>>;

const STEM_CACHE_ENTRIES: usize = 1 << 16;

thread_local! {
    static STEMS: std::cell::RefCell<FastMap<Box<str>, Box<str>>> =
        std::cell::RefCell::new(FastMap::default());
}

pub(crate) fn normalize_word(word: &str) -> std::borrow::Cow<'_, str> {
    if !word.is_ascii() || !word.bytes().all(|c| c.is_ascii_alphabetic()) {
        return std::borrow::Cow::Borrowed(word);
    }
    static ENGLISH: std::sync::OnceLock<rust_stemmers::Stemmer> = std::sync::OnceLock::new();
    ENGLISH
        .get_or_init(|| rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::English))
        .stem(word)
}
/// A prefix-code prior: Elias-gamma byte length followed by literal UTF-8 bytes.
/// This is a subprobability over byte strings with the remaining mass as escape.
pub(crate) fn literal_probability(term: &str) -> f64 {
    let n = term.len() + 1;
    let gamma = 2 * (usize::BITS - n.leading_zeros() - 1) + 1;
    (-((8 * term.len() + gamma as usize) as f64)).exp2()
}
