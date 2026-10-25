//! Account-free non-agent example. Creates a NEW raw plaintext Event journal.
use ojo::events::Journal;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: sensor_journal NEW_FILE.juro")?;
    let mut journal = Journal::create(path, None)?;
    for data in [
        b"sensor=alpha temperature=21.5\n".as_slice(),
        b"\0\xffbinary sample\r\n",
        b"",
    ] {
        let receipt = journal.append(data)?;
        let event = journal.get(receipt.cursor)?.ok_or("missing original")?;
        if event.data != data {
            return Err("original changed".into());
        }
        println!(
            "{}",
            serde_json::json!({"cursor_hex":hex(&receipt.cursor.to_bytes()),
            "accepted_at":receipt.accepted_at,"data_hex":hex(&event.data)})
        );
    }
    Ok(())
}
