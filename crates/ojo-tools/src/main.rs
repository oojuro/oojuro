//! Generic file tools; agent/session policy belongs to ojo-journal.
use ojo::{Change, Checkpoint, Database};
use serde_json::{Value, json};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const HELP: &str = "ojo — portable .ojo file tools (experimental wire 0.1)

  ojo create FILE [--key-file PRIVATE_KEY]
  ojo keygen PRIVATE_KEY
  ojo request-id
  ojo inspect|verify|recover FILE [--key-file PRIVATE_KEY]
  ojo get FILE KEY_HEX [--output NEW_FILE] [--key-file PRIVATE_KEY]
  ojo put FILE KEY_HEX VALUE_FILE --request ID_HEX [--key-file PRIVATE_KEY]
  ojo delete FILE KEY_HEX --request ID_HEX [--key-file PRIVATE_KEY]
  ojo dump FILE [--key-file PRIVATE_KEY]
  ojo export FILE NEW_COPY.ojo [--key-file PRIVATE_KEY]
  ojo archive FILE NEW_ARCHIVE.ojo [--key-file PRIVATE_KEY]

Keys are hexadecimal bytes; request IDs are 16 bytes (32 hex digits).
VALUE_FILE may be - for stdin. get writes exact raw bytes; dump emits hex JSONL
for every historical operation, including deletions. export makes a validated,
durable byte-for-byte database copy, retaining identity, history and retry IDs.
Key files contain exactly 32 raw bytes and must be private on Unix.
recover and writable opens truncate only verified incomplete trailing data;
complete corruption is refused. Archive support is optional at build time.
No operation overwrites an existing destination file. Keep encryption keys
separately from databases.
";

#[derive(Default)]
struct Arguments {
    positional: Vec<OsString>,
    key_file: Option<PathBuf>,
    request: Option<OsString>,
    output: Option<PathBuf>,
}

fn arguments(values: impl Iterator<Item = OsString>) -> Result<Arguments> {
    let mut result = Arguments::default();
    let mut values = values.peekable();
    let mut positional_only = false;
    while let Some(value) = values.next() {
        if !positional_only && value == "--" {
            positional_only = true;
        } else if !positional_only && value == "--key-file" {
            if result.key_file.is_some() {
                return Err("duplicate --key-file".into());
            }
            result.key_file = Some(values.next().ok_or("missing key file")?.into());
        } else if !positional_only && value == "--request" {
            if result.request.is_some() {
                return Err("duplicate --request".into());
            }
            result.request = Some(values.next().ok_or("missing request ID")?);
        } else if !positional_only && value == "--output" {
            if result.output.is_some() {
                return Err("duplicate --output".into());
            }
            result.output = Some(values.next().ok_or("missing output file")?.into());
        } else if !positional_only && value.to_str().is_some_and(|s| s.starts_with("--")) {
            return Err("unknown option; see ojo --help".into());
        } else {
            result.positional.push(value);
        }
    }
    Ok(result)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 15) as usize] as char);
    }
    result
}

fn unhex(value: &OsStr, max: usize) -> Result<Vec<u8>> {
    let value = value.to_str().ok_or("hex input must be UTF-8")?;
    if value.len() % 2 != 0 || value.len() / 2 > max || !value.is_ascii() {
        return Err("invalid hex length".into());
    }
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).map_err(|_| "invalid hex byte".into()))
        .collect()
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn private_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = private_new(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    File::open(parent(path))?.sync_all()?;
    Ok(())
}

fn key(path: Option<&Path>) -> Result<Option<[u8; 32]>> {
    let Some(path) = path else { return Ok(None) };
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() {
        return Err("key file must be a regular file".into());
    }
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("key file must not grant group or other access (use chmod 600)".into());
        }
        if before.dev() != metadata.dev() || before.ino() != metadata.ino() {
            return Err("key file changed while opening".into());
        }
    }
    if !metadata.is_file() || metadata.len() != 32 {
        return Err("key file must contain exactly 32 raw bytes".into());
    }
    let mut bytes = [0; 32];
    file.read_exact(&mut bytes)?;
    let mut extra = [0; 1];
    if file.read(&mut extra)? != 0 {
        return Err("key file changed while reading".into());
    }
    Ok(Some(bytes))
}

fn checkpoint(value: Checkpoint) -> Value {
    json!({"file_id":hex(&value.file_id),"sequence":value.sequence,"digest":hex(&value.digest)})
}

fn checked_copy(
    source: &Path,
    destination: &Path,
    key: Option<&[u8; 32]>,
    expected: Checkpoint,
) -> Result<()> {
    let temporary =
        parent(destination).join(format!(".ojo-export-{}.tmp", hex(&ojo::request_id()?)));
    let mut output = private_new(&temporary)?;
    let result = (|| -> Result<()> {
        io::copy(&mut File::open(source)?, &mut output)?;
        output.sync_all()?;
        let copy = Database::open(&temporary, key, false)?;
        if copy.checkpoint() != expected {
            return Err("export source changed while copying".into());
        }
        if copy.recovered_tail_bytes() != 0 {
            return Err("export source contains incomplete trailing data".into());
        }
        drop(copy);
        fs::hard_link(&temporary, destination)?;
        File::open(parent(destination))?.sync_all()?;
        Ok(())
    })();
    drop(output);
    let cleanup =
        fs::remove_file(&temporary).and_then(|()| File::open(parent(destination))?.sync_all());
    result?;
    cleanup?;
    Ok(())
}

fn run() -> Result<()> {
    let mut values = std::env::args_os().skip(1);
    let Some(command) = values.next() else {
        print!("{HELP}");
        return Ok(());
    };
    if command == "--help" || command == "help" {
        print!("{HELP}");
        return Ok(());
    }
    if command == "--version" {
        println!("ojo {} (wire 0.1)", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let args = arguments(values)?;
    let command = command.to_str().ok_or("command must be UTF-8")?;
    if args.request.is_some() && !matches!(command, "put" | "delete") {
        return Err("--request is only valid for put/delete".into());
    }
    if args.output.is_some() && command != "get" {
        return Err("--output is only valid for get".into());
    }
    let expected = match command {
        "request-id" => 0,
        "create" | "keygen" | "inspect" | "verify" | "recover" | "dump" => 1,
        "get" | "delete" | "export" | "archive" => 2,
        "put" => 3,
        _ => return Err("unknown command; see ojo --help".into()),
    };
    if args.positional.len() != expected {
        return Err("wrong number of arguments; see ojo --help".into());
    }
    if matches!(command, "request-id" | "keygen") && args.key_file.is_some() {
        return Err("--key-file is not valid for this command".into());
    }
    if command == "request-id" {
        println!("{}", hex(&ojo::request_id()?));
        return Ok(());
    }
    let path = Path::new(&args.positional[0]);
    if command == "keygen" {
        write_new(path, &ojo::generate_key()?)?;
        println!("{}", json!({"created":path,"bytes":32}));
        return Ok(());
    }
    let key = key(args.key_file.as_deref())?;
    if command == "create" {
        let db = Database::create(path, key.as_ref())?;
        println!(
            "{}",
            json!({"created":path,"checkpoint":checkpoint(db.checkpoint())})
        );
        return Ok(());
    }
    // Validate complete mutation input before a writable open can recover a tail.
    let mutation = if matches!(command, "put" | "delete") {
        let request: [u8; 16] = unhex(
            args.request
                .as_deref()
                .ok_or("--request is required; retain the same ID for retries")?,
            16,
        )?
        .try_into()
        .map_err(|_| "request ID must contain exactly 16 bytes")?;
        if request == [0; 16] {
            return Err("request ID must be nonzero".into());
        }
        let selected = unhex(&args.positional[1], 65536)?;
        let value = if command == "put" {
            let mut reader: Box<dyn Read> = if args.positional[2] == "-" {
                Box::new(io::stdin().lock())
            } else {
                Box::new(File::open(&args.positional[2])?)
            };
            let mut bytes = Vec::new();
            reader
                .by_ref()
                .take((16 << 20) + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 16 << 20 {
                return Err("value exceeds the 16 MiB wire limit".into());
            }
            Some(bytes)
        } else {
            None
        };
        Some((request, selected, value))
    } else {
        None
    };
    let writable = matches!(command, "put" | "delete" | "recover");
    let mut db = Database::open(path, key.as_ref(), writable)?;
    if db.recovered_tail_bytes() != 0
        && !matches!(command, "recover" | "inspect" | "put" | "delete")
    {
        return Err(
            "incomplete trailing data; inspect then use recover before this operation".into(),
        );
    }
    match command {
        "inspect" | "verify" | "recover" => {
            let mut operations = 0u64;
            let mut value_bytes = 0u64;
            db.scan(|_, _, _, value| {
                operations += 1;
                value_bytes += value.map_or(0, |v| v.len()) as u64;
            })?;
            println!(
                "{}",
                json!({"wire":"0.1","verified":true,"checkpoint":checkpoint(db.checkpoint()),"historical_operations":operations,"historical_value_bytes":value_bytes,"file_bytes":fs::metadata(path)?.len(),"incomplete_tail_bytes":db.recovered_tail_bytes()})
            );
        }
        "get" => {
            let value = db
                .get(&unhex(&args.positional[1], 65536)?)?
                .ok_or("key not found")?;
            if let Some(output) = args.output {
                write_new(&output, &value)?;
            } else {
                let mut output = io::stdout().lock();
                output.write_all(&value)?;
                output.flush()?;
            }
        }
        "put" | "delete" => {
            let (request, selected, value) = mutation.unwrap();
            let commit = db.commit(
                request,
                &[Change {
                    key: &selected,
                    value: value.as_deref(),
                }],
            )?;
            println!(
                "{}",
                json!({"request_id":hex(&request),"checkpoint":checkpoint(commit.checkpoint),"operations":commit.operations,"recovered_tail_bytes":db.recovered_tail_bytes()})
            );
        }
        "dump" => {
            let mut output = io::stdout().lock();
            let mut error = None;
            db.scan(|sequence,operation,key,value| {
                if error.is_none() {
                    let row = json!({"sequence":sequence,"operation":operation,"key_hex":hex(key),"value_hex":value.map(hex)});
                    if let Err(e) = writeln!(output,"{row}") {error=Some(e)}
                }
            })?;
            if let Some(error) = error {
                return Err(error.into());
            }
        }
        "export" => {
            let destination = Path::new(&args.positional[1]);
            checked_copy(path, destination, key.as_ref(), db.checkpoint())?;
            println!(
                "{}",
                json!({"exported":destination,"checkpoint":checkpoint(db.checkpoint()),"encryption_key_included":false})
            );
        }
        "archive" => {
            #[cfg(feature = "archive")]
            {
                let destination = Path::new(&args.positional[1]);
                let archived = db.archive_to(destination)?;
                println!(
                    "{}",
                    json!({"archived":destination,"checkpoint":checkpoint(archived),"source_checkpoint":checkpoint(db.checkpoint())})
                );
            }
            #[cfg(not(feature = "archive"))]
            return Err("archive support was disabled when building this tool".into());
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("ojo: {error}");
        std::process::exit(1);
    }
}
