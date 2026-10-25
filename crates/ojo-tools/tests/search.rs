#![cfg(feature = "search")]
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
            "juro-file-search-{:032x}",
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
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn refused(&self, args: &[&str]) {
        let output = self.run(args);
        assert!(!output.status.success(), "unexpected success: {args:?}");
        assert!(output.stdout.is_empty(), "failure emitted a partial answer");
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn real_event_files_supply_exact_search_originals_raw_and_compressed() {
    for encrypted in [false, true] {
        let s = Scratch::new();
        s.ok(&["keygen", "index-key"]);
        s.ok(&["keygen", "wrong-key"]);
        let mut extra = Vec::new();
        if encrypted {
            s.ok(&["keygen", "source-key"]);
            extra.extend(["--key-file", "source-key"]);
        }
        let run = |args: &[&str]| s.ok(&[args, &extra].concat());
        run(&["init", "sensors.juro"]);
        let originals = [
            b"sensor alpha calibration completed at 21.5 degrees \xcf\x80\n".as_slice(),
            b"sensor beta calibration failed at 18.2 degrees\n",
            b"\xff\0opaque calibration",
            b"",
        ];
        let mut receipts = Vec::new();
        for original in originals {
            fs::write(s.0.join("input"), original).unwrap();
            receipts.push(run(&["append", "sensors.juro", "input"]));
        }
        let original_file = fs::read(s.0.join("sensors.juro")).unwrap();
        let mut expected = None;
        for compression in ["raw", "zstd"] {
            let index = format!("index-{compression}");
            let built = run(&[
                "index",
                "sensors.juro",
                &index,
                "--index-key",
                "index-key",
                "--compression",
                compression,
            ]);
            assert_eq!(built["indexed"], 3);
            assert_eq!(built["opaque_binary_events"], 1);
            let results = run(&[
                "search",
                "sensors.juro",
                &index,
                "calibration",
                "--index-key",
                "index-key",
            ]);
            assert_eq!(results["hits"].as_array().unwrap().len(), 2);
            for hit in results["hits"].as_array().unwrap() {
                let at = receipts
                    .iter()
                    .position(|receipt| receipt["cursor"] == hit["cursor"])
                    .unwrap();
                assert_eq!(hit["accepted_at"], receipts[at]["accepted_at"]);
                let start = hit["start"].as_u64().unwrap() as usize;
                let end = hit["end"].as_u64().unwrap() as usize;
                assert_eq!(
                    hit["excerpt"].as_str().unwrap().as_bytes(),
                    &originals[at][start..end]
                );
                let read = s.run(
                    &[
                        &["read", "sensors.juro", hit["cursor"].as_str().unwrap()],
                        extra.as_slice(),
                    ]
                    .concat(),
                );
                assert!(read.status.success());
                assert_eq!(read.stdout, originals[at]);
            }
            if let Some(expected) = &expected {
                assert_eq!(&results, expected);
            } else {
                expected = Some(results.clone());
            }
            let timestamp = receipts[1]["accepted_at"].as_i64().unwrap();
            let filtered = run(&[
                "search",
                "sensors.juro",
                &index,
                "calibration",
                "--index-key",
                "index-key",
                "--from",
                &timestamp.to_string(),
                "--to",
                &(timestamp + 1).to_string(),
            ]);
            assert!(
                filtered["hits"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|hit| hit["accepted_at"] == timestamp)
            );
            assert!(
                filtered["hits"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|hit| hit["cursor"] == receipts[1]["cursor"])
            );
            s.refused(
                &[
                    &[
                        "search",
                        "sensors.juro",
                        &index,
                        "calibration",
                        "--index-key",
                        "wrong-key",
                    ],
                    extra.as_slice(),
                ]
                .concat(),
            );
            s.refused(
                &[
                    &[
                        "search",
                        "sensors.juro",
                        &index,
                        "calibration",
                        "--index-key",
                        "index-key",
                        "--max-original-bytes",
                        "1",
                    ],
                    extra.as_slice(),
                ]
                .concat(),
            );
            s.refused(
                &[
                    &["index", "sensors.juro", &index, "--index-key", "index-key"],
                    extra.as_slice(),
                ]
                .concat(),
            );
        }
        assert_eq!(fs::read(s.0.join("sensors.juro")).unwrap(), original_file);
        run(&["export", "sensors.juro", "legacy.ojo"]);
        let copy = run(&[
            "search",
            "legacy.ojo",
            "index-raw",
            "calibration",
            "--index-key",
            "index-key",
        ]);
        assert_eq!(Some(copy), expected);
        assert!(!s.0.join("legacy.juro").exists());
        #[cfg(feature = "archive")]
        {
            run(&["archive", "sensors.juro", "archive.juro"]);
            s.refused(
                &[
                    &[
                        "search",
                        "archive.juro",
                        "index-raw",
                        "calibration",
                        "--index-key",
                        "index-key",
                    ],
                    extra.as_slice(),
                ]
                .concat(),
            );
            run(&[
                "index",
                "archive.juro",
                "index-archive",
                "--index-key",
                "index-key",
                "--compression",
                "zstd",
            ]);
            let archived = run(&[
                "search",
                "archive.juro",
                "index-archive",
                "calibration",
                "--index-key",
                "index-key",
            ]);
            assert_eq!(archived["hits"], expected.as_ref().unwrap()["hits"]);
        }
        run(&["init", "unrelated.juro"]);
        s.refused(
            &[
                &[
                    "search",
                    "unrelated.juro",
                    "index-raw",
                    "calibration",
                    "--index-key",
                    "index-key",
                ],
                extra.as_slice(),
            ]
            .concat(),
        );
        fs::write(s.0.join("input"), b"new calibration original").unwrap();
        run(&["append", "sensors.juro", "input"]);
        s.refused(
            &[
                &[
                    "search",
                    "sensors.juro",
                    "index-raw",
                    "calibration",
                    "--index-key",
                    "index-key",
                ],
                extra.as_slice(),
            ]
            .concat(),
        );
    }
}

#[test]
fn bad_options_keys_budgets_and_torn_sources_refuse_without_source_changes() {
    let s = Scratch::new();
    s.ok(&["keygen", "key"]);
    s.ok(&["init", "events.juro"]);
    fs::write(s.0.join("input"), b"sensor calibration event").unwrap();
    s.ok(&["append", "events.juro", "input"]);
    let before = fs::read(s.0.join("events.juro")).unwrap();
    for options in [
        vec!["--compression", "unknown"],
        vec!["--max-index-bytes", "0"],
        vec!["--max-index-bytes", "1"],
    ] {
        let mut args = vec!["index", "events.juro", "new-index", "--index-key", "key"];
        args.extend(options);
        s.refused(&args);
        assert_eq!(fs::read(s.0.join("events.juro")).unwrap(), before);
    }
    s.refused(&["index", "events.juro", "missing-key"]);
    assert!(!s.0.join("missing-key").exists());
    s.ok(&["index", "events.juro", "index", "--index-key", "key"]);
    for options in [
        vec!["--limit", "0"],
        vec!["--from", "9", "--to", "9"],
        vec!["--max-original-bytes", "0"],
        vec!["--max-index-bytes", "1"],
    ] {
        let mut args = vec![
            "search",
            "events.juro",
            "index",
            "calibration",
            "--index-key",
            "key",
        ];
        args.extend(options);
        s.refused(&args);
    }
    s.refused(&["scan", "events.juro", "--index-key", "key"]);
    let mut torn = before;
    torn.extend(b"incomplete-tail");
    fs::write(s.0.join("events.juro"), &torn).unwrap();
    s.refused(&["index", "events.juro", "torn-index", "--index-key", "key"]);
    s.refused(&[
        "search",
        "events.juro",
        "index",
        "calibration",
        "--index-key",
        "key",
    ]);
    assert_eq!(fs::read(s.0.join("events.juro")).unwrap(), torn);
    assert!(!s.0.join("torn-index").exists());
}
