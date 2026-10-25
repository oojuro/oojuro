//! Group durability uses real file/directory barriers, including on exact retry.
#![cfg(target_os = "linux")]
use ojo::{Error, events::Journal};
use std::{fs, path::PathBuf};

#[test]
#[ignore = "manual persistent-filesystem write comparison; set OJO_GROUP_BENCH_ROOT"]
fn compare_individual_and_group_durability() {
    use std::os::fd::AsRawFd;
    let root = PathBuf::from(
        std::env::var_os("OJO_GROUP_BENCH_ROOT").expect("owned persistent scratch required"),
    );
    let data = vec![0x61; 1024];
    for encrypted in [false, true] {
        let key = encrypted.then_some([84; 32]);
        for round in 0..3 {
            for grouped in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let path = root.join(format!("group-{encrypted}-{round}-{grouped}.juro"));
                let mut journal = Journal::create(&path, key.as_ref()).unwrap();
                let file = fs::File::open(&path).unwrap();
                let mut flags: libc::c_long = 0;
                // The benchmark must not inherit transparent filesystem compression.
                assert_eq!(
                    unsafe { libc::ioctl(file.as_raw_fd(), libc::FS_IOC_GETFLAGS, &mut flags) },
                    0
                );
                assert_ne!(
                    flags & 0x400,
                    0,
                    "set chattr +m on the owned benchmark directory"
                );
                drop(file);
                let intents = (0..100)
                    .map(|_| journal.prepare(&data).unwrap())
                    .collect::<Vec<_>>();
                let items = intents
                    .iter()
                    .map(|intent| (intent, data.as_slice()))
                    .collect::<Vec<_>>();
                let start = std::time::Instant::now();
                let receipts = if grouped {
                    journal.append_prepared_batch(&items).unwrap()
                } else {
                    items
                        .iter()
                        .map(|(intent, data)| journal.append_prepared(intent, data).unwrap())
                        .collect()
                };
                let elapsed = start.elapsed();
                assert_eq!(receipts.len(), 100);
                for receipt in receipts {
                    assert_eq!(journal.get(receipt.cursor).unwrap().unwrap().data, data);
                }
                drop(journal);
                let journal = Journal::open(&path, key.as_ref(), false).unwrap();
                assert_eq!(journal.len(), 100);
                drop(journal);
                let bytes = fs::metadata(&path).unwrap().len();
                fs::remove_file(&path).unwrap();
                println!(
                    "{{\"encrypted\":{encrypted},\"round\":{round},\"grouped\":{grouped},\"rows\":100,\"data_bytes_per_row\":1024,\"filesystem_nocompress\":true,\"total_ms\":{},\"file_bytes\":{bytes}}}",
                    elapsed.as_secs_f64() * 1000.0
                );
            }
        }
    }
}

#[test]
#[ignore = "run scripts/verify-ojo-sync-retry.py --case group"]
fn event_group_never_acknowledges_an_unconfirmed_prefix() {
    let path = PathBuf::from(std::env::var_os("OJO_SYNC_FAULT_TARGET").unwrap());
    let flag = PathBuf::from(std::env::var_os("OJO_SYNC_FAULT_FLAG").unwrap());
    let counter = PathBuf::from(std::env::var_os("OJO_SYNC_FAULT_COUNTER").unwrap());
    let key = std::env::var_os("OJO_SYNC_FAULT_ENCRYPTED").map(|_| [83; 32]);
    let calls = || match fs::read(&counter) {
        Ok(bytes) => bytes.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("sync observations unavailable: {error}"),
    };
    let journal = Journal::create(&path, key.as_ref()).unwrap();
    drop(journal);
    let mut journal = Journal::open(&path, key.as_ref(), true).unwrap();
    let a = journal.prepare(b"one\0\xff").unwrap();
    let b = journal.prepare(b"two").unwrap();
    let c = journal.prepare(b"").unwrap();
    let items = [
        (&a, b"one\0\xff".as_slice()),
        (&b, b"two".as_slice()),
        (&c, b"".as_slice()),
    ];
    let initial = calls();
    fs::write(&flag, b"fail the final group barrier").unwrap();
    assert!(matches!(
        journal.append_prepared_batch(&items),
        Err(Error::Io(_))
    ));
    assert!(matches!(journal.confirm(), Err(Error::Poisoned)));
    assert_eq!(calls(), initial + 1);
    drop(journal);
    let original = fs::read(&path).unwrap();
    let mut reader = Journal::open(&path, key.as_ref(), false).unwrap();
    assert_eq!(
        reader.len(),
        3,
        "failed group may have a complete visible prefix"
    );
    let mut expected = Vec::new();
    reader
        .scan(None, 3, |event| {
            expected.push(ojo::events::Receipt {
                cursor: event.cursor,
                accepted_at: event.accepted_at,
            });
            Ok(())
        })
        .unwrap();
    drop(reader);
    let mut retry = Journal::open(&path, key.as_ref(), true).unwrap();
    assert!(
        matches!(retry.append_prepared_batch(&items), Err(Error::Io(_))),
        "replayed duplicates must confirm before a group acknowledgement"
    );
    assert!(matches!(
        retry.get(expected[0].cursor),
        Err(Error::Poisoned)
    ));
    assert_eq!(calls(), initial + 2);
    assert_eq!(fs::read(&path).unwrap(), original);
    drop(retry);
    fs::remove_file(&flag).unwrap();
    let mut retry = Journal::open(&path, key.as_ref(), true).unwrap();
    assert_eq!(retry.append_prepared_batch(&items).unwrap(), expected);
    assert_eq!(calls(), initial + 3);
    assert_eq!(retry.append_prepared_batch(&items).unwrap(), expected);
    assert_eq!(calls(), initial + 3, "healthy exact retry adds no sync");
    let d = retry.prepare(b"new one").unwrap();
    let e = retry.prepare(b"new two").unwrap();
    retry
        .append_prepared_batch(&[(&d, b"new one"), (&e, b"new two")])
        .unwrap();
    let extra = usize::from(std::env::var_os("OJO_SYNC_FAULT_DIRECTORY").is_none());
    assert_eq!(
        calls(),
        initial + 3 + extra,
        "one shared file sync and no repeated directory sync"
    );
}
