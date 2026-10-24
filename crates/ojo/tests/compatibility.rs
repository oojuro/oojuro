use ojo::{Database, Error};
use serde_json::{Value, json};
fn unhex(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap())
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn independent_frozen_wire_vectors() {
    let vectors: Value =
        serde_json::from_str(include_str!("../../../spec/0.1/vectors.json")).unwrap();
    let root = std::env::temp_dir().join(format!(
        "ojo-vectors-{:032x}",
        u128::from_le_bytes(ojo::request_id().unwrap())
    ));
    std::fs::create_dir(&root).unwrap();
    for vector in vectors["valid"].as_array().unwrap() {
        let path = root.join(vector["name"].as_str().unwrap());
        let bytes = unhex(vector["file_hex"].as_str().unwrap());
        std::fs::write(&path, bytes).unwrap();
        let key: Option<[u8; 32]> = vector["key_hex"]
            .as_str()
            .map(|s| unhex(s).try_into().unwrap());
        let opened = Database::open(&path, key.as_ref(), false);
        if vector["name"].as_str().unwrap().contains("zstd") && !cfg!(feature = "archive") {
            assert!(matches!(opened, Err(Error::Unsupported)));
            continue;
        }
        let mut db = opened.unwrap();
        let mut operations = Vec::new();
        db.scan(|sequence, operation, key, value| {
            operations.push(json!({
            "sequence":sequence,"operation":operation,"key_hex":hex(key),"value_hex":value.map(hex)
        }))
        })
        .unwrap();
        assert_eq!(
            Value::Array(operations),
            vector["operations"],
            "{}",
            vector["name"]
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn complete_but_semantically_invalid_transactions_are_never_truncated() {
    use sha2::{Digest, Sha256};
    let root = std::env::temp_dir().join(format!(
        "ojo-invalid-{:032x}",
        u128::from_le_bytes(ojo::request_id().unwrap())
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("file");
    let mut db = Database::create(&path, None).unwrap();
    db.commit(
        [1; 16],
        &[ojo::Change {
            key: b"a",
            value: Some(b"b"),
        }],
    )
    .unwrap();
    drop(db);
    let original = std::fs::read(&path).unwrap();
    for offset in [
        64 + 32,
        64 + 37,
        64 + 72,
        64 + 96,
        64 + 96 + 1,
        64 + 96 + 4,
        64 + 96 + 8,
    ] {
        let mut invalid = original.clone();
        if offset == 64 + 72 {
            invalid[136..152].fill(0);
        } else {
            invalid[offset] ^= 0x80;
        }
        let checksum = crc32fast::hash(&invalid[64..156]).to_le_bytes();
        invalid[156..160].copy_from_slice(&checksum);
        let footer = invalid.len() - 48;
        let mut hash = Sha256::new();
        hash.update(&invalid[..footer]);
        invalid[footer + 16..].copy_from_slice(&hash.finalize());
        std::fs::write(&path, &invalid).unwrap();
        assert!(
            Database::open(&path, None, true).is_err(),
            "offset {offset}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
    }
    std::fs::remove_dir_all(root).unwrap();
}
