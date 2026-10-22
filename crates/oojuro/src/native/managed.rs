//! Opt-in managed operator lane. Keys and the retained witness are never data-backup inputs.
use super::{Arguments, Error, Result, config, emit};
use oojuro_engine::{
    audit::{Domain, RetainedFloor, Signer, Verifier},
    capture_log::Identity,
    record::{
        self, Input,
        managed::{self, ControlAuditPage, FileWitness, ManagedLog, RetainedControl, Selection},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

const CONFIG: &str = "managed.json";
const CONFIG_BYTES: usize = 16 << 10;
const DATA_KEY: &str = "data-key";
const CONTROL_KEY: &str = "control-key";
const SIGNER: &str = "signing.pk8";
const MAX_JSON: usize = 1 << 20;
const AUDIT_PAGE_BYTES: usize = 16 << 20;
const VERIFY_BYTES: u64 = 256 << 20;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u32,
    data: PathBuf,
    control: PathBuf,
    witness: PathBuf,
    writer: String,
    owner: u32,
    first_id: u64,
    adapter: String,
    authority: [u8; 32],
    public_key: [u8; 32],
    identity: [u8; 32],
    origin: [u8; 32],
}
impl Config {
    fn source(&self) -> Identity {
        Identity {
            writer: self.writer.clone(),
            agent: self.owner,
            first_id: self.first_id,
            adapter: self.adapter.clone(),
        }
    }
    fn check(&self, root: &Path) -> Result<()> {
        let name =
            |s: &str, max| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control);
        if self.version != 1
            || !name(&self.writer, 256)
            || !name(&self.adapter, 128)
            || self.owner == 0
            || self.first_id == 0
            || self.authority == [0; 32]
            || self.public_key == [0; 32]
            || self.identity == [0; 32]
        {
            return Err(Error::Config);
        }
        separate(&[root, &self.data, &self.control, &self.witness])
    }
    fn verifier(&self) -> Verifier {
        Verifier::new(self.authority, Domain::NativeLifecycle, self.public_key)
    }
}

fn map<T>(result: record::Result<T>) -> Result<T> {
    result.map_err(Error::Record)
}

fn canonical_new(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Error::Config);
    }
    match fs::symlink_metadata(path) {
        Ok(_) => return Err(Error::Config),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(Error::Io),
    }
    let parent = path
        .parent()
        .ok_or(Error::Config)?
        .canonicalize()
        .map_err(|_| Error::Io)?;
    Ok(parent.join(path.file_name().ok_or(Error::Config)?))
}
fn separate(paths: &[&Path]) -> Result<()> {
    for (i, path) in paths.iter().enumerate() {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(Error::Config);
        }
        if paths[..i]
            .iter()
            .any(|other| path.starts_with(other) || other.starts_with(path))
        {
            return Err(Error::Config);
        }
    }
    Ok(())
}
fn random() -> Result<[u8; 32]> {
    let mut bytes = [0; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| Error::Key)?;
    Ok(bytes)
}
fn load(root: &Path) -> Result<(Config, [u8; 32])> {
    config::check_root(root)?;
    // A configured canonical path cannot be redirected through a replaced ancestor.
    if root.canonicalize().map_err(|_| Error::Io)? != root {
        return Err(Error::Permissions);
    }
    let cfg: Config =
        serde_json::from_slice(&config::read_private(&root.join(CONFIG), CONFIG_BYTES)?)
            .map_err(|_| Error::Config)?;
    cfg.check(root)?;
    for path in [&cfg.control, &cfg.witness] {
        config::check_root(path)?;
        if path.canonicalize().map_err(|_| Error::Io)? != *path {
            return Err(Error::Permissions);
        }
    }
    let key: [u8; 32] = config::read_private(&root.join(DATA_KEY), 32)?
        .try_into()
        .map_err(|_| Error::Key)?;
    if map(managed::identity_binding(&key, &cfg.source()))? != cfg.identity {
        return Err(Error::Authority);
    }
    Ok((cfg, key))
}
fn control(root: &Path, cfg: &Config) -> Result<Arc<RetainedControl>> {
    // Before constructors that can create fresh roots, require an independently retained current floor.
    // A missing/partial witness or control never becomes a new successful authority on a read.
    let floor: RetainedFloor =
        serde_json::from_slice(&config::read_private(&cfg.witness.join("CURRENT"), 4096)?)
            .map_err(|_| Error::Authority)?;
    if floor.sequence == 0 {
        return Err(Error::Authority);
    }
    let key = config::read_private(&root.join(CONTROL_KEY), 32)?
        .try_into()
        .map_err(|_| Error::Key)?;
    let signer = Signer::from_pkcs8(
        cfg.authority,
        Domain::NativeLifecycle,
        &config::read_private(&root.join(SIGNER), 4096)?,
    )
    .map_err(|_| Error::Key)?;
    if signer.public_key() != cfg.public_key {
        return Err(Error::Authority);
    }
    let witness = map(FileWitness::open(&cfg.witness))?;
    let control = Arc::new(map(RetainedControl::open(
        &cfg.control,
        &key,
        signer,
        cfg.authority,
        witness,
        cfg.identity,
    ))?);
    // Original origin is pinned independently of the data directory and exported bundle.
    // status() also performs the current-witness check.
    let status = map(control.status())?;
    if status.origin != cfg.origin || status.identity != cfg.identity {
        return Err(Error::Authority);
    }
    Ok(control)
}
fn cap(args: &Arguments) -> Result<usize> {
    let n = args.number("--bytes", MAX_JSON)?;
    let max = if matches!(args.command.as_str(), "managed-export" | "managed-verify") {
        AUDIT_PAGE_BYTES
    } else {
        record::MAX_OUTPUT_BYTES
    };
    if !(4096..=max).contains(&n) {
        return Err(Error::Bounds);
    }
    Ok(n)
}
fn setup(root: &Path, args: &Arguments) -> Result<()> {
    let root = canonical_new(root)?;
    let data = canonical_new(Path::new(args.required("--data-root")?))?;
    let retained = canonical_new(Path::new(args.required("--control-root")?))?;
    let witness_path = canonical_new(Path::new(args.required("--witness-root")?))?;
    let mut cfg = Config {
        version: 1,
        data,
        control: retained,
        witness: witness_path,
        writer: args.required("--writer")?.into(),
        owner: args
            .required("--owner")?
            .parse()
            .map_err(|_| Error::Usage)?,
        first_id: args.number("--first-id", 1)?,
        adapter: args.required("--adapter")?.into(),
        authority: random()?,
        public_key: [1; 32],
        identity: [1; 32],
        origin: [0; 32],
    };
    cfg.check(&root)?;
    let data_key = random()?;
    let control_key = random()?;
    if data_key == control_key {
        return Err(Error::Key);
    }
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .map_err(|_| Error::Key)?;
    let signer = Signer::from_pkcs8(cfg.authority, Domain::NativeLifecycle, pkcs8.as_ref())
        .map_err(|_| Error::Key)?;
    cfg.public_key = signer.public_key();
    cfg.identity = map(managed::identity_binding(&data_key, &cfg.source()))?;
    // Bound the readable config before creating any root. The future random origin
    // cannot serialize longer than this all-three-digit placeholder.
    cfg.origin = [255; 32];
    if serde_json::to_vec(&cfg).map_err(|_| Error::Config)?.len() > CONFIG_BYTES {
        return Err(Error::Bounds);
    }
    // No cleanup/reset on failure: interrupted setup is explicit residue, never authority to reenroll.
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .map_err(|_| Error::Io)?;
    config::write_new(&root.join(DATA_KEY), &data_key)?;
    config::write_new(&root.join(CONTROL_KEY), &control_key)?;
    config::write_new(&root.join(SIGNER), pkcs8.as_ref())?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&cfg.witness)
        .map_err(|_| Error::Io)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&cfg.control)
        .map_err(|_| Error::Io)?;
    let witness = map(FileWitness::open(&cfg.witness))?;
    let control = Arc::new(map(RetainedControl::open(
        &cfg.control,
        &control_key,
        signer,
        cfg.authority,
        witness,
        cfg.identity,
    ))?);
    let log = map(ManagedLog::create(
        &cfg.data,
        &data_key,
        cfg.source(),
        control,
    ))?;
    let binding = map(log.binding())?;
    cfg.origin = binding.stable_origin;
    config::write_new(
        &root.join(CONFIG),
        &serde_json::to_vec(&cfg).map_err(|_| Error::Config)?,
    )?;
    for path in [&root, &cfg.data, &cfg.control, &cfg.witness] {
        config::sync_dir(path)?;
        config::sync_dir(path.parent().ok_or(Error::Config)?)?;
    }
    emit(
        &json!({"status":"initialized","namespace":"oojuro-managed-record-3",
        "permanent_erasure":"unsupported_caller_owned_keys"}),
        4096,
    )
}

pub(super) fn run(root: &Path, args: &Arguments) -> Result<i32> {
    // Consent precedes root/key/input inspection. Direct records are unredacted caller data.
    if args.command == "managed-append" && !args.flags.contains("--allow-unredacted-records") {
        return Err(Error::RecordConsent);
    }
    if !Path::new(args.required("--root")?).is_absolute() {
        return Err(Error::Config);
    }
    let options: &[&str] = match args.command.as_str() {
        "managed-setup" => &[
            "--data-root",
            "--control-root",
            "--witness-root",
            "--writer",
            "--owner",
            "--first-id",
            "--adapter",
        ],
        "managed-open" | "managed-status" | "managed-deny" => &["--bytes"],
        "managed-get" => &["--id", "--bytes"],
        "managed-append" => &["--input", "--request", "--bytes"],
        "managed-prune" => &["--input", "--operation", "--bytes"],
        "managed-export" => &["--start", "--entries", "--bytes", "--floor"],
        "managed-verify" => &["--input", "--bytes", "--max-input-bytes"],
        _ => return Err(Error::Usage),
    };
    args.only(
        options,
        if args.command == "managed-append" {
            &["--allow-unredacted-records"]
        } else {
            &[]
        },
    )?;
    let limit = cap(args)?;
    if args.command == "managed-setup" {
        setup(root, args)?;
        return Ok(0);
    }
    let (cfg, key) = load(root)?;
    let owner = control(root, &cfg)?;
    match args.command.as_str() {
        "managed-status" => {
            emit(
                &json!({"authority":map(owner.status())?,"permanent_erasure":"unsupported_caller_owned_keys"}),
                limit,
            )?;
        }
        "managed-export" => {
            let start = args.number("--start", 0u64)?;
            let current = match args.value("--floor") {
                Some(value) => serde_json::from_str(value).map_err(|_| Error::Config)?,
                None if start == 0 => map(owner.current_floor())?,
                None => return Err(Error::Usage),
            };
            let entries = args.number("--entries", 100)?;
            if !(1..=1000).contains(&entries) {
                return Err(Error::Bounds);
            }
            let page = map(owner.export_audit(
                current,
                start,
                record::Limits {
                    entries,
                    bytes: limit - 1,
                },
            ))?;
            // Independently check each signed head before returning a page. Full-chain verification
            // remains explicit: a middle page alone is not a complete audit history.
            verify_page(&cfg, &page, current, start, None)?;
            emit(&page, limit)?;
        }
        "managed-verify" => {
            verify_file(&cfg, &owner, args, limit)?;
        }
        "managed-deny" => {
            // Administrative logical denial remains retryable after data cleanup.
            let receipt = map(owner.deny())?;
            map(receipt.verify(&cfg.verifier(), cfg.identity))?;
            emit(
                &json!({"authority":map(owner.status())?,"receipt":receipt,
                "permanent_erasure":"unsupported_caller_owned_keys"}),
                limit,
            )?;
        }
        _ => {
            config::check_root(&cfg.data)?;
            if cfg.data.canonicalize().map_err(|_| Error::Io)? != cfg.data {
                return Err(Error::Permissions);
            }
            let mut log = map(ManagedLog::open(&cfg.data, &key, cfg.source(), owner))?;
            match args.command.as_str() {
                "managed-open" => emit(&json!({"binding":map(log.binding())?}), limit)?,
                "managed-get" => emit(
                    &map(log.get(
                        args.required("--id")?.parse().map_err(|_| Error::Usage)?,
                        limit - 1,
                    ))?,
                    limit,
                )?,
                "managed-append" => {
                    let encoded = config::read_private(
                        Path::new(args.required("--input")?),
                        record::MAX_BATCH_BYTES,
                    )?;
                    let inputs: Vec<Input> =
                        serde_json::from_slice(&encoded).map_err(|_| Error::Config)?;
                    let ack = map(log.append(args.required("--request")?, &inputs))?;
                    emit(
                        &json!({"commit":ack.commit,"receipt_only":ack.receipt_only,"removed_ids":ack.removed_ids}),
                        limit,
                    )?;
                }
                "managed-prune" => {
                    let encoded =
                        config::read_private(Path::new(args.required("--input")?), 64 << 10)?;
                    let selection: Selection =
                        serde_json::from_slice(&encoded).map_err(|_| Error::Config)?;
                    let report = map(log.prune(args.required("--operation")?, &selection))?;
                    map(report.signed_receipt.verify(&cfg.verifier(), cfg.identity))?;
                    map(report.signed_receipt.verify_record_ids(&report.removed_ids))?;
                    emit(
                        &json!({"removed_ids":report.removed_ids,"selected_bytes":report.selected_bytes,
                        "decoded_shared_occurrences":report.decoded_shared_occurrences,"encoded_occurrences":report.encoded_occurrences,
                        "all_owned_prior_generations_removed":report.all_owned_prior_generations_removed,
                        "local_live_absence_verified":report.local_live_absence_verified,"historical":report.historical,
                        "verified_generation":report.verified_generation,"receipt":report.signed_receipt,
                        "permanent_erasure":"unsupported_caller_owned_keys"}),
                        limit,
                    )?;
                }
                _ => return Err(Error::Usage),
            }
        }
    }
    Ok(0)
}

/// Verify an entry page without adopting the page's public key or current-floor assertion.
fn verify_page(
    cfg: &Config,
    page: &ControlAuditPage,
    current: RetainedFloor,
    start: u64,
    previous: Option<[u8; 32]>,
) -> Result<[u8; 32]> {
    if page.schema != 1
        || page.identity != cfg.identity
        || page.current != current
        || page.start != start
        || page.entries.is_empty()
        || page.entries.len() > 1000
    {
        return Err(Error::Receipt);
    }
    let mut previous = previous;
    let mut ordinal = start;
    for entry in &page.entries {
        map(entry.verify(&cfg.verifier(), cfg.identity))?;
        let cp = &entry.checkpoint;
        if cp.body.sequence != ordinal.checked_add(1).ok_or(Error::Bounds)?
            || previous.is_some_and(|p| cp.body.previous != p)
            || (ordinal == 0 && cp.body.previous != [0; 32])
        {
            return Err(Error::Receipt);
        }
        previous = Some(cp.digest().map_err(|_| Error::Receipt)?);
        ordinal = ordinal.checked_add(1).ok_or(Error::Bounds)?;
    }
    if ordinal > current.sequence
        || page.next
            != if ordinal == current.sequence {
                None
            } else {
                Some(ordinal)
            }
        || (page.next.is_none() && previous != Some(current.digest))
    {
        return Err(Error::Receipt);
    }
    previous.ok_or(Error::Receipt)
}
fn verify_file(
    cfg: &Config,
    owner: &RetainedControl,
    args: &Arguments,
    limit: usize,
) -> Result<()> {
    let max = args.number("--max-input-bytes", 16u64 << 20)?;
    if !(1..=VERIFY_BYTES).contains(&max) {
        return Err(Error::Bounds);
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(Path::new(args.required("--input")?))
        .map_err(|_| Error::Io)?;
    let metadata = file.metadata().map_err(|_| Error::Io)?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(Error::Permissions);
    }
    if metadata.len() > max {
        return Err(Error::Bounds);
    }
    let current = map(owner.current_floor())?;
    let mut reader = BufReader::new(file);
    let mut read = 0u64;
    let mut start = 0;
    let mut previous = Some([0; 32]);
    let mut complete = false;
    let mut pages = 0;
    loop {
        let mut line = vec![];
        let count = (&mut reader)
            .take(AUDIT_PAGE_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|_| Error::Io)?;
        if count == 0 {
            break;
        }
        read = read.checked_add(count as u64).ok_or(Error::Bounds)?;
        if read > max || line.len() > AUDIT_PAGE_BYTES || complete {
            return Err(Error::Bounds);
        }
        let page: ControlAuditPage = serde_json::from_slice(&line).map_err(|_| Error::Receipt)?;
        previous = Some(verify_page(cfg, &page, current, start, previous)?);
        pages += 1;
        match page.next {
            Some(next) => start = next,
            None => complete = true,
        }
        if map(owner.current_floor())? != current {
            return Err(Error::Authority);
        }
    }
    if !complete || pages == 0 {
        return Err(Error::Receipt);
    }
    emit(
        &json!({"status":"verified","pages":pages,"checkpoints":current.sequence,
        "current_floor":current,"scope":"configured_current_managed_authority",
        "permanent_erasure":"unsupported_caller_owned_keys"}),
        limit,
    )
}
