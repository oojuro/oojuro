use ojo::{Change, Database, Error};
use std::{fs, path::PathBuf};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "juro-retry-cache-{:032x}",
            u128::from_le_bytes(ojo::request_id().unwrap())
        ));
        let mut create = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            create.mode(0o700);
        }
        create.create(&path).unwrap();
        Self(path)
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
fn evicted_original_batches_retry_exactly_without_reapplying_old_versions() {
    for key in [None, Some([42; 32])] {
        let scratch = Scratch::new();
        let path = scratch.0.join("source.juro");
        let mut db = Database::create(&path, key.as_ref()).unwrap();
        let original = [
            put(b"key", b"first\0\xff"),
            put(b"empty", b""),
            Change {
                key: b"deleted",
                value: None,
            },
        ];
        let first = db.commit(id(1), &original).unwrap();
        // Exceed the 1,024-entry policy while retaining a complete maximum Event batch.
        let mut last = first;
        for n in 2..=1025u128 {
            last = db.commit(id(n), &[put(b"key", &n.to_le_bytes())]).unwrap();
        }
        let checkpoint = db.checkpoint();
        let bytes = fs::read(&path).unwrap();
        for _ in 0..3 {
            assert_eq!(db.commit(id(1), &original).unwrap(), first);
            assert_eq!(
                db.commit(id(1025), &[put(b"key", &1025u128.to_le_bytes())])
                    .unwrap(),
                last
            );
            assert_eq!(db.checkpoint(), checkpoint);
            assert_eq!(db.get(b"key").unwrap().unwrap(), 1025u128.to_le_bytes());
            assert_eq!(db.get(b"empty").unwrap().unwrap(), b"");
            assert_eq!(db.get(b"deleted").unwrap(), None);
        }
        // Equal final state is insufficient: exact operation order/empty/delete matter.
        assert!(matches!(
            db.commit(id(1), &[original[1], original[0], original[2]]),
            Err(Error::Conflict)
        ));
        assert!(matches!(
            db.commit(
                id(1),
                &[
                    original[0],
                    Change {
                        key: b"empty",
                        value: None
                    },
                    original[2]
                ]
            ),
            Err(Error::Conflict)
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        drop(db);

        let mut db = Database::open(&path, key.as_ref(), true).unwrap();
        assert_eq!(db.commit(id(1), &original).unwrap(), first);
        assert_eq!(
            db.commit(id(1025), &[put(b"key", &1025u128.to_le_bytes())])
                .unwrap(),
            last
        );
        assert_eq!(db.checkpoint(), checkpoint);
        assert_eq!(fs::read(&path).unwrap(), bytes);

        #[cfg(feature = "archive")]
        {
            let archive = scratch.0.join("archive.juro");
            db.archive_to(&archive).unwrap();
            drop(db);
            let bytes = fs::read(&archive).unwrap();
            let mut archived = Database::open(&archive, key.as_ref(), true).unwrap();
            let checkpoint = archived.checkpoint();
            let retried = archived.commit(id(1), &original).unwrap();
            assert_eq!(retried.checkpoint.sequence, first.checkpoint.sequence);
            assert_eq!(retried.operations, first.operations);
            assert!(matches!(
                archived.commit(id(1), &[put(b"key", b"changed")]),
                Err(Error::Conflict)
            ));
            assert_eq!(archived.checkpoint(), checkpoint);
            assert_eq!(
                archived.get(b"key").unwrap().unwrap(),
                1025u128.to_le_bytes()
            );
            assert_eq!(fs::read(&archive).unwrap(), bytes);
        }
    }
}

#[cfg(unix)]
#[test]
fn evicted_retry_revalidates_the_whole_committed_frame_before_acknowledging() {
    use std::os::unix::fs::FileExt;
    for key in [None, Some([42; 32])] {
        let scratch = Scratch::new();
        let path = scratch.0.join("source.juro");
        let mut db = Database::create(&path, key.as_ref()).unwrap();
        let changes = [put(b"key", b"first\0\xff")];
        db.commit(id(1), &changes).unwrap();
        for n in 2..=1025u128 {
            db.commit(id(n), &[put(b"key", &n.to_le_bytes())]).unwrap();
        }
        // Frozen wire0.1: 64-byte file header + 96-byte transaction header.
        // Bypass the cooperative inode lock to model storage corruption.
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut byte = [0];
        file.read_exact_at(&mut byte, 64 + 96).unwrap();
        byte[0] ^= 1;
        file.write_all_at(&byte, 64 + 96).unwrap();
        assert!(
            matches!(db.commit(id(1), &changes), Err(Error::Corrupt(_))),
            "an evicted hash must not replace complete-frame validation"
        );
    }
}
