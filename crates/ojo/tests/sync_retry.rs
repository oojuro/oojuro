//! Opt-in real syscall-failure regression. Normal Rust tests need no C compiler.
#![cfg(target_os = "linux")]
use ojo::{
    Error,
    events::{Journal, Receipt},
};
use std::{fs, io::Write, path::PathBuf};

#[test]
#[ignore = "run scripts/verify-ojo-sync-retry.py for the Linux fsync interposer"]
fn complete_but_unsynced_retry_never_receives_a_durable_ack() {
    let target =
        PathBuf::from(std::env::var_os("OJO_SYNC_FAULT_TARGET").expect("use the fault runner"));
    let flag = PathBuf::from(std::env::var_os("OJO_SYNC_FAULT_FLAG").unwrap());
    let counter = PathBuf::from(std::env::var_os("OJO_SYNC_FAULT_COUNTER").unwrap());
    let key = std::env::var_os("OJO_SYNC_FAULT_ENCRYPTED").map(|_| [42; 32]);
    let calls = || match fs::read(&counter) {
        Ok(bytes) => bytes.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("cannot read sync observations: {error}"),
    };
    let mut journal = Journal::create(&target, key.as_ref()).unwrap();
    let initial = calls();
    assert_eq!(journal.confirm().unwrap(), journal.checkpoint());
    assert_eq!(calls(), initial);
    let data = b"exact original after an ambiguous sync failure\0\xff";
    let intent = journal.prepare(data).unwrap();
    fs::write(&flag, b"fail only this database's fsync").unwrap();
    assert!(matches!(
        journal.append_prepared(&intent, data),
        Err(Error::Io(_))
    ));
    assert!(matches!(
        journal.append_prepared(&intent, data),
        Err(Error::Poisoned)
    ));
    assert_eq!(calls(), initial + 1);
    drop(journal);

    // A complete, valid frame can remain visible even though its sync failed.
    let bytes = fs::read(&target).unwrap();
    let mut reader = Journal::open(&target, key.as_ref(), false).unwrap();
    assert!(matches!(reader.confirm(), Err(Error::ReadOnly)));
    assert_eq!(reader.len(), 1);
    assert_eq!(reader.recovered_tail_bytes(), 0);
    let mut expected = None;
    reader
        .scan(None, 1, |event| {
            assert_eq!(event.data, data);
            expected = Some(Receipt {
                cursor: event.cursor,
                accepted_at: event.accepted_at,
            });
            Ok(())
        })
        .unwrap();
    drop(reader);
    assert_eq!(calls(), initial + 1);

    let mut retry = Journal::open(&target, key.as_ref(), true).unwrap();
    assert!(
        matches!(retry.append_prepared(&intent, data), Err(Error::Io(_))),
        "an unconfirmed replay must sync before acknowledging a duplicate"
    );
    assert!(matches!(
        retry.append(b"must stay poisoned"),
        Err(Error::Poisoned)
    ));
    assert_eq!(calls(), initial + 2);
    assert_eq!(fs::read(&target).unwrap(), bytes);
    drop(retry);

    // Applications can reconstruct accepted batch identities without repeating
    // the engine commit call. Their explicit durability barrier must also fail.
    let mut replay = Journal::open(&target, key.as_ref(), true).unwrap();
    assert!(matches!(replay.confirm(), Err(Error::Io(_))));
    assert!(matches!(replay.confirm(), Err(Error::Poisoned)));
    assert_eq!(calls(), initial + 3);
    assert_eq!(fs::read(&target).unwrap(), bytes);
    drop(replay);

    fs::remove_file(&flag).unwrap();
    let mut replay = Journal::open(&target, key.as_ref(), true).unwrap();
    let checkpoint = replay.checkpoint();
    assert_eq!(replay.confirm().unwrap(), checkpoint);
    assert_eq!(calls(), initial + 4);
    assert_eq!(
        replay.append_prepared(&intent, data).unwrap(),
        expected.unwrap()
    );
    assert_eq!(calls(), initial + 4);
    drop(replay);

    let mut retry = Journal::open(&target, key.as_ref(), true).unwrap();
    assert_eq!(
        retry.append_prepared(&intent, data).unwrap(),
        expected.unwrap()
    );
    assert_eq!(calls(), initial + 5);
    assert_eq!(fs::read(&target).unwrap(), bytes);
    assert_eq!(
        retry.append_prepared(&intent, data).unwrap(),
        expected.unwrap()
    );
    assert_eq!(
        calls(),
        initial + 5,
        "a known-durable healthy retry needs no extra sync"
    );
    drop(retry);

    fs::OpenOptions::new()
        .append(true)
        .open(&target)
        .unwrap()
        .write_all(b"torn")
        .unwrap();
    let mut recovered = Journal::open(&target, key.as_ref(), true).unwrap();
    assert_eq!(recovered.recovered_tail_bytes(), 4);
    assert_eq!(calls(), initial + 6);
    assert_eq!(
        recovered.append_prepared(&intent, data).unwrap(),
        expected.unwrap()
    );
    assert_eq!(
        calls(),
        initial + 6,
        "the recovery barrier already confirmed this prefix"
    );
    assert_eq!(fs::read(&target).unwrap(), bytes);
}
