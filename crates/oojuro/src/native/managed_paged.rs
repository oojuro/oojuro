//! Explicit fresh paged operator namespace; existing snapshot commands stay separate.
use super::{Arguments, Error, Result, api, capture_record, emit_bytes, managed};
use std::path::Path;
pub(super) fn run(root: &Path, args: &Arguments) -> Result<i32> {
    if !Path::new(args.required("--root")?).is_absolute() {
        return Err(Error::Config);
    }
    match args.command.as_str() {
        "managed-paged-ingest-capture" => capture_record::run_paged(root, args),
        "managed-paged-setup" => {
            args.only(
                &[
                    "--data-root",
                    "--control-root",
                    "--witness-root",
                    "--writer",
                    "--owner",
                    "--first-id",
                    "--adapter",
                ],
                &[],
            )?;
            managed::setup_paged(root, args)?;
            Ok(0)
        }
        "managed-paged-get" => {
            args.only(&["--id", "--bytes"], &[])?;
            let limit = args.number("--bytes", 1usize << 20)?;
            if !(4096..=oojuro_engine::record::MAX_OUTPUT_BYTES).contains(&limit) {
                return Err(Error::Bounds);
            }
            let id = args.required("--id")?.parse().map_err(|_| Error::Usage)?;
            managed::with_paged(root, |log, scope| {
                let before = log.scoped_binding(scope).map_err(Error::Record)?;
                let row = log.get(id, limit - 1).map_err(Error::Record)?;
                let bytes = api::encode(&row, limit - 1).map_err(Error::Ingest)?;
                log.validate_binding(&before).map_err(Error::Record)?;
                emit_bytes(bytes, limit)
            })?;
            Ok(0)
        }
        _ => Err(Error::Usage),
    }
}
