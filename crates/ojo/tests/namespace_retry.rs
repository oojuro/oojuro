//! Opt-in real directory-sync failure regression for an uncertain creation.
#![cfg(target_os = "linux")]
use ojo::{
    Error,
    events::{Journal, Receipt},
};
use std::{fs, os::unix::fs::DirBuilderExt, path::PathBuf};

#[test]
#[ignore = "run scripts/verify-ojo-sync-retry.py --case namespace"]
fn uncertain_creation_needs_directory_confirmation_before_a_new_ack() {
    let path =
        PathBuf::from(std::env::var_os("OJO_SYNC_FAULT_TARGET").expect("use the fault runner"));
    let flag = PathBuf::from(std::env::var_os("OJO_SYNC_FAULT_FLAG").unwrap());
    let counter = PathBuf::from(std::env::var_os("OJO_SYNC_FAULT_COUNTER").unwrap());
    let key = std::env::var_os("OJO_SYNC_FAULT_ENCRYPTED").map(|_| [43; 32]);
    let calls = || fs::read(&counter).unwrap();
    fs::write(
        &flag,
        b"fail directory publication after the initial temporary is synced",
    )
    .unwrap();
    assert!(matches!(
        Journal::create(&path, key.as_ref()),
        Err(Error::Io(_))
    ));
    assert_eq!(calls(), b"SFF");
    assert!(
        path.is_file(),
        "the fully written destination can remain visible after its directory sync failed"
    );
    let opened_path = if std::env::var_os("OJO_SYNC_FAULT_ALIAS").is_some() {
        let alias_directory = path.parent().unwrap().with_file_name(format!(
            "{}-links",
            path.parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&alias_directory)
            .unwrap();
        fs::File::open(alias_directory.parent().unwrap())
            .unwrap()
            .sync_all()
            .unwrap();
        let alias = alias_directory.join("alias.juro");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        fs::File::open(&alias_directory)
            .unwrap()
            .sync_all()
            .unwrap();
        alias
    } else {
        path.clone()
    };

    let mut reader = Journal::open(&opened_path, key.as_ref(), false).unwrap();
    assert!(reader.is_empty());
    assert!(matches!(reader.confirm(), Err(Error::ReadOnly)));
    drop(reader);
    let mut writer = Journal::open(&opened_path, key.as_ref(), true).unwrap();
    let data = b"new original after an uncertain pathname publication\0\xff";
    let intent = writer.prepare(data).unwrap();
    assert!(
        matches!(writer.append_prepared(&intent, data), Err(Error::Io(_))),
        "an append after uncertain creation must confirm its parent before ACK"
    );
    assert!(matches!(writer.confirm(), Err(Error::Poisoned)));
    assert_eq!(calls(), b"SFFF");
    drop(writer);

    let bytes = fs::read(&path).unwrap();
    let mut reader = Journal::open(&opened_path, key.as_ref(), false).unwrap();
    assert_eq!(reader.len(), 1);
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
    let mut retry = Journal::open(&opened_path, key.as_ref(), true).unwrap();
    assert!(matches!(
        retry.append_prepared(&intent, data),
        Err(Error::Io(_))
    ));
    assert_eq!(calls(), b"SFFFF");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    drop(retry);

    fs::remove_file(&flag).unwrap();
    let mut confirmed = Journal::open(&opened_path, key.as_ref(), true).unwrap();
    assert_eq!(confirmed.confirm().unwrap(), confirmed.checkpoint());
    assert_eq!(calls(), b"SFFFFS");
    assert_eq!(
        confirmed.append_prepared(&intent, data).unwrap(),
        expected.unwrap()
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    confirmed.append(b"later healthy append").unwrap();
    assert_eq!(
        calls(),
        b"SFFFFS",
        "healthy operations add no redundant directory sync"
    );
    assert!(path.is_file());
    if opened_path != path {
        assert!(opened_path.is_symlink());
    }
}
