//! Reopen a native store: the loader verifies every source frame and original digest.
fn main() -> oojuro_core::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("provide a native Nine directory")?;
    let db = oojuro_nine::Nine::open(std::path::Path::new(&path))?;
    let stats = db.stats()?;
    if let Some(expected) = args.next() {
        if stats.records != expected.parse::<usize>()? {
            return Err("reopened source count differs from expected count".into());
        }
    }
    println!(
        "{}",
        serde_json::json!({"verified_reopen":true,"records":stats.records,"archive_bytes":stats.archive_bytes,"indexed_postings":stats.indexed_postings})
    );
    Ok(())
}
