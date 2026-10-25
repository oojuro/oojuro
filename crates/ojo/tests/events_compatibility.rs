use ojo::events::Journal;
use serde_json::{Value, json};

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn independently_encoded_event_profile_vectors() {
    let vectors: Value =
        serde_json::from_str(include_str!("../../../spec/events/1/vectors.json")).unwrap();
    let root = std::env::temp_dir().join(format!(
        "ojo-events-compat-{:032x}",
        u128::from_le_bytes(ojo::request_id().unwrap())
    ));
    std::fs::create_dir(&root).unwrap();
    for vector in vectors["valid"].as_array().unwrap() {
        let path = root.join(vector["name"].as_str().unwrap());
        std::fs::write(&path, unhex(vector["file_hex"].as_str().unwrap())).unwrap();
        let mut journal = Journal::open(&path, None, false).unwrap();
        let mut actual = Vec::new();
        journal.scan(None, 100, |event| {
            actual.push(json!({"cursor_hex":hex(&event.cursor.to_bytes()),"accepted_at":event.accepted_at,"data_hex":hex(&event.data)}));
            Ok(())
        }).unwrap();
        assert_eq!(Value::Array(actual), vector["events"]);
    }
    std::fs::remove_dir_all(root).unwrap();
}
