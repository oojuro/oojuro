//! Common hosted project reads. The local recorder and append-only capture use separate credentials.
use oojuro_engine::record::{Input, Kind, MAX_TEXT_BYTES, Status};
use reqwest::{
    Url,
    blocking::{Client, Response},
    header::{AUTHORIZATION, HeaderValue},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path},
    time::Duration,
};

const MIB: usize = 1 << 20;
#[derive(Debug)]
pub enum Error {
    Connection,
    PrivateFile,
    Request,
    Transport,
    Http(u16),
    Response,
    Bounds,
    SearchUnknown(Option<u16>),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(status) => write!(f, "hosted_http_{status}"),
            Self::SearchUnknown(Some(status)) => write!(
                f,
                "hosted_search_outcome_unknown_http_{status}; retain original request and query"
            ),
            Self::SearchUnknown(None) => {
                f.write_str("hosted_search_outcome_unknown; retain original request and query")
            }
            _ => f.write_str(match self {
                Self::Connection => "invalid_hosted_connection",
                Self::PrivateFile => "private_hosted_file_required",
                Self::Request => "invalid_hosted_request",
                Self::Transport => "hosted_transport_failed",
                Self::Response => "invalid_hosted_response",
                Self::Bounds => "hosted_size_bound",
                Self::Http(_) => unreachable!(),
                Self::SearchUnknown(_) => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;
fn opaque(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn cursor(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 2 * MIB
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn label(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}
/// Protected absolute file, no symlink components, hardlink aliases or writable ancestors.
fn private_ancestors(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(Error::PrivateFile);
    }
    let uid = unsafe { libc::geteuid() };
    for parent in path.ancestors().skip(1) {
        let m = fs::symlink_metadata(parent).map_err(|_| Error::PrivateFile)?;
        if !m.is_dir()
            || ![0, uid].contains(&m.uid())
            || (m.mode() & 0o022 != 0 && !(m.uid() == 0 && m.mode() & 0o1000 != 0))
        {
            return Err(Error::PrivateFile);
        }
    }
    Ok(())
}
pub fn read_private(path: &Path, limit: usize) -> Result<Vec<u8>> {
    private_ancestors(path)?;
    let uid = unsafe { libc::geteuid() };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Error::PrivateFile)?;
    let m = file.metadata().map_err(|_| Error::PrivateFile)?;
    if !m.is_file() || m.uid() != uid || m.nlink() != 1 || m.mode() & 0o777 != 0o600 {
        return Err(Error::PrivateFile);
    }
    if m.len() > limit as u64 {
        return Err(Error::Bounds);
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::PrivateFile)?;
    if bytes.len() > limit {
        return Err(Error::Bounds);
    }
    Ok(bytes)
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    version: u32,
    base_url: String,
    project: String,
    read_key: String,
}
/// Create-only project read setup. The key is never accepted on argv or returned.
pub fn connect_project(path: &Path, base_url: &str, project: &str, key_file: &Path) -> Result<()> {
    let key = read_private(key_file, 256)?;
    let read_key = std::str::from_utf8(&key)
        .map_err(|_| Error::Connection)?
        .trim();
    let c = Connection {
        version: 1,
        base_url: base_url.into(),
        project: project.into(),
        read_key: read_key.into(),
    };
    // Use precisely the same validation as every later hosted read.
    ProjectClient::from_connection(&c)?;
    private_ancestors(path)?;
    let bytes = serde_json::to_vec(&c).map_err(|_| Error::Connection)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Error::PrivateFile)?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::PrivateFile)?;
    fs::File::open(path.parent().ok_or(Error::PrivateFile)?)
        .and_then(|f| f.sync_all())
        .map_err(|_| Error::PrivateFile)
}
/// No Debug or serialization: a connection never prints its bearer.
pub struct ProjectClient {
    base: String,
    auth: HeaderValue,
    client: Client,
}
#[derive(Default)]
pub struct Selection {
    pub from: Option<i64>,
    pub through: Option<i64>,
    pub agent_id: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}
impl Selection {
    fn fields(&self) -> Result<Vec<(&'static str, String)>> {
        if self.from.zip(self.through).is_some_and(|(a, b)| a > b)
            || self.agent_id.as_ref().is_some_and(|v| !opaque(v))
            || self.limit.is_some_and(|v| !(1..=1000).contains(&v))
            || self.cursor.as_ref().is_some_and(|v| !cursor(v))
        {
            return Err(Error::Request);
        }
        let mut fields = Vec::new();
        if let Some(v) = self.from {
            fields.push(("from", v.to_string()));
        }
        if let Some(v) = self.through {
            fields.push(("through", v.to_string()));
        }
        if let Some(v) = &self.agent_id {
            fields.push(("agent_id", v.clone()));
        }
        if let Some(v) = self.limit {
            fields.push(("limit", v.to_string()));
        }
        if let Some(v) = &self.cursor {
            fields.push(("cursor", v.clone()));
        }
        Ok(fields)
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchPage {
    pub snapshot: String,
    pub hits: Vec<SearchHit>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchHit {
    pub journal: String,
    pub record: Descriptor,
    pub score_bits: f64,
    pub continued: bool,
    pub excerpt_start: usize,
    pub excerpt_end: usize,
    pub excerpt: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub snapshot: String,
    pub records: Vec<Row>,
    #[serde(deserialize_with = "present")]
    pub next: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub journal: String,
    pub record: Descriptor,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub id: String,
    pub record: Metadata,
    pub text_bytes: u64,
    pub generation: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "capture_metadata"
    )]
    pub capture: Option<Capture>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub event_id: String,
    pub run_id: String,
}
fn capture_metadata<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Capture>, D::Error> {
    Capture::deserialize(d).map(Some)
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub schema: u32,
    pub id: u64,
    pub writer: String,
    pub adapter: String,
    pub agent_id: String,
    #[serde(deserialize_with = "present")]
    pub parent_id: Option<String>,
    pub valid_at: i64,
    pub recorded_at: i64,
    pub kind: Kind,
    #[serde(deserialize_with = "present")]
    pub tool: Option<String>,
    #[serde(deserialize_with = "present")]
    pub status: Option<Status>,
    #[serde(deserialize_with = "present")]
    pub exit: Option<i32>,
    #[serde(deserialize_with = "present")]
    pub key: Option<String>,
    pub attrs: BTreeMap<String, serde_json::Value>,
    pub indexed: bool,
}
impl Metadata {
    fn valid(&self) -> bool {
        self.schema == 1
            && self.id > 0
            && opaque(&self.writer)
            && opaque(&self.agent_id)
            && label(&self.adapter, 256)
            && Input {
                agent_id: self.agent_id.clone(),
                parent_id: self.parent_id.clone(),
                valid_at: self.valid_at,
                kind: self.kind,
                tool: self.tool.clone(),
                status: self.status,
                exit: self.exit,
                key: self.key.clone(),
                text: String::new(),
                attrs: self.attrs.clone(),
                indexed: self.indexed,
            }
            .validate()
            .is_ok()
    }
}
fn valid_row(journal: &str, d: &Descriptor, s: &Selection) -> bool {
    let m = &d.record;
    opaque(journal)
        && opaque(&d.id)
        && m.valid()
        && d.text_bytes <= MAX_TEXT_BYTES as u64
        && d.generation > 0
        && d.capture
            .as_ref()
            .is_none_or(|c| opaque(&c.event_id) && opaque(&c.run_id))
        && s.agent_id.as_ref().is_none_or(|a| a == &m.agent_id)
        && s.from.is_none_or(|v| m.valid_at >= v)
        && s.through.is_none_or(|v| m.valid_at <= v)
}
impl ProjectClient {
    pub fn open(path: &Path) -> Result<Self> {
        let c: Connection =
            serde_json::from_slice(&read_private(path, 8192)?).map_err(|_| Error::Connection)?;
        Self::from_connection(&c)
    }
    fn from_connection(c: &Connection) -> Result<Self> {
        let url = Url::parse(&c.base_url).map_err(|_| Error::Connection)?;
        let local = url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        let key = c.read_key.strip_prefix("ojp_").ok_or(Error::Connection)?;
        if c.version != 1
            || !opaque(&c.project)
            || !(32..=100).contains(&key.len())
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || (!local && url.scheme() != "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.host_str().is_none()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Connection);
        }
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", c.read_key))
            .map_err(|_| Error::Connection)?;
        auth.set_sensitive(true);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .referer(false)
            .https_only(!local)
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .build()
            .map_err(|_| Error::Connection)?;
        Ok(Self {
            base: format!(
                "{}/api/projects/{}/",
                url.origin().ascii_serialization(),
                c.project
            ),
            auth,
            client,
        })
    }
    fn collect(&self, response: Response, media: &str, max: usize) -> Result<Vec<u8>> {
        if response.status().as_u16() != 200 {
            return Err(Error::Http(response.status().as_u16()));
        }
        let h = response.headers();
        if h.get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case(media)
            })
            != Some(true)
            || h.get("content-encoding").is_some_and(|v| v != "identity")
        {
            return Err(Error::Response);
        }
        if h.get_all("content-length").iter().count() > 1 {
            return Err(Error::Response);
        }
        let declared = h
            .get("content-length")
            .map(|v| {
                let s = v.to_str().map_err(|_| Error::Response)?;
                if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
                    return Err(Error::Response);
                }
                s.parse::<usize>().map_err(|_| Error::Bounds)
            })
            .transpose()?;
        if declared.is_some_and(|n| n > max) {
            return Err(Error::Bounds);
        }
        let mut bytes = Vec::new();
        response
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Transport)?;
        if bytes.len() > max {
            return Err(Error::Bounds);
        }
        if declared.is_some_and(|n| n != bytes.len()) {
            return Err(Error::Response);
        }
        Ok(bytes)
    }
    pub fn records(&self, s: &Selection) -> Result<Page> {
        let fields = s.fields()?;
        let body = reqwest::Url::parse_with_params("https://unused.invalid/", &fields)
            .map_err(|_| Error::Request)?;
        let body = body.query().unwrap_or("");
        if body.len() > 2 * MIB {
            return Err(Error::Bounds);
        }
        let response = self
            .client
            .post(format!("{}records", self.base))
            .header(AUTHORIZATION, self.auth.clone())
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept-encoding", "identity")
            .body(body.to_owned())
            .send()
            .map_err(|_| Error::Transport)?;
        let bytes = self.collect(response, "application/json", 4 * MIB)?;
        let page: Page = serde_json::from_slice(&bytes).map_err(|_| Error::Response)?;
        if !opaque(&page.snapshot)
            || page.records.len() > s.limit.unwrap_or(100)
            || page
                .next
                .as_ref()
                .is_some_and(|v| !cursor(v) || page.records.is_empty())
        {
            return Err(Error::Response);
        }
        let mut seen = BTreeSet::new();
        for row in &page.records {
            let d = &row.record;
            if !valid_row(&row.journal, d, s) || !seen.insert((&row.journal, &d.id)) {
                return Err(Error::Response);
            }
        }
        Ok(page)
    }
    /// Metered operation: an uncertain reply must retain the original identity.
    /// Repeating it obtains status only; neither client nor server reruns it.
    pub fn search(&self, request: &str, text: &str, s: &Selection, k: usize) -> Result<SearchPage> {
        if !opaque(request)
            || text.trim().is_empty()
            || text.len() > 4096
            || !(1..=100).contains(&k)
            || s.limit.is_some()
            || s.cursor.is_some()
        {
            return Err(Error::Request);
        }
        let mut fields = s.fields()?;
        fields.extend([
            ("request_id", request.into()),
            ("q", text.into()),
            ("k", k.to_string()),
        ]);
        let encoded = Url::parse_with_params("https://unused.invalid/", &fields)
            .map_err(|_| Error::Request)?;
        let body = encoded.query().unwrap_or("");
        if body.len() > 32 * 1024 {
            return Err(Error::Bounds);
        }
        let response = self
            .client
            .post(format!("{}search", self.base))
            .header(AUTHORIZATION, self.auth.clone())
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept-encoding", "identity")
            .body(body.to_owned())
            .send()
            .map_err(|_| Error::SearchUnknown(None))?;
        let result = (|| {
            let page: SearchPage =
                serde_json::from_slice(&self.collect(response, "application/json", 4 * MIB)?)
                    .map_err(|_| Error::Response)?;
            if !opaque(&page.snapshot) || page.hits.len() > k {
                return Err(Error::Response);
            }
            let mut seen = BTreeSet::new();
            for hit in &page.hits {
                if !valid_row(&hit.journal, &hit.record, s)
                    || !seen.insert((&hit.journal, &hit.record.id))
                    || !hit.record.record.indexed
                    || !hit.score_bits.is_finite()
                    || hit.excerpt.len() > 320
                    || hit.excerpt_end < hit.excerpt_start
                    || hit.excerpt_end as u64 > hit.record.text_bytes
                    || hit.excerpt.len() != hit.excerpt_end - hit.excerpt_start
                {
                    return Err(Error::Response);
                }
            }
            Ok(page)
        })();
        result.map_err(|e| {
            Error::SearchUnknown(if let Error::Http(status) = e {
                Some(status)
            } else {
                None
            })
        })
    }
    pub fn content(&self, journal: &str, record: &str) -> Result<String> {
        if !opaque(journal) || !opaque(record) {
            return Err(Error::Request);
        }
        let response = self
            .client
            .get(format!(
                "{}journals/{journal}/records/{record}/content",
                self.base
            ))
            .header(AUTHORIZATION, self.auth.clone())
            .header("accept-encoding", "identity")
            .send()
            .map_err(|_| Error::Transport)?;
        String::from_utf8(self.collect(response, "text/plain", MAX_TEXT_BYTES)?)
            .map_err(|_| Error::Response)
    }
}
