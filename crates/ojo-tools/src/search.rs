//! Explicit composition of verified Event files and disposable encrypted indexes.
use super::{Arguments, Result, checkpoint, hex, private_bytes};
use ojo::{
    Checkpoint,
    events::{Cursor, Journal},
};
use oojuro_engine::record::{
    Error as IndexError,
    projection::{
        Binding,
        disk::{Compression, Limits},
    },
};
use oojuro_engine::{EventBuilder, EventIndex, EventQuery, EventRow};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{ffi::OsStr, fs, path::Path};

fn number<T: std::str::FromStr>(value: &OsStr) -> Result<T> {
    value
        .to_str()
        .ok_or("numeric argument must be UTF-8")?
        .parse()
        .map_err(|_| "invalid numeric argument".into())
}
fn index_error(error: IndexError) -> Box<dyn std::error::Error> {
    format!("index unavailable or outside its source/budget: {error:?}; rebuild a stale index in a new directory").into()
}
fn binding(checkpoint: Checkpoint) -> Binding {
    let mut namespace = Sha256::new();
    namespace.update(b"oojuro/event-file-index/namespace/v1");
    namespace.update(checkpoint.file_id);
    let mut view = Sha256::new();
    view.update(b"oojuro/event-file-index/checkpoint/v1");
    view.update(checkpoint.file_id);
    view.update(checkpoint.sequence.to_be_bytes());
    view.update(checkpoint.digest);
    Binding {
        namespace: namespace.finalize().into(),
        writer: "event-profile-1".into(),
        view: view.finalize().into(),
    }
}

pub(super) fn run(command: &str, args: &Arguments, source_key: Option<&[u8; 32]>) -> Result<()> {
    let index_key: [u8; 32] = private_bytes(
        args.index_key
            .as_deref()
            .ok_or("--index-key is required; use keygen to create a separate private key")?,
        32,
    )?
    .try_into()
    .unwrap();
    let mut limits = Limits {
        index_bytes: args
            .index_bytes
            .as_deref()
            .map(number::<u64>)
            .transpose()?
            .unwrap_or(8 << 30),
        original_bytes_per_query: args
            .original_bytes
            .as_deref()
            .map(number::<usize>)
            .transpose()?
            .unwrap_or(64 << 20),
        ..Limits::default()
    };
    if limits.index_bytes == 0 || limits.original_bytes_per_query == 0 {
        return Err("index and original byte budgets must be positive".into());
    }
    let compression = args
        .compression
        .as_deref()
        .map(|value| value.to_str().ok_or("compression mode must be UTF-8"))
        .transpose()?
        .unwrap_or("raw");
    limits.compression = match compression {
        "raw" => Compression::Raw,
        "zstd" => Compression::Zstd,
        _ => return Err("--compression must be raw or zstd".into()),
    };
    let query_text = args
        .positional
        .get(2)
        .map(|text| text.to_str().ok_or("query must be UTF-8"))
        .transpose()?;
    let mut query = EventQuery::new(query_text.unwrap_or(""));
    query.limit = args
        .limit
        .as_deref()
        .map(number::<usize>)
        .transpose()?
        .unwrap_or(20);
    query.from = args
        .from
        .as_deref()
        .map(number::<i64>)
        .transpose()?
        .unwrap_or(i64::MIN);
    query.to = args.to.as_deref().map(number::<i64>).transpose()?;
    if !(1..=1024).contains(&query.limit) || query.to.is_some_and(|to| to <= query.from) {
        return Err("search limit must be 1..=1024 and timestamp range must be [from,to)".into());
    }
    if command == "search" && (query.text.trim().is_empty() || query.text.len() > 4096) {
        return Err("search requires a nonempty query of at most4096 UTF-8 bytes".into());
    }
    let source = Path::new(&args.positional[0]);
    let directory = Path::new(&args.positional[1]);
    let mut journal = Journal::open(source, source_key, false)?;
    if journal.recovered_tail_bytes() != 0 {
        return Err("incomplete trailing data; inspect then recover before indexing/search".into());
    }
    let expected = journal.checkpoint();
    let current = binding(expected);
    if command == "index" {
        // Claim only a new directory. Existing paths and caches are never replaced
        // or recursively cleaned after an uncertain filesystem publication error.
        let mut create = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            create.mode(0o700);
        }
        create.create(directory)?;
        let mut builder =
            EventBuilder::create(directory, &index_key, current, limits).map_err(index_error)?;
        let mut after = None;
        let mut indexed = 0usize;
        let mut opaque = 0usize;
        loop {
            let mut failure = None;
            let page = journal.scan(after, 1024, |event| {
                match builder.push(&event.cursor.to_bytes(), event.accepted_at, &event.data) {
                    Ok(true) => indexed += 1,
                    Ok(false) => opaque += 1,
                    Err(error) => {
                        failure = Some(error);
                        return Err(ojo::Error::Limit);
                    }
                }
                Ok(())
            });
            if let Some(error) = failure {
                return Err(index_error(error));
            }
            let page = page?;
            after = page.cursor;
            if !page.has_more {
                break;
            }
        }
        journal.require_checkpoint(&expected)?;
        let index = builder.finish().map_err(index_error)?;
        journal.require_checkpoint(&expected)?;
        let response = json!({"indexed":indexed,"opaque_binary_events":opaque,"source_checkpoint":checkpoint(expected),"index":directory,"compression":limits.compression,"index_bytes":index.index_bytes(),"allocated_index_bytes":index.allocated_index_bytes(),"estimated_heap_bytes":index.estimated_heap_bytes(),"build_metrics":index.build_metrics()});
        drop(index);
        drop(journal);
        println!("{response}");
    } else {
        let index = EventIndex::open_with_limits(directory, &index_key, &current, limits)
            .map_err(index_error)?;
        let hits = index
            .search(&current, &query, |bytes| {
                let cursor =
                    Cursor::from_bytes(bytes.try_into().map_err(|_| IndexError::Authority)?)
                        .map_err(|_| IndexError::Authority)?;
                let event = journal
                    .get(cursor)
                    .map_err(|_| IndexError::Authority)?
                    .ok_or(IndexError::Authority)?;
                Ok(EventRow {
                    timestamp: event.accepted_at,
                    data: event.data,
                })
            })
            .map_err(index_error)?;
        journal.require_checkpoint(&expected)?;
        let hits = hits.into_iter().map(|hit| json!({"cursor":hex(&hit.cursor),"accepted_at":hit.timestamp,"bits":hit.bits,"continued":hit.continued,"start":hit.start,"end":hit.end,"excerpt":hit.excerpt})).collect::<Vec<_>>();
        let response = json!({"source_checkpoint":checkpoint(expected),"hits":hits});
        drop(index);
        drop(journal);
        println!("{response}");
    }
    Ok(())
}
