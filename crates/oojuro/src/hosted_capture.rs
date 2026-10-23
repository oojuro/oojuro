//! Explicit hosted connection and bounded, crash-recoverable Codex/Claude transcript delivery.
//! This does not change the local recorder or activate existing-history import.
use crate::hosted::read_private;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use oojuro_capture::{FileCtx, Format, ParseCounts, Rec, redact};
use reqwest::{
    Url,
    blocking::{Client, Response},
    header::{AUTHORIZATION, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    time::Duration,
};
const MIB: usize = 1 << 20;
const STATE_LIMIT: usize = 40 * MIB;
const LINE_LIMIT: usize = 16 * MIB;
const FILE_LIMIT: usize = 10_000;
type Result<T> = std::result::Result<T, &'static str>;
fn opaque(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn random() -> Result<[u8; 32]> {
    let mut b = [0; 32];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|_| "random_unavailable")?;
    Ok(b)
}
fn random_id() -> Result<String> {
    Ok(blake3::hash(&random()?).to_hex().to_string())
}
fn origin(value: &str) -> Result<String> {
    let u = Url::parse(value).map_err(|_| "invalid_capture_origin")?;
    if !(u.scheme() == "https"
        || u.scheme() == "http"
            && matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || u.path() != "/"
    {
        return Err("invalid_capture_origin");
    }
    Ok(u.origin().ascii_serialization())
}
fn ancestors(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err("absolute_private_path_required");
    }
    let uid = unsafe { libc::geteuid() };
    for p in path.ancestors().skip(1) {
        let m = fs::symlink_metadata(p).map_err(|_| "private_path_unavailable")?;
        if !m.is_dir()
            || ![0, uid].contains(&m.uid())
            || m.mode() & 0o022 != 0 && !(m.uid() == 0 && m.mode() & 0o1000 != 0)
        {
            return Err("unsafe_path_ancestor");
        }
    }
    Ok(())
}
fn directory(path: &Path) -> Result<()> {
    ancestors(path)?;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("private_directory_unavailable"),
    }
    let m = fs::symlink_metadata(path).map_err(|_| "private_directory_unavailable")?;
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o777 != 0o700 {
        return Err("mode_0700_directory_required");
    }
    File::open(path.parent().ok_or("invalid_directory")?)
        .and_then(|f| f.sync_all())
        .map_err(|_| "directory_sync_failed")
}
fn create_private(path: &Path, bytes: &[u8]) -> Result<()> {
    ancestors(path)?;
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "private_file_already_exists_or_unavailable")?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|_| "private_file_sync_failed")?;
    File::open(path.parent().ok_or("invalid_path")?)
        .and_then(|f| f.sync_all())
        .map_err(|_| "directory_sync_failed")
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    version: u32,
    base_url: String,
    source: String,
    journal: String,
    capture_key: String,
    credential_id: String,
    label: String,
    until: i64,
}
/// Public connection information: never includes the capture secret.
#[derive(Serialize)]
pub struct Approval {
    pub approval_url: String,
    pub fingerprint: String,
    pub expires_us: i64,
}
pub fn connect(
    path: &Path,
    base: &str,
    source: &str,
    journal: &str,
    label: &str,
) -> Result<Approval> {
    let base_url = origin(base)?;
    if !opaque(source)
        || !opaque(journal)
        || label.is_empty()
        || label.len() > 128
        || label.chars().any(char::is_control)
    {
        return Err("invalid_connection");
    }
    let secret = random()?;
    let namespace = format!("v3-{source}");
    let mut h = blake3::Hasher::new();
    h.update(b"oojuro-prefix-credential-v1");
    h.update(&(namespace.len() as u64).to_le_bytes());
    h.update(namespace.as_bytes());
    h.update(&secret);
    let credential_id = h.finalize().to_hex().to_string();
    let until = oojuro_capture::time::now_us()
        .checked_add(600_000_000)
        .ok_or("invalid_clock")?;
    let cfg = Connection {
        version: 1,
        base_url,
        source: source.into(),
        journal: journal.into(),
        capture_key: format!("ojc_{source}_{}", URL_SAFE_NO_PAD.encode(secret)),
        credential_id,
        label: label.into(),
        until,
    };
    create_private(
        path,
        &serde_json::to_vec(&cfg).map_err(|_| "invalid_connection")?,
    )?;
    approval(&cfg)
}
fn approval(cfg: &Connection) -> Result<Approval> {
    let mut url = Url::parse(&format!(
        "{}/app/manage/agents/{}/keys/connect",
        cfg.base_url, cfg.journal
    ))
    .map_err(|_| "invalid_connection")?;
    url.query_pairs_mut()
        .append_pair("credential", &cfg.credential_id)
        .append_pair("label", &cfg.label)
        .append_pair("until", &cfg.until.to_string());
    Ok(Approval {
        approval_url: url.into(),
        fingerprint: cfg.credential_id[..16].into(),
        expires_us: cfg.until,
    })
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    events: usize,
    metadata_bytes: usize,
    text_bytes: usize,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery {
    pub format: u32,
    pub journal: String,
    pub agent_id: String,
    pub key_id: String,
    pub producer_id: String,
    pub label: String,
    pub expires_us: i64,
    limits: Limits,
}
struct CaptureClient {
    cfg: Connection,
    http: Client,
    auth: HeaderValue,
}
impl CaptureClient {
    fn open(path: &Path) -> Result<Self> {
        let cfg: Connection = serde_json::from_slice(
            &read_private(path, 16384).map_err(|_| "private_capture_connection_required")?,
        )
        .map_err(|_| "invalid_connection")?;
        if cfg.version != 1
            || origin(&cfg.base_url)? != cfg.base_url
            || !opaque(&cfg.source)
            || !opaque(&cfg.journal)
            || !opaque(&cfg.credential_id)
        {
            return Err("invalid_connection");
        }
        let prefix = format!("ojc_{}_", cfg.source);
        let raw = cfg
            .capture_key
            .strip_prefix(&prefix)
            .ok_or("invalid_connection")?;
        let secret = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| "invalid_connection")?;
        if secret.len() != 32 || URL_SAFE_NO_PAD.encode(&secret) != raw {
            return Err("invalid_connection");
        }
        let ns = format!("v3-{}", cfg.source);
        let mut h = blake3::Hasher::new();
        h.update(b"oojuro-prefix-credential-v1");
        h.update(&(ns.len() as u64).to_le_bytes());
        h.update(ns.as_bytes());
        h.update(&secret);
        if h.finalize().to_hex().as_str() != cfg.credential_id {
            return Err("invalid_connection");
        }
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", cfg.capture_key))
            .map_err(|_| "invalid_connection")?;
        auth.set_sensitive(true);
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| "transport_unavailable")?;
        Ok(Self { cfg, http, auth })
    }
    fn discovery(&self) -> Result<Discovery> {
        let r = self
            .http
            .get(format!("{}/api/capture", self.cfg.base_url))
            .header(AUTHORIZATION, self.auth.clone())
            .send()
            .map_err(|_| "capture_transport_failed")?;
        if r.status().as_u16() == 404 {
            return Err("capture_not_authorized_or_expired");
        }
        let info: Discovery =
            serde_json::from_slice(&response(r)?).map_err(|_| "invalid_capture_discovery")?;
        if info.format != 1
            || info.journal != self.cfg.journal
            || info.key_id != self.cfg.credential_id
            || [&info.agent_id, &info.producer_id]
                .iter()
                .any(|v| !opaque(v))
            || info.expires_us <= oojuro_capture::time::now_us()
            || info.limits.events != 100
            || info.limits.metadata_bytes != 65536
            || info.limits.text_bytes != 16 * MIB
        {
            return Err("invalid_capture_discovery");
        }
        Ok(info)
    }
    fn send(&self, batch: &Batch) -> Result<()> {
        let (body, total) = wire(batch)?;
        let r = self
            .http
            .post(format!("{}/api/capture/events", self.cfg.base_url))
            .header(AUTHORIZATION, self.auth.clone())
            .header("content-type", "application/vnd.oojuro.capture.v1")
            .header("content-encoding", "identity")
            .body(body)
            .send()
            .map_err(|_| "capture_outcome_unknown")?;
        let ack: Ack = serde_json::from_slice(&response(r).map_err(|_| "capture_outcome_unknown")?)
            .map_err(|_| "capture_outcome_unknown")?;
        let mut ids = BTreeSet::new();
        if ack.request_id != batch.request
            || ack.generation == 0
            || ack.recorded_bytes > total
            || ack.records.len() != batch.events.len()
            || ack.records.iter().zip(&batch.events).any(|(r, e)| {
                r.event_id != e.event_id || !opaque(&r.record_id) || !ids.insert(&r.record_id)
            })
        {
            return Err("capture_outcome_unknown");
        }
        Ok(())
    }
}
fn response(mut r: Response) -> Result<Vec<u8>> {
    if r.status().as_u16() != 200
        || r.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_none_or(|s| s.split(';').next() != Some("application/json"))
        || r.headers()
            .get("content-encoding")
            .is_some_and(|v| v != "identity")
    {
        return Err("capture_response_refused");
    }
    let length = r
        .headers()
        .get("content-length")
        .map(|v| {
            v.to_str()
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or("invalid_capture_response")
        })
        .transpose()?;
    if length.is_some_and(|n| n > 65536) {
        return Err("capture_response_bound");
    }
    let mut b = Vec::new();
    (&mut r)
        .take(65537)
        .read_to_end(&mut b)
        .map_err(|_| "capture_response_truncated")?;
    if b.len() > 65536 || length.is_some_and(|n| n != b.len()) {
        return Err("capture_response_bound");
    }
    Ok(b)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ack {
    request_id: String,
    generation: u64,
    recorded_bytes: usize,
    records: Vec<AckRow>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AckRow {
    event_id: String,
    record_id: String,
    #[allow(dead_code)]
    replayed: bool,
}
pub fn status(path: &Path) -> Result<Discovery> {
    CaptureClient::open(path)?.discovery()
}
pub fn resume_approval(path: &Path) -> Result<Approval> {
    approval(&CaptureClient::open(path)?.cfg)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    event_id: String,
    run_id: String,
    adapter: String,
    input: oojuro_engine::record::Input,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    request: String,
    events: Vec<Event>,
}
fn wire(batch: &Batch) -> Result<(Vec<u8>, usize)> {
    if batch.events.is_empty() || batch.events.len() > 100 {
        return Err("capture_line_event_bound");
    }
    let mut text = Vec::new();
    let mut metadata = Vec::new();
    for e in &batch.events {
        let mut input = e.input.clone();
        let n = input.text.len();
        if text.len() + n > 16 * MIB {
            return Err("capture_line_text_bound");
        }
        text.extend_from_slice(input.text.as_bytes());
        input.text.clear();
        metadata.push(json!({"event_id":e.event_id,"run_id":e.run_id,"adapter":e.adapter,"input":input,"text_length":n}));
    }
    let meta =
        serde_json::to_vec(&json!({"format":1,"request_id":batch.request,"events":metadata}))
            .map_err(|_| "invalid_capture_batch")?;
    if meta.len() > 65536 {
        return Err("capture_line_metadata_bound");
    }
    let mut body = (meta.len() as u32).to_be_bytes().to_vec();
    body.extend(meta);
    let total = text.len();
    body.extend(text);
    Ok((body, total))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    dev: u64,
    ino: u64,
    offset: u64,
    boundary: String,
    skip_partial: bool,
    ctx: FileCtx,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    base: String,
    journal: String,
    agent: String,
    producer: String,
    adapter: String,
    root: PathBuf,
    files: BTreeMap<PathBuf, Source>,
    pending: Option<Batch>,
    delivered: u64,
}
struct Spool {
    dir: PathBuf,
    _lock: File,
    state: State,
}
fn boundary(file: &mut File, offset: u64) -> Result<String> {
    let start = offset.saturating_sub(256);
    file.seek(SeekFrom::Start(start))
        .map_err(|_| "capture_source_io")?;
    let mut b = vec![0; (offset - start) as usize];
    file.read_exact(&mut b)
        .map_err(|_| "capture_source_changed")?;
    Ok(blake3::hash(&b).to_hex().to_string())
}
fn transcript(path: &Path) -> Result<File> {
    ancestors(path)?;
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "capture_source_unavailable")?;
    let m = f.metadata().map_err(|_| "capture_source_unavailable")?;
    if !m.is_file()
        || m.uid() != unsafe { libc::geteuid() }
        || m.nlink() != 1
        || m.mode() & 0o022 != 0
    {
        return Err("unsafe_capture_source");
    }
    Ok(f)
}
fn scan(root: &Path, format: Format) -> Result<Vec<PathBuf>> {
    let mut dirs = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut visited = 0;
    while let Some(dir) = dirs.pop() {
        ancestors(&dir.join("check"))?;
        for entry in fs::read_dir(dir).map_err(|_| "capture_source_unavailable")? {
            visited += 1;
            if visited > 50_000 {
                return Err("capture_directory_bound");
            }
            let e = entry.map_err(|_| "capture_source_unavailable")?;
            let t = e.file_type().map_err(|_| "capture_source_unavailable")?;
            if t.is_dir() {
                dirs.push(e.path());
            } else if t.is_file()
                && e.path().extension().is_some_and(|s| s == "jsonl")
                && (format != Format::Codex
                    || e.file_name().to_string_lossy().starts_with("rollout-"))
            {
                files.push(e.path());
                if files.len() > FILE_LIMIT {
                    return Err("capture_file_bound");
                }
            }
        }
    }
    files.sort();
    Ok(files)
}
impl Spool {
    fn open(
        dir: &Path,
        client: &CaptureClient,
        info: &Discovery,
        root: &Path,
        format: Format,
    ) -> Result<Self> {
        directory(dir)?;
        let lock_path = dir.join("lock");
        match create_private(&lock_path, b"") {
            Ok(()) => {}
            Err(_) if lock_path.exists() => {}
            Err(e) => return Err(e),
        }
        read_private(&lock_path, 0).map_err(|_| "private_spool_lock_required")?;
        let lock = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(lock_path)
            .map_err(|_| "spool_lock_failed")?;
        if unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&lock),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        } != 0
        {
            return Err("capture_already_running");
        }
        // A crashed atomic replacement may leave one private temporary copy.
        for entry in fs::read_dir(dir)
            .map_err(|_| "private_spool_unavailable")?
            .take(128)
        {
            let entry = entry.map_err(|_| "private_spool_unavailable")?;
            if entry
                .file_name()
                .to_str()
                .and_then(|s| s.strip_prefix("pending-"))
                .is_some_and(opaque)
            {
                read_private(&entry.path(), STATE_LIMIT).map_err(|_| "invalid_spool_temporary")?;
                fs::remove_file(entry.path()).map_err(|_| "spool_cleanup_failed")?;
            }
        }
        let path = dir.join("state.json");
        let existed = path.try_exists().map_err(|_| "private_spool_unavailable")?;
        let state = if existed {
            serde_json::from_slice(
                &read_private(&path, STATE_LIMIT).map_err(|_| "private_spool_required")?,
            )
            .map_err(|_| "invalid_capture_spool")?
        } else {
            State {
                version: 1,
                base: client.cfg.base_url.clone(),
                journal: info.journal.clone(),
                agent: info.agent_id.clone(),
                producer: info.producer_id.clone(),
                adapter: format.name().into(),
                root: root.into(),
                files: BTreeMap::new(),
                pending: None,
                delivered: 0,
            }
        };
        let mut spool = Self {
            dir: dir.into(),
            _lock: lock,
            state,
        };
        let s = &spool.state;
        if s.version != 1
            || s.base != client.cfg.base_url
            || s.journal != info.journal
            || s.agent != info.agent_id
            || s.producer != info.producer_id
            || s.root != root
            || s.adapter != format.name()
            || s.files.len() > FILE_LIMIT
        {
            return Err("capture_spool_binding_mismatch");
        }
        if !existed {
            // Existing bytes are explicitly excluded. A partial old line is excluded through its next LF.
            for p in scan(root, format)? {
                let mut f = transcript(&p)?;
                let m = f.metadata().map_err(|_| "capture_source_io")?;
                let offset = m.len();
                let b = boundary(&mut f, offset)?;
                let skip_partial = if offset == 0 {
                    false
                } else {
                    f.seek(SeekFrom::Start(offset - 1))
                        .map_err(|_| "capture_source_io")?;
                    let mut last = [0];
                    f.read_exact(&mut last).map_err(|_| "capture_source_io")?;
                    last[0] != b'\n'
                };
                spool.state.files.insert(
                    p.clone(),
                    Source {
                        dev: m.dev(),
                        ino: m.ino(),
                        offset,
                        boundary: b,
                        skip_partial,
                        ctx: context(format, &p),
                    },
                );
            }
            spool.save()?;
        }
        Ok(spool)
    }
    fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec(&self.state).map_err(|_| "invalid_capture_spool")?;
        if bytes.len() > STATE_LIMIT {
            return Err("capture_spool_full");
        }
        let tmp = self.dir.join(format!("pending-{}", random_id()?));
        create_private(&tmp, &bytes)?;
        fs::rename(&tmp, self.dir.join("state.json")).map_err(|_| "capture_spool_sync_failed")?;
        File::open(&self.dir)
            .and_then(|f| f.sync_all())
            .map_err(|_| "capture_spool_sync_failed")
    }
    fn flush(&mut self, client: &CaptureClient) -> Result<()> {
        if let Some(batch) = &self.state.pending {
            let info = client.discovery()?;
            if info.agent_id != self.state.agent
                || info.journal != self.state.journal
                || info.producer_id != self.state.producer
            {
                return Err("capture_spool_binding_mismatch");
            }
            client.send(batch)?;
            self.state.delivered = self
                .state
                .delivered
                .checked_add(batch.events.len() as u64)
                .ok_or("capture_count_bound")?;
            self.state.pending = None;
            self.save()?;
        }
        Ok(())
    }
    fn tick(&mut self, client: &CaptureClient, format: Format) -> Result<usize> {
        self.flush(client)?;
        let red = redact::Redactor::new();
        let mut processed = 0;
        for p in scan(&self.state.root, format)? {
            let mut f = transcript(&p)?;
            let m = f.metadata().map_err(|_| "capture_source_io")?;
            self.state.files.entry(p.clone()).or_insert_with(|| Source {
                dev: m.dev(),
                ino: m.ino(),
                offset: 0,
                boundary: blake3::hash(b"").to_hex().to_string(),
                skip_partial: false,
                ctx: context(format, &p),
            });
            if self.state.files.len() > FILE_LIMIT {
                return Err("capture_file_bound");
            }
            let source = self
                .state
                .files
                .get_mut(&p)
                .ok_or("invalid_capture_spool")?;
            if source.dev != m.dev()
                || source.ino != m.ino()
                || m.len() < source.offset
                || boundary(&mut f, source.offset)? != source.boundary
            {
                return Err(
                    "capture_source_changed; original source retained, explicit reconnection required",
                );
            }
            f.seek(SeekFrom::Start(source.offset))
                .map_err(|_| "capture_source_io")?;
            let mut line = Vec::new();
            let mut reader = std::io::BufReader::new(f.take((LINE_LIMIT + 1) as u64));
            std::io::BufRead::read_until(&mut reader, b'\n', &mut line)
                .map_err(|_| "capture_source_io")?;
            if line.len() > LINE_LIMIT {
                return Err("capture_line_bound; source not advanced");
            }
            if line.last() != Some(&b'\n') {
                continue;
            }
            let mut records = Vec::new();
            let mut rc = redact::Counts::default();
            let mut pc = ParseCounts::default();
            let offset = source.offset;
            if !source.skip_partial {
                match format {
                    Format::Codex => oojuro_capture::codex::parse_line(
                        &mut source.ctx,
                        &line,
                        offset,
                        &red,
                        &mut rc,
                        &mut pc,
                        &mut records,
                    ),
                    Format::ClaudeCode => oojuro_capture::claude::parse_line(
                        &mut source.ctx,
                        &line,
                        offset,
                        &red,
                        &mut rc,
                        &mut pc,
                        &mut records,
                    ),
                }
            }
            if pc.bad_lines > 0 {
                return Err("invalid_transcript_line; source not advanced");
            }
            source.skip_partial = false;
            source.offset += line.len() as u64;
            let mut current = transcript(&p)?;
            source.boundary = boundary(&mut current, source.offset)?;
            let run = blake3::hash(
                format!(
                    "{}:{}:{}:{}",
                    self.state.journal,
                    p.display(),
                    m.dev(),
                    m.ino()
                )
                .as_bytes(),
            )
            .to_hex()
            .to_string();
            let events = records
                .into_iter()
                .enumerate()
                .map(|(k, r)| event(r, &self.state.agent, &run, offset, k, &line, format))
                .collect::<Result<Vec<_>>>()?;
            if !events.is_empty() {
                let batch = Batch {
                    request: random_id()?,
                    events,
                };
                wire(&batch)?;
                self.state.pending = Some(batch);
            }
            // The cursor and original pending batch become durable together, before network effects.
            self.save()?;
            self.flush(client)?;
            processed += 1;
            if processed >= 100 {
                break;
            }
        }
        Ok(processed)
    }
}
fn context(format: Format, path: &Path) -> FileCtx {
    let mut ctx = match format {
        Format::Codex => oojuro_capture::codex::new_ctx(path),
        Format::ClaudeCode => oojuro_capture::claude::new_ctx(path),
    };
    // No parser may read referenced tool-result side files from the user's machine.
    ctx.results_root = None;
    ctx
}
fn event(
    r: Rec,
    agent: &str,
    run: &str,
    offset: u64,
    k: usize,
    line: &[u8],
    format: Format,
) -> Result<Event> {
    use oojuro_engine::record::{Input, Kind, Status};
    let kind = match r.kind {
        0 => Kind::System,
        1 => Kind::Input,
        2 => Kind::Thinking,
        3 => Kind::Output,
        4 => Kind::ToolCall,
        5 => Kind::ToolResult,
        _ => return Err("unsupported_capture_kind"),
    };
    let mut h = blake3::Hasher::new();
    h.update(b"oojuro-hosted-transcript-v1");
    h.update(run.as_bytes());
    h.update(&offset.to_le_bytes());
    h.update(&(k as u64).to_le_bytes());
    h.update(blake3::hash(line).as_bytes());
    Ok(Event {
        event_id: h.finalize().to_hex().to_string(),
        run_id: run.into(),
        adapter: format!("{}/transcript-redacted-v1", format.name()),
        input: Input {
            agent_id: agent.into(),
            parent_id: None,
            valid_at: r.ts_us,
            kind,
            tool: (!r.tool.is_empty()).then_some(r.tool),
            status: match r.status {
                2 => Some(Status::Ok),
                3 => Some(Status::Error),
                _ => None,
            },
            exit: None,
            key: None,
            text: r.text,
            indexed: oojuro_capture::kind::indexed(r.kind)
                && !r
                    .meta
                    .iter()
                    .any(|(key, _)| *key == oojuro_capture::marker::META),
            attrs: Default::default(),
        },
    })
}
/// A retry-safe, unindexed setup marker. It verifies delivery, not automatic transcript coverage.
pub fn verify_delivery(connection: &Path) -> Result<()> {
    use oojuro_engine::record::{Input, Kind};
    let client = CaptureClient::open(connection)?;
    let info = client.discovery()?;
    let id =
        blake3::hash(format!("oojuro-connect-check-v1:{}", client.cfg.credential_id).as_bytes())
            .to_hex()
            .to_string();
    let batch = Batch {
        request: format!("connect_check_{}", client.cfg.credential_id),
        events: vec![Event {
            event_id: id.clone(),
            run_id: id,
            adapter: "oojuro-connection-check-v1".into(),
            input: Input {
                agent_id: info.agent_id,
                parent_id: None,
                valid_at: client.cfg.until - 600_000_000,
                kind: Kind::System,
                tool: None,
                status: None,
                exit: None,
                key: None,
                text: "Oojuro capture connection check".into(),
                attrs: Default::default(),
                indexed: false,
            },
        }],
    };
    client.send(&batch)
}
#[derive(Serialize)]
pub struct Report {
    pub delivered: u64,
    pub pending: usize,
    pub tracked_files: usize,
    pub new_lines: usize,
}
pub fn capture(
    connection: &Path,
    state: &Path,
    root: &Path,
    adapter: &str,
    once: bool,
) -> Result<Report> {
    let format = Format::from_name(adapter).ok_or("supported_adapters: codex, claude-code")?;
    let client = CaptureClient::open(connection)?;
    let info = client.discovery()?;
    let mut spool = Spool::open(state, &client, &info, root, format)?;
    loop {
        let new_lines = spool.tick(&client, format)?;
        let report = Report {
            delivered: spool.state.delivered,
            pending: spool.state.pending.as_ref().map_or(0, |b| b.events.len()),
            tracked_files: spool.state.files.len(),
            new_lines,
        };
        if once {
            return Ok(report);
        }
        if new_lines == 0 {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::{
        net::TcpListener,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };
    struct Fixture {
        dir: PathBuf,
        base: String,
        stop: Arc<AtomicBool>,
        lost: Arc<AtomicBool>,
        wires: Arc<Mutex<Vec<Vec<u8>>>>,
        handle: Option<thread::JoinHandle<()>>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            self.handle.take().unwrap().join().unwrap();
            fs::remove_dir_all(&self.dir).unwrap();
        }
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("oojuro-hosted-capture-{}", random_id().unwrap()));
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let lost = Arc::new(AtomicBool::new(false));
            let wires = Arc::new(Mutex::new(Vec::new()));
            let (st, ls, ws, root) = (stop.clone(), lost.clone(), wires.clone(), dir.clone());
            let handle = thread::spawn(move || {
                while !st.load(Ordering::SeqCst) {
                    let (mut s, _) = match listener.accept() {
                        Ok(s) => s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(e) => panic!("{e}"),
                    };
                    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                    let mut req = Vec::new();
                    let mut part = [0; 8192];
                    let (header, body) = loop {
                        let n = s.read(&mut part).unwrap();
                        assert!(n > 0);
                        req.extend_from_slice(&part[..n]);
                        assert!(req.len() < 20 * MIB);
                        if let Some(at) = req.windows(4).position(|b| b == b"\r\n\r\n") {
                            let header = String::from_utf8(req[..at].to_vec()).unwrap();
                            let len = header
                                .to_ascii_lowercase()
                                .lines()
                                .find_map(|l| {
                                    l.strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            if req.len() >= at + 4 + len {
                                break (header, req[at + 4..].to_vec());
                            }
                        }
                    };
                    let cfg: Connection =
                        serde_json::from_slice(&fs::read(root.join("connection.json")).unwrap())
                            .unwrap();
                    assert!(header.to_ascii_lowercase().contains(&format!(
                        "authorization: bearer {}",
                        cfg.capture_key.to_ascii_lowercase()
                    )));
                    let value = if header.starts_with("GET ") {
                        json!({"format":1,"journal":cfg.journal,"agent_id":"a".repeat(64),"key_id":cfg.credential_id,"producer_id":"b".repeat(64),"label":"Fixture","expires_us":oojuro_capture::time::now_us()+600_000_000,"limits":{"events":100,"metadata_bytes":65536,"text_bytes":16*MIB}})
                    } else {
                        ws.lock().unwrap().push(body.clone());
                        let n = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
                        let meta: serde_json::Value =
                            serde_json::from_slice(&body[4..4 + n]).unwrap();
                        json!({"request_id":meta["request_id"],"generation":1,"recorded_bytes":body.len()-4-n,"records":meta["events"].as_array().unwrap().iter().map(|e|json!({"event_id":e["event_id"],"record_id":e["event_id"],"replayed":false})).collect::<Vec<_>>()})
                    };
                    let bytes = serde_json::to_vec(&value).unwrap();
                    let truncated = header.starts_with("POST ") && ls.swap(false, Ordering::SeqCst);
                    write!(s,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()+usize::from(truncated)).unwrap();
                    s.write_all(&bytes).unwrap();
                }
            });
            Self {
                dir,
                base,
                stop,
                lost,
                wires,
                handle: Some(handle),
            }
        }
        fn connection(&self) -> PathBuf {
            let path = self.dir.join("connection.json");
            connect(
                &path,
                &self.base,
                &"1".repeat(64),
                &"2".repeat(64),
                "Local Codex",
            )
            .unwrap();
            path
        }
    }
    fn codex(text: &str) -> Vec<u8> {
        format!("{}\n",json!({"timestamp":"2026-10-23T12:00:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}})).into_bytes()
    }
    #[test]
    fn local_secret_never_enters_approval_and_unsafe_connections_are_refused() {
        let f = Fixture::new();
        let path = f.connection();
        let c = CaptureClient::open(&path).unwrap();
        let a = approval(&c.cfg).unwrap();
        assert!(!a.approval_url.contains(&c.cfg.capture_key));
        assert_eq!(a.fingerprint, &c.cfg.credential_id[..16]);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert!(
            connect(
                &path,
                &f.base,
                &"1".repeat(64),
                &"2".repeat(64),
                "overwrite"
            )
            .is_err()
        );
        for bad in [
            "http://example.com",
            "https://user@example.com",
            "https://example.com/a",
            "https://example.com/?token=secret",
        ] {
            assert!(origin(bad).is_err());
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(CaptureClient::open(&path).is_err());
    }
    #[test]
    fn interrupted_delivery_reopens_same_bytes_and_does_not_import_existing_history() {
        let f = Fixture::new();
        let path = f.connection();
        let client = CaptureClient::open(&path).unwrap();
        let info = client.discovery().unwrap();
        let root = f.dir.join("transcripts");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let source = root.join("rollout-test.jsonl");
        create_private(&source, b"old malformed history is never parsed\n").unwrap();
        let spool_dir = f.dir.join("spool");
        let mut s = Spool::open(&spool_dir, &client, &info, &root, Format::Codex).unwrap();
        assert_eq!(s.tick(&client, Format::Codex).unwrap(), 0);
        assert!(f.wires.lock().unwrap().is_empty());
        let original = codex("original α 🦉\0");
        let mut file = OpenOptions::new().append(true).open(&source).unwrap();
        file.write_all(&original).unwrap();
        file.sync_all().unwrap();
        f.lost.store(true, Ordering::SeqCst);
        assert_eq!(
            s.tick(&client, Format::Codex),
            Err("capture_outcome_unknown")
        );
        assert!(s.state.pending.is_some());
        drop(s);
        let mut s = Spool::open(&spool_dir, &client, &info, &root, Format::Codex).unwrap();
        assert_eq!(s.tick(&client, Format::Codex).unwrap(), 0);
        assert!(s.state.pending.is_none());
        assert_eq!(s.state.delivered, 1);
        let wires = f.wires.lock().unwrap();
        assert_eq!(wires.len(), 2);
        assert_eq!(wires[0], wires[1]);
        let n = u32::from_be_bytes(wires[0][..4].try_into().unwrap()) as usize;
        assert_eq!(&wires[0][4 + n..], "original α 🦉\0".as_bytes());
    }
    #[test]
    fn spool_single_writer_scope_and_changed_source_refuse_without_advancing() {
        let f = Fixture::new();
        let path = f.connection();
        let client = CaptureClient::open(&path).unwrap();
        let info = client.discovery().unwrap();
        let root = f.dir.join("transcripts");
        directory(&root).unwrap();
        let source = root.join("rollout-test.jsonl");
        create_private(&source, &codex("existing")).unwrap();
        let dir = f.dir.join("spool");
        let mut s = Spool::open(&dir, &client, &info, &root, Format::Codex).unwrap();
        assert!(Spool::open(&dir, &client, &info, &root, Format::Codex).is_err());
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(source)
            .unwrap()
            .write_all(b"truncated\n")
            .unwrap();
        assert!(s.tick(&client, Format::Codex).is_err());
        assert!(f.wires.lock().unwrap().is_empty());
        drop(s);
        assert!(Spool::open(&dir, &client, &info, &root, Format::ClaudeCode).is_err());
    }
    #[test]
    fn claude_redacts_known_secrets_and_never_reads_referenced_tool_result_files() {
        let f = Fixture::new();
        let side = f.dir.join("private-output.txt");
        create_private(&side, b"must never read this secondary file").unwrap();
        let text = format!(
            "<persisted-output>\nFull output saved to: {}\npreview",
            side.display()
        );
        let line=serde_json::to_vec(&json!({"type":"user","timestamp":"2026-10-23T12:00:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":text}]}})).unwrap();
        let mut ctx = context(Format::ClaudeCode, &f.dir.join("session.jsonl"));
        let mut rc = redact::Counts::default();
        let mut pc = ParseCounts::default();
        let mut records = Vec::new();
        oojuro_capture::claude::parse_line(
            &mut ctx,
            &line,
            0,
            &redact::Redactor::new(),
            &mut rc,
            &mut pc,
            &mut records,
        );
        assert!(!records.is_empty());
        assert!(records.iter().any(|r| r.text == text));
        assert!(records.iter().all(|r| !r.text.contains("must never read")));
        let mut records = Vec::new();
        oojuro_capture::codex::parse_line(
            &mut context(Format::Codex, &f.dir.join("rollout.jsonl")),
            &codex(
                "ojc_1111111111111111111111111111111111111111111111111111111111111111_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ),
            0,
            &redact::Redactor::new(),
            &mut rc,
            &mut pc,
            &mut records,
        );
        assert!(records[0].text.starts_with("[REDACTED:oojuro:"));
    }
}
