use super::*;

fn source(id: &str, text: &str, start: usize, at: i64) -> EvidenceSnippet {
    EvidenceSnippet {
        id: id.into(),
        key: format!("key/{id}"),
        text: text.into(),
        valid_at: at,
        recorded_at: at + 1,
        verified: at % 2 == 0,
        source_range: Some([start, start + text.len()]),
    }
}
fn equal(a: &[EvidenceSnippet], b: &[EvidenceSnippet]) {
    assert_eq!(
        serde_json::to_vec(a).unwrap(),
        serde_json::to_vec(b).unwrap()
    );
}

#[test]
fn interleaved_origins_roundtrip_with_exact_unicode_ranges_and_all_metadata() -> Result<()> {
    let a = "λ🦀 a quoted \"value\"\n".repeat(60);
    let b = "different original text\n".repeat(60);
    let inputs = vec![
        source("first", &a, 7, 1),
        source("middle", &b, 0, 3),
        source("later", &a, 120, 50),
        source("last", &b, 901, 2),
    ];
    let mut packet = ContextPacket::new(2, 16_384)?;
    for input in &inputs {
        assert!(packet.push(input.clone())?);
    }
    assert_eq!((packet.block_count(), packet.origin_count()), (2, 4));
    let encoded = packet.to_json()?;
    assert!(encoded.len() < serde_json::to_vec(&inputs)?.len());
    assert_eq!(packet.bytes(), encoded.len());
    equal(&packet.expand()?, &inputs);
    equal(&decode_context(&encoded)?, &inputs);
    let before = packet.to_json()?;
    assert!(!packet.push(source("unequal", &format!("{a}!"), 0, 1))?);
    assert_eq!(before, packet.to_json()?);
    Ok(())
}

#[test]
fn short_equal_text_keeps_the_cheaper_legacy_flat_shape() -> Result<()> {
    let inputs = vec![source("a", "x", 0, 1), source("b", "x", 30, 3)];
    let mut packet = ContextPacket::new(1, 4096)?;
    for s in &inputs {
        assert!(packet.push(s.clone())?);
    }
    assert_eq!(packet.block_count(), 1);
    assert_eq!(packet.origin_count(), 2);
    assert_eq!(packet.to_json()?, serde_json::to_vec(&inputs)?);
    equal(&decode_context(&serde_json::to_vec(&inputs)?)?, &inputs);
    Ok(())
}

#[test]
fn every_measured_addition_and_exact_budget_boundary_preserves_source_order() -> Result<()> {
    let inputs: Vec<_> = (0..27)
        .map(|i| {
            source(
                &format!("row-{i}"),
                &format!("{} {}", i % 3, "☃\n\"escaped\"\\text ".repeat(40)),
                i * 1_000_000,
                i as i64,
            )
        })
        .collect();
    let mut full = ContextPacket::new(3, 32_768)?;
    for s in &inputs {
        assert!(full.push(s.clone())?);
        assert_eq!(full.bytes(), full.to_json()?.len());
    }
    let limit = full.bytes();
    let mut exact = ContextPacket::new(3, limit)?;
    for s in &inputs {
        assert!(exact.push(s.clone())?);
    }
    assert_eq!(exact.remaining(), 0);
    let before = exact.to_json()?;
    assert!(!exact.push(source("extra", "new", 0, 1))?);
    assert_eq!(before, exact.to_json()?);
    let mut tight = ContextPacket::new(3, limit - 1)?;
    let mut accepted = Vec::new();
    for s in &inputs {
        if tight.push(s.clone())? {
            accepted.push(s.clone());
        }
        assert!(tight.to_json()?.len() < limit);
    }
    assert!(accepted.len() < inputs.len());
    equal(&decode_context(&tight.to_json()?)?, &accepted);
    Ok(())
}

#[test]
fn malformed_aliases_and_range_lengths_are_rejected() -> Result<()> {
    let text = "original ".repeat(100);
    let mut packet = ContextPacket::new(1, 8192)?;
    for i in 0..3 {
        assert!(packet.push(source(&format!("r{i}"), &text, i * 1000, 1))?);
    }
    let valid: serde_json::Value = serde_json::from_slice(&packet.to_json()?)?;
    for mode in 0..4 {
        let mut bad = valid.clone();
        let aliases = &mut bad[0]["also_from"];
        match mode {
            0 => aliases[0]["at"] = serde_json::json!(10_000),
            1 => aliases[1]["at"] = aliases[0]["at"].clone(),
            2 => aliases[0]["text"] = serde_json::json!("unexpected text"),
            _ => aliases[0]["source_range"] = serde_json::json!([0, 1]),
        }
        assert!(decode_context(&serde_json::to_vec(&bad)?).is_err());
    }
    let mut invalid = source("bad", &text, 0, 1);
    invalid.source_range = Some([9, 8]);
    let before = packet.to_json()?;
    assert!(packet.push(invalid).is_err());
    assert_eq!(before, packet.to_json()?);
    Ok(())
}

#[test]
fn empty_packets_and_invalid_limits_are_explicit() -> Result<()> {
    let mut empty = ContextPacket::new(1, 2)?;
    assert_eq!(empty.to_json()?, b"[]");
    assert!(!empty.push(source("x", "text", 0, 1))?);
    assert!(decode_context(b"[]")?.is_empty());
    assert!(ContextPacket::new(0, 16).is_err());
    assert!(ContextPacket::new(1, 1).is_err());
    assert!(ContextPacket::new(1, MAX_FRAME + 1).is_err());
    Ok(())
}

#[test]
fn expanded_size_is_bounded_before_shared_text_is_cloned_into_all_origins() -> Result<()> {
    let text = "z".repeat(MAX_FRAME / 2 + 1);
    let first = source("first", &text, 0, 1);
    let second = source("second", &text, 0, 2);
    let mut packet = ContextPacket::new(1, MAX_FRAME)?;
    assert!(packet.push(first.clone())?);
    assert!(!packet.push(second.clone())?);
    assert!(packet.expanded_bytes() <= MAX_FRAME);
    let mut alias = second;
    alias.text.clear();
    let encoded = serde_json::to_vec(&vec![Group {
        source: first,
        also_from: vec![Alias {
            at: 1,
            source: alias,
        }],
    }])?;
    assert!(encoded.len() < MAX_FRAME);
    assert!(decode_context(&encoded).is_err());
    Ok(())
}
