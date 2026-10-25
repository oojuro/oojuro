//! Application validation happens before recovery, using only public engine APIs.
use ojo::{Change, Database, Error, Result};
use std::{fs, io::Write, path::PathBuf};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ojo-checked-{:032x}",
            u128::from_le_bytes(ojo::request_id().unwrap())
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn app(db: &mut Database) -> Result<()> {
    assert!(matches!(
        db.commit(
            [200; 16],
            &[Change {
                key: b"forbidden",
                value: Some(b"not written")
            }]
        ),
        Err(Error::ReadOnly)
    ));
    assert!(matches!(db.confirm(), Err(Error::ReadOnly)));
    assert_eq!(
        db.get(b"original")?.as_deref(),
        Some(b"exact\0 original\r\n".as_slice())
    );
    if db.get(b"manifest")?.as_deref() != Some(b"application/v1") {
        return Err(Error::Invalid("application manifest required"));
    }
    Ok(())
}
#[test]
fn missing_or_malformed_application_state_refuses_without_recovering_tail() {
    for key in [None, Some([71; 32])] {
        for malformed in [false, true] {
            let scratch = Scratch::new();
            let path = scratch.0.join("legacy.juro");
            let mut db = Database::create(&path, key.as_ref()).unwrap();
            let mut changes = vec![Change {
                key: b"original",
                value: Some(b"exact\0 original\r\n"),
            }];
            if malformed {
                changes.push(Change {
                    key: b"manifest",
                    value: Some(b"different application"),
                });
            }
            db.commit([1; 16], &changes).unwrap();
            drop(db);
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"torn tail")
                .unwrap();
            let before = fs::read(&path).unwrap();
            for writable in [false, true] {
                let mut called = false;
                let result = Database::open_checked(&path, key.as_ref(), writable, |db| {
                    called = true;
                    app(db)
                });
                assert!(called);
                assert!(matches!(
                    result,
                    Err(Error::Invalid("application manifest required"))
                ));
                assert_eq!(fs::read(&path).unwrap(), before);
            }
        }
    }
}
#[test]
fn successful_readonly_check_recovers_only_after_acceptance_and_restores_requested_mode() {
    for key in [None, Some([72; 32])] {
        let scratch = Scratch::new();
        let path = scratch.0.join("accepted.juro");
        let mut db = Database::create(&path, key.as_ref()).unwrap();
        db.commit(
            [2; 16],
            &[
                Change {
                    key: b"manifest",
                    value: Some(b"application/v1"),
                },
                Change {
                    key: b"original",
                    value: Some(b"exact\0 original\r\n"),
                },
            ],
        )
        .unwrap();
        let checkpoint = db.checkpoint();
        drop(db);
        let complete = fs::read(&path).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"torn tail")
            .unwrap();
        let before = fs::read(&path).unwrap();
        let mut reader = Database::open_checked(&path, key.as_ref(), false, app).unwrap();
        assert_eq!(reader.checkpoint(), checkpoint);
        assert_eq!(reader.recovered_tail_bytes(), 9);
        assert!(matches!(reader.confirm(), Err(Error::ReadOnly)));
        assert_eq!(fs::read(&path).unwrap(), before);
        drop(reader);
        let refused = Database::open_checked(&path, key.as_ref(), true, |db| {
            app(db)?;
            Err(Error::Invalid("caller scope"))
        });
        assert!(matches!(refused, Err(Error::Invalid("caller scope"))));
        assert_eq!(fs::read(&path).unwrap(), before);
        let mut writer = Database::open_checked(&path, key.as_ref(), true, app).unwrap();
        assert_eq!(writer.checkpoint(), checkpoint);
        assert_eq!(writer.recovered_tail_bytes(), 9);
        assert_eq!(fs::read(&path).unwrap(), complete);
        writer
            .commit(
                [3; 16],
                &[Change {
                    key: b"later",
                    value: Some(b"new exact bytes"),
                }],
            )
            .unwrap();
        writer.confirm().unwrap();
        assert_eq!(
            writer.get(b"original").unwrap().as_deref(),
            Some(b"exact\0 original\r\n".as_slice())
        );
        drop(writer);
        let mut reader = Database::open_checked(&path, key.as_ref(), false, app).unwrap();
        assert_eq!(
            reader.get(b"later").unwrap().as_deref(),
            Some(b"new exact bytes".as_slice())
        );
    }
}
#[test]
fn invalid_engine_bytes_never_reach_application_check() {
    let scratch = Scratch::new();
    let path = scratch.0.join("invalid.juro");
    drop(Database::create(&path, None).unwrap());
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 1;
    fs::write(&path, &bytes).unwrap();
    let mut called = false;
    assert!(
        Database::open_checked(&path, None, true, |_| {
            called = true;
            Ok(())
        })
        .is_err()
    );
    assert!(!called);
    assert_eq!(fs::read(&path).unwrap(), bytes);
}
