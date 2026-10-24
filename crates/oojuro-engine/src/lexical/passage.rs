//! Query-addressed exact UTF-8 ranges; no generated text or summaries.
use super::text::{QUERY_STOP_WORDS, literal_query, normalize_word, phrase_start, query_terms};
use crate::record::{Error, Result};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize)]
pub struct TextSlice<'a> {
    pub start: usize,
    pub end: usize,
    pub text: &'a str,
}

/// Select a bounded original range by query-feature coverage and local surprise.
/// No overlap yields a prefix, which carries no claim of relevant evidence.
pub(crate) fn extract_passage<'a>(
    text: &'a str,
    query: &str,
    max_bytes: usize,
) -> Result<Option<TextSlice<'a>>> {
    extract(text, query, max_bytes, false)
}

fn extract<'a>(
    text: &'a str,
    query: &str,
    max_bytes: usize,
    normalized: bool,
) -> Result<Option<TextSlice<'a>>> {
    if query.len() > 65_536 {
        return Err(Error::Bounds);
    }
    if text.is_empty() || max_bytes == 0 {
        return Ok(None);
    }
    if let Some(needle) = literal_query(query) {
        if needle.len() > max_bytes {
            return Ok(None);
        }
        return Ok(phrase_start(text, needle)
            .map(|start| expand(text, start, start + needle.len(), max_bytes)));
    }
    let mut terms = if normalized {
        query_terms(query)
    } else {
        query_words(query)
    };
    for (term, n) in terms.clone() {
        if term.contains('_') {
            let parts = if normalized {
                query_terms(&term.replace('_', " "))
            } else {
                query_words(&term.replace('_', " "))
            };
            for (part, count) in parts {
                *terms.entry(part).or_default() += n * count;
            }
        }
    }
    let terms: Vec<_> = terms.into_iter().collect();
    let lookup: std::collections::HashMap<_, _> = terms
        .iter()
        .enumerate()
        .map(|(i, (t, _))| (t.as_str(), i))
        .collect();
    let mut frequencies = vec![0usize; terms.len()];
    let mut total = 0usize;
    let mut hits = Vec::<(usize, usize, usize)>::new();
    let mut stems = std::collections::HashMap::<&'a str, String>::new();
    for (start, end, word) in words(text) {
        let mut visit = |raw: &'a str| {
            total += 1;
            let term = if normalized
                && raw.is_ascii()
                && raw.bytes().all(|c| c.is_ascii_alphabetic())
            {
                if stems.len() < 4096 {
                    std::borrow::Cow::Borrowed(
                        stems
                            .entry(raw)
                            .or_insert_with(|| {
                                normalize_word(&raw.to_ascii_lowercase()).into_owned()
                            })
                            .as_str(),
                    )
                } else if let Some(stem) = stems.get(raw) {
                    std::borrow::Cow::Borrowed(stem.as_str())
                } else {
                    std::borrow::Cow::Owned(normalize_word(&raw.to_ascii_lowercase()).into_owned())
                }
            } else if raw.is_ascii() && !raw.as_bytes().iter().any(u8::is_ascii_uppercase) {
                std::borrow::Cow::Borrowed(raw)
            } else {
                std::borrow::Cow::Owned(raw.to_lowercase())
            };
            if let Some(&i) = lookup.get(term.as_ref()) {
                frequencies[i] += 1;
                if end - start <= max_bytes {
                    hits.push((start, end, i));
                }
            }
        };
        let mut parts = 0;
        for part in word.split('_').filter(|s| !s.is_empty()) {
            parts += 1;
            visit(part);
        }
        if parts > 0 && word.contains('_') {
            visit(word);
        }
    }
    let weights: Vec<_> = frequencies
        .iter()
        .zip(&terms)
        .map(|(f, (_, n))| *n as f64 * ((total + terms.len() + 1) as f64 / (*f + 1) as f64).log2())
        .collect();
    let mut counts = vec![0usize; terms.len()];
    let (mut left, mut score, mut best) = (0usize, 0f64, (0f64, 0usize, 0usize));
    for right in 0..hits.len() {
        let (_, end, i) = hits[right];
        if counts[i] == 0 {
            score += weights[i];
        }
        counts[i] += 1;
        while end - hits[left].0 > max_bytes {
            let i = hits[left].2;
            counts[i] -= 1;
            if counts[i] == 0 {
                score -= weights[i];
            }
            left += 1;
        }
        let start = hits[left].0;
        if score > best.0 + 1e-10
            || ((score - best.0).abs() <= 1e-10 && end - start < best.2 - best.1)
        {
            best = (score, start, end);
        }
    }
    if best.0 > 0. {
        return Ok(Some(expand(text, best.1, best.2, max_bytes)));
    }
    if !normalized && !terms.is_empty() {
        return extract(text, query, max_bytes, true);
    }
    let mut end = max_bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok((end > 0).then(|| TextSlice {
        start: 0,
        end,
        text: &text[..end],
    }))
}

fn expand(text: &str, start: usize, end: usize, max_bytes: usize) -> TextSlice<'_> {
    // Expand around the strongest matching span while preserving the cap.
    let padding = max_bytes.saturating_sub(end - start);
    let mut start = start.saturating_sub(padding / 2);
    let mut end = start.saturating_add(max_bytes).min(text.len());
    start = start.saturating_sub(max_bytes.saturating_sub(end - start));
    while !text.is_char_boundary(start) {
        start += 1;
    }
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    TextSlice {
        start,
        end,
        text: &text[start..end],
    }
}

fn words(text: &str) -> impl Iterator<Item = (usize, usize, &str)> {
    let mut chars = text.char_indices().peekable();
    std::iter::from_fn(move || {
        let start = loop {
            let (at, c) = chars.next()?;
            if c.is_alphanumeric() || c == '_' {
                break at;
            }
        };
        let mut end = text.len();
        while let Some(&(at, c)) = chars.peek() {
            if c.is_alphanumeric() || c == '_' {
                chars.next();
            } else {
                end = at;
                break;
            }
        }
        Some((start, end, &text[start..end]))
    })
}

pub(crate) fn query_words(text: &str) -> BTreeMap<String, usize> {
    let all: Vec<_> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.chars().any(char::is_alphanumeric))
        .map(str::to_lowercase)
        .collect();
    let mut filtered = BTreeMap::new();
    for t in &all {
        if !QUERY_STOP_WORDS.contains(&t.as_str()) {
            *filtered.entry(t.clone()).or_default() += 1;
        }
    }
    if filtered.is_empty() {
        for t in all {
            *filtered.entry(t).or_default() += 1;
        }
    }
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selects_distant_exact_evidence_with_unicode_and_bounded_ranges() -> Result<()> {
        let text = format!(
            "{}\nThe garnet job uses resolved_epoch=91; verification passed.\n{}",
            "λ🦀 noise\n".repeat(10000),
            "irrelevant tail ".repeat(1000)
        );
        let part = extract_passage(&text, "garnet resolved_epoch", 256)?.unwrap();
        assert!(part.text.contains("resolved_epoch=91"));
        assert_eq!(part.text, &text[part.start..part.end]);
        assert!(part.text.len() <= 256);
        assert!(extract_passage(&text, "garnet", 0)?.is_none());
        assert!(extract_passage("🦀", "absent", 3)?.is_none());
        let prefix = extract_passage(&text, "absent", 128)?.unwrap();
        assert_eq!(prefix.start, 0);
        assert_eq!(prefix.text, &text[..prefix.end]);
        Ok(())
    }
    #[test]
    fn composite_components_and_short_inputs_are_exact() -> Result<()> {
        let text = "head repair_shadow_page() done café";
        for q in [
            "shadow",
            "repair_shadow_page",
            "shadow repair_shadow_page",
            "",
            "unknown_name",
        ] {
            for bytes in 0..80 {
                if let Some(part) = extract_passage(text, q, bytes)? {
                    assert!(part.text.len() <= bytes);
                    assert_eq!(part.text, &text[part.start..part.end]);
                }
            }
        }
        Ok(())
    }
    #[test]
    fn word_cache_overflow_keeps_normalized_hits_and_original_offsets() -> Result<()> {
        let mut text = (0..4200usize)
            .map(|i| {
                format!(
                    "noise{}{}{} ",
                    (b'a' + (i / 676 % 26) as u8) as char,
                    (b'a' + (i / 26 % 26) as u8) as char,
                    (b'a' + (i % 26) as u8) as char
                )
            })
            .collect::<String>();
        text.push_str("The workers were running efficiently. café.");
        let part = extract_passage(&text, "worker runs", 128)?.unwrap();
        assert!(part.text.contains("workers were running"));
        assert_eq!(part.text, &text[part.start..part.end]);
        assert!(part.text.len() <= 128);
        let irregular = extract_passage("A worker was dying.", "die", 32)?.unwrap();
        assert!(irregular.text.contains("dying"));
        Ok(())
    }
    #[test]
    fn literal_evidence_keeps_priority_over_a_shorter_variant_only_span() -> Result<()> {
        let text = format!(
            "worker {} runs; verified artifact=17.\n{}\nworkers running; unrelated artifact=4.",
            "padding ".repeat(12),
            "unrelated context ".repeat(200)
        );
        let part = extract_passage(&text, "worker runs", 192)?.unwrap();
        assert!(part.text.contains("verified artifact=17"));
        assert!(!part.text.contains("unrelated artifact=4"));
        assert_eq!(part.text, &text[part.start..part.end]);
        Ok(())
    }
}
