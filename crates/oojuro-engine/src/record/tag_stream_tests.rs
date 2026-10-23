use super::*;

#[test]
fn streamed_tags_preserve_exact_keyed_json_and_escaped_bounds() {
    let values = [
        serde_json::json!(null),
        serde_json::json!({"a": ["quote\"slash\\line\n", "λ🦀\u{0000}", "", "é"], "b": [0, -1, 1.25, true]}),
        serde_json::json!({"text": "λ\"\\\n\0".repeat(4096)}),
    ];
    for value in values {
        for domain in ["request", "typed-input-v1"] {
            let value = (domain, &value);
            let expected = serde_json::to_vec(&value).unwrap();
            for key in [[0; 32], [41; 32]] {
                for limit in [0, 1, expected.len() - 1, expected.len(), expected.len() + 1] {
                    let result = bounded_json(&value, limit, blake3::Hasher::new_keyed(&key));
                    if limit < expected.len() {
                        assert!(matches!(result, Err(Error::Bounds)));
                    } else {
                        let actual = result.unwrap();
                        assert_eq!(actual.size, expected.len());
                        assert_eq!(
                            actual.writer.finalize(),
                            blake3::keyed_hash(&key, &expected)
                        );
                    }
                }
            }
        }
    }
}
