//! Explicit version-2 API-key setup and durable provider-session routing.
use super::*;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiConnection {
    version: u32,
    base_url: String,
    integration_key: String,
    label: String,
}
#[derive(Serialize)]
pub struct ApiConnectionInfo {
    pub journal_mode: &'static str,
    pub local_key_saved: bool,
}
fn label(value: &str) -> Result<()> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > 128
        || value.chars().any(char::is_control)
    {
        return Err("invalid_session_label");
    }
    Ok(())
}
fn normalized(cfg: ApiConnection) -> Result<Connection> {
    if cfg.version != 2 || origin(&cfg.base_url)? != cfg.base_url {
        return Err("invalid_connection");
    }
    label(&cfg.label)?;
    let token = cfg
        .integration_key
        .strip_prefix("ojc_")
        .ok_or("invalid_connection")?;
    if token.len() != 108 || token.as_bytes()[64] != b'_' || !opaque(&token[..64]) {
        return Err("invalid_connection");
    }
    let source = &token[..64];
    let raw = &token[65..];
    let secret = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| "invalid_connection")?;
    if secret.len() != 32 || URL_SAFE_NO_PAD.encode(&secret) != raw {
        return Err("invalid_connection");
    }
    let namespace = format!("v3-{source}");
    let mut hash = blake3::Hasher::new();
    hash.update(b"oojuro-prefix-credential-v1");
    hash.update(&(namespace.len() as u64).to_le_bytes());
    hash.update(namespace.as_bytes());
    hash.update(&secret);
    Ok(Connection {
        version: 2,
        base_url: cfg.base_url,
        source: source.into(),
        journal: String::new(),
        capture_key: cfg.integration_key,
        credential_id: hash.finalize().to_hex().to_string(),
        label: cfg.label,
        until: 0,
    })
}
pub(super) fn decode(bytes: &[u8]) -> Result<Connection> {
    normalized(serde_json::from_slice(bytes).map_err(|_| "invalid_connection")?)
}
/// The key is read only from a protected file. No network, browser approval, or key output.
pub fn connect_api(
    path: &Path,
    base: &str,
    key_file: &Path,
    name: &str,
) -> Result<ApiConnectionInfo> {
    label(name)?;
    let bytes = read_private(key_file, 4096).map_err(|_| "private_api_key_file_required")?;
    let key = std::str::from_utf8(&bytes)
        .map_err(|_| "invalid_connection")?
        .trim();
    let cfg = ApiConnection {
        version: 2,
        base_url: origin(base)?,
        integration_key: key.into(),
        label: name.into(),
    };
    normalized(ApiConnection {
        version: cfg.version,
        base_url: cfg.base_url.clone(),
        integration_key: cfg.integration_key.clone(),
        label: cfg.label.clone(),
    })?;
    create_private(
        path,
        &serde_json::to_vec(&cfg).map_err(|_| "invalid_connection")?,
    )?;
    Ok(ApiConnectionInfo {
        journal_mode: "session",
        local_key_saved: true,
    })
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Route {
    format: u32,
    pub journal: String,
    source: String,
    agent_id: String,
    run_id: String,
    project: Option<String>,
    pub events_url: String,
}
impl Route {
    pub(super) fn validate(
        &self,
        client: &CaptureClient,
        info: &Discovery,
        run: &str,
    ) -> Result<()> {
        if self.format != 1
            || self.source != client.cfg.source
            || self.agent_id != info.agent_id
            || self.run_id != run
            || !opaque(&self.journal)
            || self.project.as_ref().is_some_and(|id| !opaque(id))
            || self.events_url != format!("/api/capture/journals/{}/events", self.journal)
        {
            return Err("invalid_session_route");
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Session {
    pub label: String,
    pub route: Option<Route>,
}
impl CaptureClient {
    pub(super) fn provision(&self, info: &Discovery, run: &str, name: &str) -> Result<Route> {
        if self.cfg.version != 2 || !opaque(run) {
            return Err("invalid_session_route");
        }
        label(name)?;
        let bytes = serde_json::to_vec(&json!({"format":1,"run_id":run,"label":name}))
            .map_err(|_| "invalid_session_route")?;
        let reply = self
            .http
            .post(format!("{}/api/capture/sessions", self.cfg.base_url))
            .header(AUTHORIZATION, self.auth.clone())
            .header("content-type", "application/json")
            .body(bytes)
            .send()
            .map_err(|_| "session_provision_outcome_unknown")?;
        let bytes = response(reply).map_err(|_| "session_provision_outcome_unknown")?;
        if bytes.len() > 16384 {
            return Err("invalid_session_route");
        }
        let route: Route = serde_json::from_slice(&bytes).map_err(|_| "invalid_session_route")?;
        route.validate(self, info, run)?;
        Ok(route)
    }
}
/// Bounded header inspection identifies a session; it never imports old transcript records.
/// Filename/path identity is the conservative fallback when a provider header is unavailable.
pub(super) fn run(path: &Path, format: Format, agent: &str) -> Result<String> {
    let file = transcript(path)?;
    let mut first = Vec::new();
    std::io::BufRead::read_until(
        &mut std::io::BufReader::new(file.take(65537)),
        b'\n',
        &mut first,
    )
    .map_err(|_| "capture_source_io")?;
    let value = if first.len() <= 65536 {
        serde_json::from_slice::<serde_json::Value>(&first).ok()
    } else {
        None
    };
    let provider = value
        .as_ref()
        .and_then(|value| match format {
            Format::Codex if value["type"] == "session_meta" => value["payload"]["id"].as_str(),
            Format::ClaudeCode => value["sessionId"].as_str(),
            _ => None,
        })
        .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control));
    let fallback = path.to_string_lossy();
    // Codex filenames retain the provider UUID across resume/compaction rollouts.
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let uuid = stem.get(stem.len().saturating_sub(36)..).filter(|value| {
        value.len() == 36
            && value.bytes().enumerate().all(|(i, c)| {
                if [8, 13, 18, 23].contains(&i) {
                    c == b'-'
                } else {
                    c.is_ascii_hexdigit()
                }
            })
    });
    let identity = provider.or(uuid).unwrap_or(&fallback);
    // Claude sidechain files can report the parent's sessionId. Bind their own stable agent filename too.
    let sidechain = if format == Format::ClaudeCode
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "subagents")
    {
        Some(stem)
    } else {
        None
    };
    let mut hash = blake3::Hasher::new();
    hash.update(b"oojuro-hosted-provider-session-v1");
    for value in [agent, format.name(), identity] {
        hash.update(&(value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    if let Some(agent) = sidechain {
        hash.update(&(agent.len() as u64).to_le_bytes());
        hash.update(agent.as_bytes());
    }
    Ok(hash.finalize().to_hex().to_string())
}
pub(super) fn name(base: &str, adapter: &str, run: &str) -> String {
    let suffix = format!(" / {adapter} / {}", &run[..8]);
    let mut prefix = base.to_string();
    while prefix.len() + suffix.len() > 128 {
        prefix.pop();
    }
    format!("{}{suffix}", prefix.trim_end())
}
