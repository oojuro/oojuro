//! Explicit bounded local parsed-copy composition. No default capture, cloud or Message lane.
use super::{
    Arguments, Error, Result,
    api::{self, Namespace, Owner},
    config, emit_bytes, managed,
};
use oojuro_engine::{
    capture_record as bridge,
    record::managed::{Binding, ManagedPagedLog},
};
use serde::Serialize;
use std::path::Path;
const OUTPUT_BYTES: usize = 1 << 20;
#[derive(Serialize)]
struct Output {
    schema: u32,
    report: bridge::Report,
    target: Binding,
}
fn bridge_error(error: bridge::Error) -> Error {
    match error {
        bridge::Error::Bounds => Error::Bounds,
        bridge::Error::Scope => Error::Ingest("scope"),
        bridge::Error::Source => Error::Ingest("source"),
        bridge::Error::Authority => Error::Ingest("authority"),
        bridge::Error::Checkpoint => Error::Ingest("checkpoint"),
        bridge::Error::Target(error) => Error::Record(error),
    }
}
fn inputs(args: &Arguments) -> Result<(&Path, &Path, bridge::Limits)> {
    args.only(
        &[
            "--capture-root",
            "--checkpoint",
            "--receipts",
            "--input-bytes",
        ],
        &["--allow-capture-ingest"],
    )?;
    // Explicit consent precedes all file/key/config inspection.
    if !args.flags.contains("--allow-capture-ingest") {
        return Err(Error::Ingest("consent_required"));
    }
    let capture_root = Path::new(args.required("--capture-root")?);
    let checkpoint = Path::new(args.required("--checkpoint")?);
    if !Path::new(args.required("--root")?).is_absolute()
        || !capture_root.is_absolute()
        || !checkpoint.is_absolute()
    {
        return Err(Error::Config);
    }
    let limits = bridge::Limits {
        receipts: args.number("--receipts", 128usize)?,
        bytes: args.number("--input-bytes", 32usize << 20)?,
    };
    if limits.receipts == 0
        || limits.receipts > 1000
        || limits.bytes == 0
        || limits.bytes > 32 << 20
    {
        return Err(Error::Bounds);
    }
    config::check_private_path(capture_root)?;
    Ok((capture_root, checkpoint, limits))
}
pub(super) fn run(root: &Path, args: &Arguments) -> Result<i32> {
    let (capture_root, checkpoint, limits) = inputs(args)?;
    let output =
        managed::with_existing_for_ingest(root, capture_root, checkpoint, |destination, scope| {
            // Reuse the protected existing-only loader; LocalCapture also validates its existing
            // adapter context and builds its bounded read index. No second authority loader.
            let Owner::Capture { capture, .. } = Owner::open(capture_root, Namespace::Capture)?
            else {
                return Err(Error::Authority);
            };
            let report = bridge::ingest(
                capture.authority().map_err(|_| Error::Authority)?,
                destination,
                scope,
                checkpoint,
                limits,
            )
            .map_err(bridge_error)?;
            Ok(Output {
                schema: 1,
                report,
                target: destination.binding().map_err(Error::Record)?,
            })
        })?;
    emit_report(output)
}
fn emit_report(output: Output) -> Result<i32> {
    // Reuse the bounded writer directly over typed data, before allocating encoded bytes.
    // A thousand numeric-only mappings plus binding fit within the fixed one-MiB limit.
    let bytes = api::encode(&output, OUTPUT_BYTES - 1).map_err(Error::Ingest)?;
    emit_bytes(bytes, OUTPUT_BYTES)?;
    Ok(0)
}

pub(super) fn run_paged(root: &Path, args: &Arguments) -> Result<i32> {
    let (capture_root, checkpoint, limits) = inputs(args)?;
    let output = managed::with_paged_for_ingest(
        root,
        capture_root,
        checkpoint,
        |destination: &mut ManagedPagedLog, scope| {
            let Owner::Capture { capture, .. } = Owner::open(capture_root, Namespace::Capture)?
            else {
                return Err(Error::Authority);
            };
            let report = bridge::ingest_paged(
                capture.authority().map_err(|_| Error::Authority)?,
                destination,
                scope,
                checkpoint,
                limits,
            )
            .map_err(bridge_error)?;
            Ok(Output {
                schema: 1,
                report,
                target: destination.scoped_binding(scope).map_err(Error::Record)?,
            })
        },
    )?;
    emit_report(output)
}
