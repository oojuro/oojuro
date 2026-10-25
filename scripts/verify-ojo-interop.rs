//! Compile against each frozen library, then read the other implementation's files.
//! See docs/benchmarks/OJO_IO_OPTIMIZATION.md. Only synthetic io_profile data.
use ojo::{Change, Database};

fn value(index: usize, width: usize) -> Vec<u8> {
    let mut bytes = b"synthetic trace: source bytes with exact line endings\r\n"
        .iter()
        .copied()
        .cycle()
        .take(width)
        .collect::<Vec<_>>();
    bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
    bytes
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(args.len(), 5, "FILE COUNT BATCH WIDTH ENCRYPTED_BOOL");
    let count: usize = args[1].parse()?;
    let batch: usize = args[2].parse()?;
    let width: usize = args[3].parse()?;
    let encrypted: bool = args[4].parse()?;
    assert!((1..=100_000).contains(&count));
    assert!((1..=1024).contains(&batch));
    assert!((8..=16384).contains(&width));
    let key = encrypted.then_some([42; 32]);
    let mut db = Database::open(&args[0], key.as_ref(), true)?;
    assert_eq!(db.recovered_tail_bytes(), 0);
    let checkpoint = db.checkpoint();
    let mut seen = 0;
    db.scan(|sequence, operation, key, bytes| {
        assert_eq!(sequence, (seen / batch + 1) as u64);
        assert_eq!(operation, (seen % batch) as u32);
        assert_eq!(key, (seen as u64).to_be_bytes());
        assert_eq!(bytes.unwrap(), value(seen, width));
        seen += 1;
    })?;
    assert_eq!(seen, count);
    for n in 0..32 {
        let index = (n * 7919 + 31) % count;
        assert_eq!(
            db.get(&(index as u64).to_be_bytes())?.unwrap(),
            value(index, width)
        );
    }
    let keys = (0..batch.min(count))
        .map(|i| (i as u64).to_be_bytes())
        .collect::<Vec<_>>();
    let values = (0..keys.len()).map(|i| value(i, width)).collect::<Vec<_>>();
    let changes = keys
        .iter()
        .zip(&values)
        .map(|(key, value)| Change {
            key,
            value: Some(value),
        })
        .collect::<Vec<_>>();
    let receipt = db.commit(1u128.to_le_bytes(), &changes)?;
    assert_eq!(receipt.checkpoint.sequence, 1);
    assert_eq!(receipt.operations, keys.len() as u32);
    assert_eq!(db.commit(1u128.to_le_bytes(), &changes)?, receipt);
    assert_eq!(db.checkpoint(), checkpoint);
    println!("verified {seen} records, sampled gets and persistent retry");
    Ok(())
}
