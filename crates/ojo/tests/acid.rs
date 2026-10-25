use ojo::{Change, Database, Error};
use std::{fs, path::PathBuf};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ojo-test-{:032x}",
            u128::from_le_bytes(ojo::request_id().unwrap())
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self) -> PathBuf {
        self.0.join("test.ojo")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn id(n: u128) -> [u8; 16] {
    n.to_le_bytes()
}
fn put<'a>(key: &'a [u8], value: &'a [u8]) -> Change<'a> {
    Change {
        key,
        value: Some(value),
    }
}

#[test]
fn raw_binary_versions_deletes_and_exact_retries() {
    for key in [None, Some([42; 32])] {
        let scratch = Scratch::new();
        let path = scratch.file();
        let mut db = Database::create(&path, key.as_ref()).unwrap();
        let original = db.checkpoint();
        let changes = [
            put(b"\xff\0", b"\0\xff\r\n"),
            put(b"empty", b""),
            put(b"a", b"old"),
            put(b"a", b"new"),
        ];
        let commit = db.commit(id(1), &changes).unwrap();
        assert_eq!(db.commit(id(1), &changes).unwrap(), commit);
        let length = fs::metadata(&path).unwrap().len();
        assert!(matches!(
            db.commit(id(1), &[put(b"a", b"different")]),
            Err(Error::Conflict)
        ));
        assert_eq!(fs::metadata(&path).unwrap().len(), length);
        assert_eq!(db.get(b"a").unwrap(), Some(b"new".to_vec()));
        assert_eq!(db.get(b"empty").unwrap(), Some(vec![]));
        assert_eq!(db.get(b"\xff\0").unwrap(), Some(b"\0\xff\r\n".to_vec()));
        assert_eq!(db.get(b"missing").unwrap(), None);
        db.require_checkpoint(&original).unwrap();
        db.commit(
            id(2),
            &[Change {
                key: b"empty",
                value: None,
            }],
        )
        .unwrap();
        assert_eq!(db.get(b"empty").unwrap(), None);
        let mut versions = Vec::new();
        db.scan(|sequence, operation, key, value| {
            versions.push((sequence, operation, key.to_vec(), value.map(<[u8]>::to_vec)))
        })
        .unwrap();
        assert_eq!(versions.len(), 5);
        assert_eq!(versions[2].3.as_deref(), Some(b"old".as_slice()));
        let checkpoint = db.checkpoint();
        drop(db);
        assert_eq!(fs::read(&path).unwrap()[64 + 36], 0);
        let mut db = Database::open(&path, key.as_ref(), true).unwrap();
        db.require_checkpoint(&checkpoint).unwrap();
        assert_eq!(db.commit(id(1), &changes).unwrap(), commit);
        // get/scan must not move the next append away from the committed end.
        assert_eq!(db.get(b"a").unwrap(), Some(b"new".to_vec()));
        db.commit(id(3), &[put(b"a", b"last")]).unwrap();
        drop(db);
        let mut db = Database::open(&path, key.as_ref(), false).unwrap();
        assert_eq!(db.get(b"a").unwrap(), Some(b"last".to_vec()));
        assert!(matches!(db.commit(id(4), &changes), Err(Error::ReadOnly)));
    }
}

#[test]
fn every_torn_byte_recovers_only_whole_batches() {
    for key in [None, Some([42; 32])] {
        let scratch = Scratch::new();
        let path = scratch.file();
        let mut db = Database::create(&path, key.as_ref()).unwrap();
        db.commit(id(1), &[put(b"a", b"first")]).unwrap();
        let prefix = fs::metadata(&path).unwrap().len() as usize;
        let checkpoint = db.checkpoint();
        let changes = [put(b"a", b"second"), put(b"b", b"second")];
        let commit = db.commit(id(2), &changes).unwrap();
        drop(db);
        let all = fs::read(&path).unwrap();
        for end in prefix..=all.len() {
            fs::write(&path, &all[..end]).unwrap();
            let mut reader = Database::open(&path, key.as_ref(), false).unwrap();
            let complete = end == all.len();
            assert_eq!(reader.get(b"b").unwrap().is_some(), complete);
            assert_eq!(
                reader.recovered_tail_bytes(),
                if complete { 0 } else { (end - prefix) as u64 }
            );
            assert_eq!(fs::read(&path).unwrap(), all[..end]);
            drop(reader);
            let mut writer = Database::open(&path, key.as_ref(), true).unwrap();
            writer.require_checkpoint(&checkpoint).unwrap();
            assert_eq!(
                writer.commit(id(2), &changes).unwrap().checkpoint.sequence,
                commit.checkpoint.sequence
            );
            drop(writer);
            let mut reader = Database::open(&path, key.as_ref(), false).unwrap();
            assert_eq!(reader.get(b"a").unwrap().unwrap(), b"second");
            assert_eq!(reader.get(b"b").unwrap().unwrap(), b"second");
        }
    }
}

#[test]
fn every_corrupted_byte_is_refused_without_repair() {
    for key in [None, Some([42; 32])] {
        let scratch = Scratch::new();
        let path = scratch.file();
        let mut db = Database::create(&path, key.as_ref()).unwrap();
        db.commit(id(1), &[put(b"a", b"first"), put(b"b", b"second")])
            .unwrap();
        drop(db);
        let all = fs::read(&path).unwrap();
        for at in 0..all.len() {
            let mut corrupt = all.clone();
            corrupt[at] ^= 0x80;
            fs::write(&path, &corrupt).unwrap();
            assert!(
                Database::open(&path, key.as_ref(), true).is_err(),
                "corruption at {at}"
            );
            assert_eq!(fs::read(&path).unwrap(), corrupt);
        }
    }
}

#[test]
fn keys_downgrades_and_invalid_inputs_are_refused() {
    let scratch = Scratch::new();
    let path = scratch.file();
    let mut db = Database::create(&path, Some(&[42; 32])).unwrap();
    let checkpoint = db.checkpoint();
    for changes in [&[][..], &[put(b"", b"text")][..]] {
        assert!(db.commit(id(1), changes).is_err());
        assert_eq!(db.checkpoint(), checkpoint);
    }
    assert!(db.commit(id(0), &[put(b"a", b"b")]).is_err());
    drop(db);
    assert!(matches!(
        Database::open(&path, None, true),
        Err(Error::KeyRequired)
    ));
    assert!(matches!(
        Database::open(&path, Some(&[43; 32]), true),
        Err(Error::WrongKey)
    ));
    assert!(Database::create(&path, None).is_err());
    fs::remove_file(&path).unwrap();
    drop(Database::create(&path, None).unwrap());
    assert!(matches!(
        Database::open(&path, Some(&[42; 32]), false),
        Err(Error::Downgrade)
    ));
    let db = Database::open(&path, None, false).unwrap();
    assert!(matches!(
        db.require_checkpoint(&checkpoint),
        Err(Error::Rollback)
    ));
}

#[test]
fn locks_exclude_writers_and_allow_shared_readers() {
    let scratch = Scratch::new();
    let path = scratch.file();
    let db = Database::create(&path, None).unwrap();
    assert!(matches!(
        Database::open(&path, None, true),
        Err(Error::Locked)
    ));
    assert!(matches!(
        Database::open(&path, None, false),
        Err(Error::Locked)
    ));
    drop(db);
    let a = Database::open(&path, None, false).unwrap();
    let b = Database::open(&path, None, false).unwrap();
    assert!(matches!(
        Database::open(&path, None, true),
        Err(Error::Locked)
    ));
    drop((a, b));
    Database::open(&path, None, true).unwrap();
}

#[test]
fn last_of_1024_operations_reads_exactly() {
    let scratch = Scratch::new();
    let mut db = Database::create(scratch.file(), None).unwrap();
    let keys: Vec<_> = (0u64..1024).map(u64::to_le_bytes).collect();
    let changes: Vec<_> = keys.iter().map(|key| put(key, key)).collect();
    db.commit(id(1), &changes).unwrap();
    for index in [0, 511, 1023] {
        assert_eq!(db.get(&keys[index]).unwrap().unwrap(), keys[index]);
    }
}

#[test]
fn direct_raw_reads_detect_changed_values_after_open() {
    use std::io::{Seek, SeekFrom, Write};
    let scratch = Scratch::new();
    let path = scratch.file();
    let mut db = Database::create(&path, None).unwrap();
    db.commit(id(1), &[put(b"a", b"first"), put(b"b", b"second")])
        .unwrap();
    drop(db);
    let mut db = Database::open(&path, None, false).unwrap();
    // Deliberately bypass advisory locking to model external byte corruption.
    let mut external = fs::OpenOptions::new().write(true).open(&path).unwrap();
    external.seek(SeekFrom::Start(64 + 96 + 16 + 1)).unwrap();
    external.write_all(b"X").unwrap();
    assert!(matches!(db.get(b"a"), Err(Error::Corrupt(_))));
    assert_eq!(db.get(b"b").unwrap().unwrap(), b"second");
    assert!(db.scan(|_, _, _, _| {}).is_err());
    drop(db);
    assert!(Database::open(&path, None, true).is_err());
}

#[test]
fn reopened_retry_first_conflict_does_not_replace_the_original_fingerprint() {
    for key in [None, Some([42; 32])] {
        let scratch = Scratch::new();
        let path = scratch.file();
        let changes = [put(b"first", b"exact\0\xff"), put(b"empty", b"")];
        let mut db = Database::create(&path, key.as_ref()).unwrap();
        let original = db.commit(id(1), &changes).unwrap();
        drop(db);
        let before = fs::read(&path).unwrap();
        let mut db = Database::open(&path, key.as_ref(), true).unwrap();
        assert!(matches!(
            db.commit(id(1), &[put(b"first", b"changed")]),
            Err(Error::Conflict)
        ));
        assert_eq!(db.commit(id(1), &changes).unwrap(), original);
        assert_eq!(db.commit(id(1), &changes).unwrap(), original);
        assert_eq!(fs::read(&path).unwrap(), before);
        db.commit(id(2), &[put(b"after-retry", b"new transaction")])
            .unwrap();
        drop(db);
        let mut db = Database::open(&path, key.as_ref(), false).unwrap();
        assert_eq!(db.get(b"after-retry").unwrap().unwrap(), b"new transaction");
    }
}

#[test]
fn encoded_offsets_cover_variable_binary_keys_empty_values_and_tombstones() {
    for key in [None, Some([42; 32])] {
        let scratch = Scratch::new();
        let path = scratch.file();
        let keys = (0u64..1024)
            .map(|i| {
                let mut bytes = i.to_be_bytes().to_vec();
                bytes.extend(std::iter::repeat_n(0xff, i as usize % 23));
                bytes
            })
            .collect::<Vec<_>>();
        let values = (0usize..1024)
            .map(|i| (i % 19 != 0).then(|| vec![i as u8; if i % 17 == 0 { 0 } else { i % 1301 }]))
            .collect::<Vec<_>>();
        let changes = keys
            .iter()
            .zip(&values)
            .map(|(key, value)| Change {
                key,
                value: value.as_deref(),
            })
            .collect::<Vec<_>>();
        let mut db = Database::create(&path, key.as_ref()).unwrap();
        db.commit(id(1), &changes).unwrap();
        for i in [0, 1, 17, 19, 511, 1023] {
            assert_eq!(db.get(&keys[i]).unwrap(), values[i]);
        }
        drop(db);
        let mut db = Database::open(&path, key.as_ref(), false).unwrap();
        for i in [0, 1, 17, 19, 511, 1023] {
            assert_eq!(db.get(&keys[i]).unwrap(), values[i]);
        }
        #[cfg(feature = "archive")]
        {
            let archive = scratch.0.join("archive.ojo");
            db.archive_to(&archive).unwrap();
            let mut archive = Database::open(archive, key.as_ref(), false).unwrap();
            for i in [0, 1, 17, 19, 511, 1023] {
                assert_eq!(archive.get(&keys[i]).unwrap(), values[i]);
            }
        }
    }
}

#[test]
fn encoded_and_tombstone_reads_still_verify_the_whole_containing_transaction() {
    use std::io::{Seek, SeekFrom, Write};
    for key in [None, Some([42; 32])] {
        let scratch = Scratch::new();
        let path = scratch.file();
        let mut db = Database::create(&path, key.as_ref()).unwrap();
        db.commit(
            id(1),
            &[
                put(b"a", b"first"),
                Change {
                    key: b"deleted",
                    value: None,
                },
                put(b"b", b"neighbor"),
            ],
        )
        .unwrap();
        drop(db);
        let mut db = Database::open(&path, key.as_ref(), false).unwrap();
        assert_eq!(db.get(b"deleted").unwrap(), None);
        assert_eq!(db.get(b"a").unwrap().unwrap(), b"first");
        let mut external = fs::OpenOptions::new().write(true).open(&path).unwrap();
        external.seek(SeekFrom::End(-49)).unwrap();
        external.write_all(b"X").unwrap();
        assert!(matches!(db.get(b"deleted"), Err(Error::Corrupt(_))));
        if key.is_some() {
            assert!(matches!(db.get(b"a"), Err(Error::Corrupt(_))));
        }
        assert!(db.scan(|_, _, _, _| {}).is_err());
    }
}

#[test]
fn crash_child() {
    let Some(path) = std::env::var_os("OJO_CRASH_FIXTURE") else {
        return;
    };
    let mut db = Database::create(path, None).unwrap();
    db.commit(
        id(1),
        &[put(b"a", b"acknowledged"), put(b"b", b"acknowledged")],
    )
    .unwrap();
    // No Drop/normal close; the OS must release the writer lock and retain acknowledged data.
    std::process::exit(73);
}
#[test]
fn acknowledged_batch_survives_process_exit() {
    let scratch = Scratch::new();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child"])
        .env("OJO_CRASH_FIXTURE", scratch.file())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(73));
    let mut db = Database::open(scratch.file(), None, true).unwrap();
    assert_eq!(db.get(b"a").unwrap().unwrap(), b"acknowledged");
    assert_eq!(db.get(b"b").unwrap().unwrap(), b"acknowledged");
}

#[cfg(target_os = "linux")]
#[test]
fn io_failure_child() {
    let Some(path) = std::env::var_os("OJO_IO_FAILURE_FIXTURE") else {
        return;
    };
    let mut db = Database::create(path, None).unwrap();
    let limit = libc::rlimit {
        rlim_cur: 114,
        rlim_max: 114,
    };
    // A per-process kernel file-size limit forces a partial write, then EFBIG.
    unsafe {
        assert_ne!(libc::signal(libc::SIGXFSZ, libc::SIG_IGN), libc::SIG_ERR);
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
    }
    assert!(matches!(
        db.commit(id(1), &[put(b"a", b"unacknowledged")]),
        Err(Error::Io(_))
    ));
    assert!(matches!(db.get(b"a"), Err(Error::Poisoned)));
    assert!(matches!(
        db.commit(id(1), &[put(b"a", b"unacknowledged")]),
        Err(Error::Poisoned)
    ));
    std::process::exit(74);
}
#[cfg(target_os = "linux")]
#[test]
fn partial_io_failure_poisons_writer_and_recovery_allows_same_retry() {
    let scratch = Scratch::new();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "io_failure_child"])
        .env("OJO_IO_FAILURE_FIXTURE", scratch.file())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(74));
    assert_eq!(fs::metadata(scratch.file()).unwrap().len(), 114);
    let mut db = Database::open(scratch.file(), None, true).unwrap();
    assert_eq!(db.recovered_tail_bytes(), 50);
    assert_eq!(db.get(b"a").unwrap(), None);
    db.commit(id(1), &[put(b"a", b"unacknowledged")]).unwrap();
    assert_eq!(db.get(b"a").unwrap().unwrap(), b"unacknowledged");
}

#[cfg(feature = "archive")]
#[test]
fn separate_archive_preserves_history_encryption_retries_and_original() {
    for key in [None, Some([42; 32])] {
        let scratch = Scratch::new();
        let path = scratch.file();
        let destination = scratch.0.join("archive.ojo");
        let mut db = Database::create(&path, key.as_ref()).unwrap();
        let payload = vec![b'x'; 65536];
        let changes = [put(b"a", &payload), put(b"b", b"old")];
        db.commit(id(1), &changes).unwrap();
        db.commit(
            id(2),
            &[Change {
                key: b"b",
                value: None,
            }],
        )
        .unwrap();
        let original = fs::read(&path).unwrap();
        let checkpoint = db.checkpoint();
        let archive_checkpoint = db.archive_to(&destination).unwrap();
        assert_ne!(checkpoint.file_id, archive_checkpoint.file_id);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(db.checkpoint(), checkpoint);
        let archived = fs::read(&destination).unwrap();
        assert!(archived.len() < original.len());
        assert_eq!(archived[64 + 36], 1);
        assert!(db.archive_to(&destination).is_err());
        assert_eq!(fs::read(&destination).unwrap(), archived);
        let mut archive = Database::open(&destination, key.as_ref(), true).unwrap();
        archive.require_checkpoint(&archive_checkpoint).unwrap();
        assert!(matches!(
            archive.require_checkpoint(&checkpoint),
            Err(Error::Rollback)
        ));
        assert_eq!(archive.get(b"a").unwrap().unwrap(), payload);
        assert_eq!(archive.get(b"b").unwrap(), None);
        assert_eq!(
            archive.commit(id(1), &changes).unwrap().checkpoint.sequence,
            1
        );
        assert_eq!(fs::read(&destination).unwrap(), archived);
        let mut old = Vec::new();
        let mut new = Vec::new();
        db.scan(|s, o, k, v| old.push((s, o, k.to_vec(), v.map(<[u8]>::to_vec))))
            .unwrap();
        archive
            .scan(|s, o, k, v| new.push((s, o, k.to_vec(), v.map(<[u8]>::to_vec))))
            .unwrap();
        assert_eq!(old, new);
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 2);
    }
}

#[cfg(feature = "archive")]
#[test]
fn archive_failure_removes_only_its_temporary_file() {
    use std::io::{Seek, SeekFrom, Write};
    let scratch = Scratch::new();
    let path = scratch.file();
    let destination = scratch.0.join("archive.ojo");
    let mut db = Database::create(&path, None).unwrap();
    db.commit(id(1), &[put(b"a", b"first")]).unwrap();
    let mut external = fs::OpenOptions::new().write(true).open(&path).unwrap();
    external.seek(SeekFrom::Start(64 + 96 + 16 + 1)).unwrap();
    external.write_all(b"X").unwrap();
    let original = fs::read(&path).unwrap();
    assert!(db.archive_to(&destination).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(!destination.exists());
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
}
