//! Removed local entry points must fail without touching legacy homes or credentials.
mod common;
use common::Scratch;
use std::process::Command;
#[test]
fn legacy_commands_and_global_flags_are_rejected_without_side_effects() {
    let root = Scratch::new("retired-cli");
    let home = root.path("untouched");
    for args in [
        vec!["remember", "private-synthetic-text"],
        vec!["mcp"],
        vec!["hook", "Stop"],
        vec!["--home", home.to_str().unwrap(), "cloud", "connection"],
        vec!["--direct", "stats"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_oojuro"))
            .args(args)
            .env("OOJURO_HOME", &home)
            .current_dir(&root.0)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("unsupported_command"));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("private-synthetic-text"));
        assert!(!home.exists());
    }
}
