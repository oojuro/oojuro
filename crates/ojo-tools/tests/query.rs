#![cfg(feature = "search")]
use ojo::events::Journal;
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "juro-query-{:032x}",
            u128::from_le_bytes(ojo::request_id().unwrap())
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ojo"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn refused(&self, args: &[&str]) {
        let out = self.run(args);
        assert!(!out.status.success(), "{args:?}");
        assert!(
            out.stdout.is_empty(),
            "refusal emitted stale/partial results"
        );
    }
    fn generation(&self) -> String {
        fs::read_to_string(self.0.join("source.juro.cache/CURRENT")).unwrap()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn file_inodes(path: &std::path::Path) -> std::collections::BTreeSet<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let mut result = std::collections::BTreeSet::new();
    for entry in fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        let metadata = fs::symlink_metadata(&path).unwrap();
        if metadata.is_dir() {
            result.extend(file_inodes(&path));
        } else if metadata.is_file() {
            result.insert((metadata.dev(), metadata.ino()));
        }
    }
    result
}

#[test]
fn cached_queries_refresh_actual_plain_encrypted_raw_zstd_files_without_duplicates() {
    for encrypted in [false, true] {
        for compression in ["raw", "zstd"] {
            let s = Scratch::new();
            s.ok(&["keygen", "index.key"]);
            let key = encrypted.then_some([47; 32]);
            if let Some(key) = key {
                fs::write(s.0.join("source.key"), key).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(s.0.join("source.key"), fs::Permissions::from_mode(0o600))
                        .unwrap();
                }
            }
            let mut journal = Journal::create(s.0.join("source.juro"), key.as_ref()).unwrap();
            let originals = [
                b"alpha calibration completed \xcf\x80\r\n".as_slice(),
                b"",
                b"\xffcalibration",
            ];
            for data in originals {
                journal.append(data).unwrap();
            }
            drop(journal);
            let initial_source = fs::read(s.0.join("source.juro")).unwrap();
            let extra = if encrypted {
                vec!["--key-file", "source.key"]
            } else {
                vec![]
            };
            let query = [
                &[
                    "query",
                    "source.juro",
                    "calibration",
                    "--index-key",
                    "index.key",
                    "--compression",
                    compression,
                ][..],
                &extra,
            ]
            .concat();
            let first = s.ok(&query);
            assert_eq!(first["cache"]["state"], "built");
            assert_eq!(first["cache"]["indexed_events"], 2);
            assert_eq!(first["cache"]["added_binary_events"], 1);
            assert_eq!(first["hits"].as_array().unwrap().len(), 1);
            let initial = s.generation();
            #[cfg(unix)]
            let initial_inodes =
                file_inodes(&s.0.join("source.juro.cache").join(&initial).join("index"));
            let second = s.ok(&query);
            assert_eq!(second["cache"]["state"], "reused");
            assert_eq!(second["hits"], first["hits"]);
            assert_eq!(s.generation(), initial);

            let mut journal = Journal::open(s.0.join("source.juro"), key.as_ref(), true).unwrap();
            let intent = journal.prepare(b"beta calibration failed").unwrap();
            let added = journal
                .append_prepared(&intent, b"beta calibration failed")
                .unwrap();
            assert_eq!(
                journal
                    .append_prepared(&intent, b"beta calibration failed")
                    .unwrap(),
                added
            );
            journal.append(b"\xfeopaque").unwrap();
            drop(journal);
            let source = fs::read(s.0.join("source.juro")).unwrap();
            let refreshed = s.ok(&query);
            assert_eq!(refreshed["cache"]["state"], "refreshed");
            assert_eq!(refreshed["cache"]["added_text_events"], 1);
            assert_eq!(refreshed["cache"]["added_binary_events"], 1);
            assert_eq!(refreshed["cache"]["indexed_events"], 3);
            assert_eq!(refreshed["hits"].as_array().unwrap().len(), 2);
            assert_ne!(s.generation(), initial);
            assert!(!s.0.join("source.juro.cache").join(initial).exists());
            #[cfg(unix)]
            {
                let new_inodes = file_inodes(
                    &s.0.join("source.juro.cache")
                        .join(s.generation())
                        .join("index"),
                );
                assert!(
                    initial_inodes.intersection(&new_inodes).next().is_some(),
                    "refresh did not retain immutable packs by hard link"
                );
            }
            for hit in refreshed["hits"].as_array().unwrap() {
                let read = s.run(
                    &[
                        &["read", "source.juro", hit["cursor"].as_str().unwrap()][..],
                        &extra,
                    ]
                    .concat(),
                );
                assert!(read.status.success());
                let start = hit["start"].as_u64().unwrap() as usize;
                let end = hit["end"].as_u64().unwrap() as usize;
                assert_eq!(
                    hit["excerpt"].as_str().unwrap().as_bytes(),
                    &read.stdout[start..end]
                );
            }
            s.ok(&[
                &[
                    "index",
                    "source.juro",
                    "fresh",
                    "--index-key",
                    "index.key",
                    "--compression",
                    compression,
                ][..],
                &extra,
            ]
            .concat());
            let rebuilt = s.ok(&[
                &[
                    "search",
                    "source.juro",
                    "fresh",
                    "calibration",
                    "--index-key",
                    "index.key",
                ][..],
                &extra,
            ]
            .concat());
            assert_eq!(
                refreshed["hits"], rebuilt["hits"],
                "incremental results differ from full rebuild"
            );
            let from = added.accepted_at.to_string();
            let to = (added.accepted_at + 1).to_string();
            let filtered = s.ok(&[query.as_slice(), &["--from", &from, "--to", &to]].concat());
            assert!(
                filtered["hits"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|hit| hit["accepted_at"] == added.accepted_at)
            );
            assert_eq!(fs::read(s.0.join("source.juro")).unwrap(), source);
            assert_eq!(s.ok(&query)["cache"]["state"], "reused");
            let read = s.run(
                &[
                    &[
                        "read",
                        "source.juro",
                        first["hits"][0]["cursor"].as_str().unwrap(),
                    ][..],
                    &extra,
                ]
                .concat(),
            );
            assert_eq!(read.stdout, originals[0]);
            fs::write(s.0.join("source.juro"), &initial_source).unwrap();
            s.refused(&query);
            assert_eq!(fs::read(s.0.join("source.juro")).unwrap(), initial_source);
        }
    }
}

#[test]
fn failed_refresh_keeps_previous_generation_and_never_serves_stale_results() {
    let s = Scratch::new();
    s.ok(&["keygen", "index.key"]);
    let mut journal = Journal::create(s.0.join("source.juro"), None).unwrap();
    journal.append(b"small calibration event").unwrap();
    drop(journal);
    let query = [
        "query",
        "source.juro",
        "calibration",
        "--index-key",
        "index.key",
    ];
    s.ok(&query);
    let initial = s.generation();
    let mut journal = Journal::open(s.0.join("source.juro"), None, true).unwrap();
    journal
        .append(&[b"calibration ".as_slice(), &vec![b'x'; 256_000]].concat())
        .unwrap();
    drop(journal);
    let source = fs::read(s.0.join("source.juro")).unwrap();
    s.refused(&[&query[..], &["--max-index-bytes", "200000"]].concat());
    assert_eq!(s.generation(), initial);
    assert_eq!(fs::read(s.0.join("source.juro")).unwrap(), source);
    let refreshed = s.ok(&query);
    assert_eq!(refreshed["cache"]["state"], "refreshed");
    assert_eq!(refreshed["hits"].as_array().unwrap().len(), 2);
    assert_eq!(
        fs::read_dir(s.0.join("source.juro.cache"))
            .unwrap()
            .filter_map(|e| {
                let e = e.unwrap();
                e.file_name().to_str().map(|name| name.starts_with("g-"))
            })
            .filter(|v| *v)
            .count(),
        1
    );
}

#[test]
fn wrong_keys_replaced_or_torn_sources_and_tampered_checkpoints_refuse() {
    let s = Scratch::new();
    s.ok(&["keygen", "index.key"]);
    s.ok(&["keygen", "wrong.key"]);
    s.ok(&["init", "source.juro"]);
    fs::write(s.0.join("data"), b"calibration").unwrap();
    s.ok(&["append", "source.juro", "data"]);
    let query = [
        "query",
        "source.juro",
        "calibration",
        "--index-key",
        "index.key",
    ];
    s.ok(&query);
    let generation = s.generation();
    let source = fs::read(s.0.join("source.juro")).unwrap();
    s.refused(&[
        "query",
        "source.juro",
        "calibration",
        "--index-key",
        "wrong.key",
    ]);
    fs::write(
        s.0.join("source.juro"),
        [source.as_slice(), b"torn"].concat(),
    )
    .unwrap();
    s.refused(&query);
    assert_eq!(s.generation(), generation);
    assert_eq!(
        fs::read(s.0.join("source.juro")).unwrap(),
        [source.as_slice(), b"torn"].concat()
    );
    fs::write(s.0.join("source.juro"), &source).unwrap();
    let checkpoint_path =
        s.0.join("source.juro.cache")
            .join(&generation)
            .join("checkpoint");
    let checkpoint = fs::read(&checkpoint_path).unwrap();
    let mut tampered = checkpoint.clone();
    tampered[55] ^= 1;
    fs::write(&checkpoint_path, tampered).unwrap();
    s.refused(&query);
    fs::write(&checkpoint_path, checkpoint).unwrap();
    s.ok(&["init", "unrelated.juro"]);
    fs::rename(s.0.join("unrelated.juro"), s.0.join("source.juro")).unwrap();
    let replacement = fs::read(s.0.join("source.juro")).unwrap();
    s.refused(&query);
    assert_eq!(fs::read(s.0.join("source.juro")).unwrap(), replacement);
    assert_eq!(s.generation(), generation);
}

#[test]
fn empty_and_binary_only_journals_refresh_and_unrecognized_caches_are_preserved() {
    let s = Scratch::new();
    s.ok(&["keygen", "index.key"]);
    s.ok(&["init", "source.juro"]);
    let query = [
        "query",
        "source.juro",
        "calibration",
        "--index-key",
        "index.key",
    ];
    assert!(s.ok(&query)["hits"].as_array().unwrap().is_empty());
    let mut journal = Journal::open(s.0.join("source.juro"), None, true).unwrap();
    journal.append(b"\xff\0binary").unwrap();
    drop(journal);
    let refreshed = s.ok(&query);
    assert_eq!(refreshed["cache"]["state"], "refreshed");
    assert_eq!(refreshed["cache"]["indexed_events"], 0);
    assert_eq!(refreshed["cache"]["added_binary_events"], 1);
    fs::create_dir(s.0.join("unrelated")).unwrap();
    fs::write(s.0.join("unrelated/keep"), b"original").unwrap();
    s.refused(&[&query[..], &["--cache", "unrelated"]].concat());
    assert_eq!(fs::read(s.0.join("unrelated/keep")).unwrap(), b"original");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(s.0.join("source.juro.cache/LOCK"))
        .unwrap();
    lock.try_lock().unwrap();
    s.refused(&query);
    lock.unlock().unwrap();
    s.ok(&query);
}

#[test]
fn refresh_cannot_raise_the_caches_stored_original_byte_ceiling() {
    let s = Scratch::new();
    s.ok(&["keygen", "index.key"]);
    s.ok(&["init", "source.juro"]);
    let query = [
        "query",
        "source.juro",
        "calibration",
        "--index-key",
        "index.key",
    ];
    s.ok(&[&query[..], &["--max-original-bytes", "1"]].concat());
    fs::write(s.0.join("data"), b"calibration completed").unwrap();
    s.ok(&["append", "source.juro", "data"]);
    s.refused(&query);
    s.refused(&[&query[..], &["--max-original-bytes", "1000000"]].concat());
    assert_eq!(
        s.ok(&[&query[..], &["--cache", "fresh-cache"]].concat())["hits"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn managed_cache_never_accepts_a_source_or_key_inside_its_directory() {
    let s = Scratch::new();
    s.ok(&["keygen", "index.key"]);
    s.ok(&["init", "source.juro"]);
    let query = [
        "query",
        "source.juro",
        "calibration",
        "--index-key",
        "index.key",
    ];
    s.ok(&query);
    let generation = s.generation();
    fs::copy(
        s.0.join("source.juro"),
        s.0.join("source.juro.cache/nested.juro"),
    )
    .unwrap();
    s.refused(&[
        "query",
        "source.juro.cache/nested.juro",
        "calibration",
        "--index-key",
        "index.key",
        "--cache",
        "source.juro.cache",
    ]);
    let source = fs::read(s.0.join("source.juro")).unwrap();
    assert_eq!(
        fs::read(s.0.join("source.juro.cache/nested.juro")).unwrap(),
        source
    );
    fs::copy(
        s.0.join("index.key"),
        s.0.join("source.juro.cache/nested.key"),
    )
    .unwrap();
    s.refused(&[
        "query",
        "source.juro",
        "calibration",
        "--index-key",
        "source.juro.cache/nested.key",
    ]);
    assert_eq!(s.generation(), generation);
}
