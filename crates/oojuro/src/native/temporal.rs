//! Bounded stdin-only Temporal stream bridge. No payloads in argv, plaintext spool or logging.
use super::*;
use oojuro_capture::local::Stream;
use std::io::Read;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Position {
    source_id: String,
    stream: Stream,
    event_id: String,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Append {
    source_id: String,
    stream: Stream,
    cursor: u64,
    frame: String,
}
fn input<T: serde::de::DeserializeOwned>() -> Result<T> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take((8 << 20) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Io)?;
    if bytes.len() > 8 << 20 {
        return Err(Error::Bounds);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::Usage)
}
pub(super) fn run(root: &Path, args: &Arguments) -> Result<i32> {
    let append = args.command == "temporal-append";
    args.only(&[], if append { &["--allow-raw-source"] } else { &[] })?;
    if append && !args.flags.contains("--allow-raw-source") {
        return Err(Error::Consent);
    }
    let (config, key) = config::load(root)?;
    if !matches!(config.harness, Harness::Temporal) {
        return Err(Error::Usage);
    }
    let capture = open(
        root,
        &config,
        &key,
        Box::new(policy::ReadOnly),
        "native-read-only-1",
    )?;
    if !append {
        let v: Position = input()?;
        let position = capture
            .temporal_position(source(&v.source_id)?, &v.stream, &v.event_id)
            .map_err(|_| Error::Authority)?;
        emit(&position, 4096)?;
        return Ok(0);
    }
    let v: Append = input()?;
    let generation = source(&v.source_id)?;
    // An exact original ACK remains available even if the current policy is unreadable/stopped.
    if let Some(receipt) = capture
        .temporal_receipt(generation, &v.stream, v.cursor, v.frame.as_bytes(), true)
        .map_err(|_| Error::Authority)?
    {
        emit(&receipt, 4096)?;
        return Ok(0);
    }
    let frozen =
        policy::FrozenPolicy::parse(&config::read_private(&root.join(config::POLICY), 65536)?)?;
    if !frozen.enabled() {
        return Err(Error::Stopped);
    }
    drop(capture);
    let id = frozen.id.clone();
    let mut capture = open(root, &config, &key, Box::new(frozen), &id)?;
    match capture.append_temporal(generation, &v.stream, v.cursor, v.frame.as_bytes(), true) {
        Ok(receipt) => {
            emit(&receipt, 4096)?;
            Ok(0)
        }
        Err(oojuro_capture::local::Error::CommittedUnavailable(_)) => Ok(3),
        Err(oojuro_capture::local::Error::Invalid(reason)) => Err(Error::Capture(reason)),
        Err(_) => Err(Error::Authority),
    }
}
