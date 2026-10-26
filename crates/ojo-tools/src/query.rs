//! A managed local search cache. Source checkpoints and indexes are published
//! together; a failed refresh never substitutes an older snapshot's results.
use super::search::{binding, index_error, settings};
use super::{Arguments, Result, checkpoint, hex, private_bytes, private_new, write_new};
use ojo::{
    Checkpoint,
    events::{Cursor, Journal, Replay},
};
use oojuro_engine::{EventAppender, EventBuilder, EventIndex, EventRow};
use serde_json::json;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

const FORMAT: &[u8] = b"JURO-QUERY-CACHE-1\n";

struct Cache {
    root: PathBuf,
    lock: File,
}
impl Drop for Cache {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}
fn directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}
fn valid_generation(name: &str) -> bool {
    name.len() == 34
        && name.starts_with("g-")
        && name[2..]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
impl Cache {
    fn open(root: PathBuf) -> Result<Self> {
        match directory(&root) {
            Ok(()) => write_new(&root.join("FORMAT"), FORMAT)?,
            Err(error) if root.exists() => {
                let metadata = fs::symlink_metadata(&root)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err("cache must be a private regular directory".into());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o077 != 0 {
                        return Err("cache directory must be private (use chmod 700)".into());
                    }
                }
                if private_bytes(&root.join("FORMAT"), FORMAT.len())? != FORMAT {
                    return Err("unrecognized cache directory; choose a new --cache path".into());
                }
                drop(error);
            }
            Err(error) => return Err(error),
        }
        let path = root.join("LOCK");
        let lock = match private_new(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                private_bytes(&path, 0)?;
                let mut options = OpenOptions::new();
                options.read(true).write(true);
                #[cfg(target_os = "linux")]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(0x20000); // Linux O_NOFOLLOW; Linux is the supported platform.
                }
                options.open(path)?
            }
            Err(error) => return Err(error.into()),
        };
        lock.try_lock()
            .map_err(|_| "cache is in use; retry after the other query finishes")?;
        Ok(Self { root, lock })
    }
    fn current(&self) -> Result<Option<String>> {
        let path = self.root.join("CURRENT");
        if !path.try_exists()? && !path.is_symlink() {
            return Ok(None);
        }
        let name = String::from_utf8(private_bytes(&path, 34)?)?;
        if !valid_generation(&name) {
            return Err("invalid cache generation; choose a new --cache directory".into());
        }
        let metadata = fs::symlink_metadata(self.root.join(&name))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("invalid cache generation directory".into());
        }
        Ok(Some(name))
    }
    fn publish(&self, generation: &str) -> Result<()> {
        let temporary = self
            .root
            .join(format!(".current-{}", hex(&ojo::request_id()?)));
        write_new(&temporary, generation.as_bytes())?;
        fs::rename(&temporary, self.root.join("CURRENT"))?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    fn prune(&self, current: &str) -> Result<()> {
        // Only this command owns these private generation directories. Source
        // files and keys are explicitly excluded from the managed cache below.
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name != current && valid_generation(name) {
                let metadata = fs::symlink_metadata(entry.path())?;
                if metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && private_bytes(&entry.path().join("OWNER"), FORMAT.len())
                        .is_ok_and(|v| v == FORMAT)
                {
                    fs::remove_dir_all(entry.path())?;
                }
            }
        }
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
}
fn encode(value: Checkpoint) -> Vec<u8> {
    [
        value.file_id.as_slice(),
        &value.sequence.to_be_bytes(),
        &value.digest,
    ]
    .concat()
}
fn decode(path: &Path) -> Result<Checkpoint> {
    let bytes = private_bytes(path, 56)?;
    let checkpoint = Checkpoint {
        file_id: bytes[..16].try_into().unwrap(),
        sequence: u64::from_be_bytes(bytes[16..24].try_into().unwrap()),
        digest: bytes[24..].try_into().unwrap(),
    };
    if checkpoint.sequence == 0 {
        return Err("cache lacks an Event-profile checkpoint".into());
    }
    Ok(checkpoint)
}

pub(super) fn run(args: &Arguments, source_key: Option<&[u8; 32]>) -> Result<()> {
    let options = settings(args, 1, true)?;
    let source = Path::new(&args.positional[0]);
    let root = args.cache.clone().unwrap_or_else(|| {
        let mut name = source.as_os_str().to_os_string();
        name.push(".cache");
        name.into()
    });
    let cache = Cache::open(root)?;
    let canonical = fs::canonicalize(&cache.root)?;
    for path in [
        Some(source),
        args.key_file.as_deref(),
        args.index_key.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if fs::canonicalize(path)?.starts_with(&canonical) {
            return Err("source files and keys must be outside the managed cache directory".into());
        }
    }
    let previous_name = cache.current()?;
    let previous_checkpoint = previous_name
        .as_ref()
        .map(|name| decode(&cache.root.join(name).join("checkpoint")))
        .transpose()?;
    let mut seen = 0u64;
    let mut after = None;
    let mut journal = Journal::open_observed(source, source_key, false, |item| {
        if let Replay::Event { receipt, .. } = item {
            seen += 1;
            if previous_checkpoint.is_some_and(|cp| cp.sequence - 1 == seen) {
                after = Some(receipt.cursor);
            }
        }
        Ok(())
    })?;
    if journal.recovered_tail_bytes() != 0 {
        return Err("incomplete trailing data; inspect then recover before querying".into());
    }
    let expected = journal.checkpoint();
    let current_binding = binding(expected);
    let previous = if let (Some(name), Some(cp)) = (&previous_name, previous_checkpoint) {
        journal.require_checkpoint(&cp).map_err(
            |_| "cache source prefix changed or was replaced; choose a new --cache directory",
        )?;
        Some(
            EventIndex::open_with_limits(
                cache.root.join(name).join("index"),
                &options.key,
                &binding(cp),
                options.limits,
            )
            .map_err(index_error)?,
        )
    } else {
        None
    };
    let mut limits = options.limits;
    if let Some(previous) = &previous {
        limits.original_bytes_per_query = limits
            .original_bytes_per_query
            .min(previous.limits().original_bytes_per_query);
    }
    let mut added = 0usize;
    let mut opaque = 0usize;
    let (index, generation, state) = if previous_checkpoint == Some(expected) {
        (previous.unwrap(), previous_name.unwrap(), "reused")
    } else {
        let generation = format!("g-{}", hex(&ojo::request_id()?));
        let directory_path = cache.root.join(&generation);
        directory(&directory_path)?;
        write_new(&directory_path.join("OWNER"), FORMAT)?;
        write_new(&directory_path.join("checkpoint"), &encode(expected))?;
        let destination = directory_path.join("index");
        enum Build {
            New(Box<EventBuilder>),
            Extend(Box<EventAppender>),
        }
        let mut builder = if let Some(previous) = &previous {
            Build::Extend(Box::new(
                EventAppender::extend_validated_prefix_to(
                    previous,
                    &destination,
                    current_binding.clone(),
                    limits,
                )
                .map_err(index_error)?,
            ))
        } else {
            Build::New(Box::new(
                EventBuilder::create(&destination, &options.key, current_binding.clone(), limits)
                    .map_err(index_error)?,
            ))
        };
        loop {
            let mut failure = None;
            let page = journal.scan(after, 1024, |event| {
                let result = match &mut builder {
                    Build::New(b) => {
                        b.push(&event.cursor.to_bytes(), event.accepted_at, &event.data)
                    }
                    Build::Extend(b) => {
                        b.push(&event.cursor.to_bytes(), event.accepted_at, &event.data)
                    }
                };
                match result {
                    Ok(true) => added += 1,
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
        let index = match builder {
            Build::New(b) => b.finish(),
            Build::Extend(b) => b.finish(),
        }
        .map_err(index_error)?;
        File::open(&directory_path)?.sync_all()?;
        cache.publish(&generation)?;
        let state = if previous.is_some() {
            "refreshed"
        } else {
            "built"
        };
        drop(previous);
        (index, generation, state)
    };
    cache.prune(&generation)?;
    let hits = index
        .search(&current_binding, &options.query, |bytes| {
            let cursor = Cursor::from_bytes(
                bytes
                    .try_into()
                    .map_err(|_| oojuro_engine::record::Error::Authority)?,
            )
            .map_err(|_| oojuro_engine::record::Error::Authority)?;
            let event = journal
                .get(cursor)
                .map_err(|_| oojuro_engine::record::Error::Authority)?
                .ok_or(oojuro_engine::record::Error::Authority)?;
            Ok(EventRow {
                timestamp: event.accepted_at,
                data: event.data,
            })
        })
        .map_err(index_error)?;
    journal.require_checkpoint(&expected)?;
    let hits = hits.into_iter().map(|hit| json!({"cursor":hex(&hit.cursor),"accepted_at":hit.timestamp,
        "bits":hit.bits,"continued":hit.continued,"start":hit.start,"end":hit.end,"excerpt":hit.excerpt})).collect::<Vec<_>>();
    let response = json!({"source_checkpoint":checkpoint(expected),"hits":hits,
        "cache":{"path":cache.root,"state":state,"indexed_events":index.row_count(),
        "added_text_events":added,"added_binary_events":opaque}});
    drop(index);
    drop(journal);
    drop(cache); // Slow stdout must not exclude a writer or another local query.
    println!("{response}");
    Ok(())
}
