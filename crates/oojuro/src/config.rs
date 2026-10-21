//! Settings (`docs/DESIGN.md`, "Settings").
//!
//! Files: the global `~/.oojuro/config.toml`, a per-project `.oojuro.toml`
//! (the nearest one at or above the working directory), and per-agent
//! sections `[agents."<writer>"]` in either (a section name may use `*`,
//! e.g. `[agents."codex:*"]`).
//!
//! Precedence, lowest first: defaults, global top level, project top level,
//! global agent sections, project agent sections (within one file, `*`
//! sections before the exact one). The last value wins, except `exclude`,
//! which is the union of every layer (a project can add exclusions, never
//! drop the global ones). A relative `exclude` pattern with a `/` in a
//! project file is relative to that project's directory.
//!
//! `paused` (set by `oojuro pause` / `resume`) is a global switch read from
//! the global file's top level only, and so is the `[proxy]` table (the
//! model API proxy, [`proxy_settings`]).
use crate::Result;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Longest query any setting may allow (the engine is opened with this cap;
/// the configured `max_query_bytes` is enforced per call).
pub const HARD_MAX_QUERY_BYTES: usize = 16 << 20;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Settings {
    /// Capture what the agent sees, thinks and does.
    pub record: bool,
    /// Search before each turn and add the results to context (off in v1).
    pub inject: bool,
    /// Maximum results injected per turn.
    pub top_k: u32,
    /// Cap on injected text.
    pub budget_tokens: u32,
    /// Inject only records that save at least this many bits.
    pub min_bits: f64,
    /// Recency weight in bits per turn (0 = off).
    pub recency: f64,
    pub record_thinking: bool,
    pub index_thinking: bool,
    /// Path globs never recorded.
    pub exclude: Vec<String>,
    /// Longest accepted search query in bytes.
    pub max_query_bytes: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            record: true,
            inject: false,
            top_k: 20,
            budget_tokens: 2500,
            min_bits: 8.0,
            recency: 1.0,
            record_thinking: true,
            index_thinking: true,
            exclude: Vec::new(),
            max_query_bytes: 65_536,
        }
    }
}

pub const KEYS: [&str; 10] = [
    "record",
    "inject",
    "top_k",
    "budget_tokens",
    "min_bits",
    "recency",
    "record_thinking",
    "index_thinking",
    "exclude",
    "max_query_bytes",
];

/// The global-only switch written by `oojuro pause` / `resume`.
pub const PAUSED: &str = "paused";
/// The OpenTelemetry receiver's section (global only; see `crate::otel`).
pub const OTEL: &str = "otel";

/// The global-only table of the model API proxy.
pub const PROXY: &str = "proxy";

/// One parsed settings file.
#[derive(Clone, Debug)]
pub struct File {
    pub path: PathBuf,
    /// A project file (`.oojuro.toml`): relative patterns resolve against its directory.
    pub project: bool,
    pub table: toml::Table,
}

impl File {
    /// `None` when the file does not exist.
    pub fn load(path: &Path, project: bool) -> Result<Option<File>> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display()).into()),
        };
        let table: toml::Table = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some(File { path: path.to_path_buf(), project, table }))
    }

    fn dir(&self) -> Option<&Path> {
        self.project.then(|| self.path.parent()).flatten()
    }

    /// Agent sections that apply to `writer`: `*` patterns first (file
    /// order), then the exact name.
    fn agent_sections(&self, writer: &str) -> Vec<(String, &toml::Table)> {
        let Some(toml::Value::Table(agents)) = self.table.get("agents") else { return Vec::new() };
        let mut out: Vec<(String, &toml::Table)> = agents
            .iter()
            .filter(|(name, _)| name.contains('*') && wildcard(name, writer))
            .filter_map(|(name, v)| v.as_table().map(|t| (name.clone(), t)))
            .collect();
        if let Some(toml::Value::Table(t)) = agents.get(writer) {
            out.push((writer.to_string(), t));
        }
        out
    }
}

/// `*` matches any run of characters, `?` one character.
pub fn wildcard(pattern: &str, s: &str) -> bool {
    let (p, s): (Vec<char>, Vec<char>) = (pattern.chars().collect(), s.chars().collect());
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Settings files in effect: the global file and (optionally) one project file.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub global: Option<File>,
    pub project: Option<File>,
}

/// Effective settings for one writer, with where each value came from.
#[derive(Clone, Debug, Serialize)]
pub struct Resolved {
    pub settings: Settings,
    pub paused: bool,
    /// key -> `default` or `<file>` or `<file> [agents."<name>"]`.
    pub sources: BTreeMap<String, String>,
    /// Unknown keys and misplaced switches (ignored).
    pub warnings: Vec<String>,
}

impl Config {
    pub fn load(global: &Path, project: Option<&Path>) -> Result<Self> {
        Ok(Self {
            global: File::load(global, false)?,
            project: project.map(|p| File::load(p, true)).transpose()?.flatten(),
        })
    }

    /// The global file and the nearest project file at or above `dir`.
    pub fn discover(global: &Path, dir: &Path) -> Result<Self> {
        Self::load(global, crate::paths::find_project_config(dir).as_deref())
    }

    pub fn resolve(&self, writer: Option<&str>) -> Result<Resolved> {
        let mut r = Resolved {
            settings: Settings::default(),
            paused: false,
            sources: KEYS.iter().map(|k| (k.to_string(), "default".to_string())).collect(),
            warnings: Vec::new(),
        };
        let files: Vec<&File> = [&self.global, &self.project].into_iter().flatten().collect();
        for f in &files {
            apply(&mut r, &f.table, &f.path.display().to_string(), f.dir(), true)?;
        }
        if let Some(w) = writer {
            for f in &files {
                for (name, t) in f.agent_sections(w) {
                    apply(&mut r, t, &format!("{} [agents.\"{name}\"]", f.path.display()), f.dir(), false)?;
                }
            }
        }
        if let Some(g) = &self.global {
            match g.table.get(PAUSED) {
                Some(toml::Value::Boolean(b)) => r.paused = *b,
                Some(_) => return Err(format!("{}: `paused` must be true or false", g.path.display()).into()),
                None => {}
            }
        }
        if let Some(p) = &self.project {
            for k in [PAUSED, PROXY, OTEL] {
                if p.table.contains_key(k) {
                    r.warnings.push(format!("{}: `{k}` is global only (ignored)", p.path.display()));
                }
            }
        }
        Ok(r)
    }
}

fn apply(r: &mut Resolved, t: &toml::Table, source: &str, project_dir: Option<&Path>, top: bool) -> Result<()> {
    for (k, v) in t {
        if top && (k == "agents" || k == PAUSED || k == PROXY || k == OTEL) {
            continue;
        }
        if !KEYS.contains(&k.as_str()) {
            r.warnings.push(format!("{source}: unknown setting `{k}` (ignored)"));
            continue;
        }
        check_value(k, v).map_err(|e| format!("{source}: {e}"))?;
        let s = &mut r.settings;
        match k.as_str() {
            "record" => s.record = v.as_bool().unwrap(),
            "inject" => s.inject = v.as_bool().unwrap(),
            "record_thinking" => s.record_thinking = v.as_bool().unwrap(),
            "index_thinking" => s.index_thinking = v.as_bool().unwrap(),
            "top_k" => s.top_k = v.as_integer().unwrap() as u32,
            "budget_tokens" => s.budget_tokens = v.as_integer().unwrap() as u32,
            "max_query_bytes" => s.max_query_bytes = v.as_integer().unwrap() as u64,
            "min_bits" => s.min_bits = number(v).unwrap(),
            "recency" => s.recency = number(v).unwrap(),
            "exclude" => {
                for p in v.as_array().unwrap() {
                    let p = p.as_str().unwrap();
                    let p = match project_dir {
                        Some(d) if p.contains('/') && !p.starts_with('/') && !p.starts_with("~/") => {
                            d.join(p.trim_start_matches("./")).to_string_lossy().into_owned()
                        }
                        _ => p.to_string(),
                    };
                    if !s.exclude.contains(&p) {
                        s.exclude.push(p);
                    }
                }
                let e = r.sources.get_mut("exclude").unwrap();
                *e = if e == "default" { source.to_string() } else { format!("{e} + {source}") };
                continue;
            }
            _ => unreachable!(),
        }
        r.sources.insert(k.clone(), source.to_string());
    }
    Ok(())
}

fn number(v: &toml::Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// Type and range of one setting.
pub fn check_value(key: &str, v: &toml::Value) -> Result<()> {
    let int = |lo: i64, hi: i64| match v.as_integer() {
        Some(i) if (lo..=hi).contains(&i) => Ok(()),
        _ => Err(format!("`{key}` must be an integer in {lo}..={hi}")),
    };
    let r = match key {
        "record" | "inject" | "record_thinking" | "index_thinking" | PAUSED => {
            v.as_bool().map(|_| ()).ok_or_else(|| format!("`{key}` must be true or false"))
        }
        "top_k" => int(1, 10_000),
        "budget_tokens" => int(0, 10_000_000),
        "max_query_bytes" => int(1, HARD_MAX_QUERY_BYTES as i64),
        "min_bits" | "recency" => match number(v) {
            Some(x) if x.is_finite() && x >= 0.0 => Ok(()),
            _ => Err(format!("`{key}` must be a number >= 0")),
        },
        "exclude" => match v.as_array() {
            Some(a) if a.iter().all(|x| x.as_str().is_some_and(|s| !s.is_empty())) => Ok(()),
            _ => Err(format!("`{key}` must be a list of path globs")),
        },
        _ => Err(format!("unknown setting `{key}` (known: {})", KEYS.join(", "))),
    };
    r.map_err(Into::into)
}

/// A value typed on the command line: `true`/`false`/`on`/`off`, numbers,
/// and for `exclude` a TOML list (`["*.env", "~/secret/**"]`) or
/// comma-separated globs.
pub fn parse_cli_value(key: &str, raw: &str) -> Result<toml::Value> {
    let raw = raw.trim();
    let v = match key {
        "record" | "inject" | "record_thinking" | "index_thinking" | PAUSED => {
            match raw.to_ascii_lowercase().as_str() {
                "true" | "on" | "yes" | "1" => toml::Value::Boolean(true),
                "false" | "off" | "no" | "0" => toml::Value::Boolean(false),
                _ => return Err(format!("`{key}` takes true/false (or on/off)").into()),
            }
        }
        "top_k" | "budget_tokens" | "max_query_bytes" => {
            toml::Value::Integer(raw.replace('_', "").parse().map_err(|_| format!("`{key}` takes an integer"))?)
        }
        "min_bits" | "recency" => toml::Value::Float(raw.parse().map_err(|_| format!("`{key}` takes a number"))?),
        "exclude" => {
            if raw.starts_with('[') {
                let t: toml::Table = toml::from_str(&format!("v = {raw}")).map_err(|e| format!("bad list: {e}"))?;
                t["v"].clone()
            } else {
                toml::Value::Array(
                    raw.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|s| toml::Value::String(s.into()))
                        .collect(),
                )
            }
        }
        _ => return Err(format!("unknown setting `{key}` (known: {})", KEYS.join(", ")).into()),
    };
    check_value(key, &v)?;
    Ok(v)
}

/// The model API proxy's settings: the `[proxy]` table of the global file.
///
/// ```toml
/// [proxy]
/// enabled = true                      # run it inside `oojurod tail`
/// listen = "127.0.0.1:8790"           # loopback only
/// writer = "my-agent"                 # optional: writer for calls without x-oojuro-writer
/// anthropic_upstream = "https://api.anthropic.com"
/// openai_upstream = "https://api.openai.com"
/// record_tailed = false              # true: also record agents whose transcripts are tailed
/// # seen_per_writer = 100000, queue_jobs = 1024, queue_mb = 512, max_body_mb = 64
///
/// [[proxy.listeners]]                 # more listeners, each with its own writer/upstreams
/// listen = "127.0.0.1:8791"
/// writer = "openrouter-bot"
/// openai_upstream = "https://openrouter.ai/api"
/// ```
#[derive(Clone, Debug)]
pub struct ProxySettings {
    /// Run the proxy inside `oojurod tail`.
    pub enabled: bool,
    pub config: oojuro_proxy::Config,
}

/// File name of the proxy's seen-set (message hashes, no text) in the
/// journal directory: it says which messages that journal holds.
pub const PROXY_SEEN: &str = "proxy-seen.bin";

/// Read `[proxy]` from the global settings file (defaults when absent) for
/// the journal at `store`. Unknown keys and bad values are errors: the
/// proxy is an explicit opt-in.
pub fn proxy_settings(global: &Path, store: &Path) -> Result<ProxySettings> {
    let mut cfg = oojuro_proxy::Config { seen_path: Some(store.join(PROXY_SEEN)), ..Default::default() };
    let file = File::load(global, false)?;
    let Some(v) = file.as_ref().and_then(|f| f.table.get(PROXY)) else {
        return Ok(ProxySettings { enabled: false, config: cfg });
    };
    let at = |k: &str| format!("{}: [proxy] {k}", global.display());
    let t = v.as_table().ok_or_else(|| format!("{}: `proxy` must be a table", global.display()))?;
    let mut enabled = false;
    let mut main = oojuro_proxy::Listener::default();
    let int = |k: &str, v: &toml::Value, lo: i64, hi: i64| -> Result<usize> {
        match v.as_integer() {
            Some(i) if (lo..=hi).contains(&i) => Ok(i as usize),
            _ => Err(format!("{} must be an integer in {lo}..={hi}", at(k)).into()),
        }
    };
    let string = |k: &str, v: &toml::Value| -> Result<String> {
        v.as_str().map(str::to_string).ok_or_else(|| format!("{} must be a string", at(k)).into())
    };
    for (k, v) in t {
        match k.as_str() {
            "enabled" => enabled = v.as_bool().ok_or_else(|| format!("{} must be true or false", at(k)))?,
            "listeners" => {} // below, once the defaults are known
            "seen_per_writer" => cfg.seen_per_writer = int(k, v, 1000, 10_000_000)?,
            "queue_jobs" => cfg.queue_jobs = int(k, v, 1, 100_000)?,
            "queue_mb" => cfg.queue_bytes = int(k, v, 1, 16_384)? << 20,
            "max_body_mb" => cfg.max_body_bytes = int(k, v, 1, 1024)? << 20,
            "record_tailed" => {
                cfg.record_tailed = v.as_bool().ok_or_else(|| format!("{} must be true or false", at(k)))?
            }
            _ => listener_key(&mut main, k, v, &at, &string)?,
        }
    }
    let mut listeners = vec![main.clone()];
    if let Some(ls) = t.get("listeners") {
        let ls = ls.as_array().ok_or_else(|| format!("{} must be a list of tables", at("listeners")))?;
        for l in ls {
            let lt = l.as_table().ok_or_else(|| format!("{} must be a list of tables", at("listeners")))?;
            if !lt.contains_key("listen") {
                return Err(format!("{} needs `listen`", at("listeners")).into());
            }
            let mut x = oojuro_proxy::Listener { writer: None, ..main.clone() };
            for (k, v) in lt {
                listener_key(&mut x, k, v, &at, &string)?;
            }
            listeners.push(x);
        }
    }
    for l in &listeners {
        if !l.listen.ip().is_loopback() {
            return Err(
                format!("{}: {} is not a loopback address (127.0.0.1 or ::1 only)", at("listen"), l.listen).into()
            );
        }
        for u in [&l.anthropic_upstream, &l.openai_upstream] {
            oojuro_proxy::forward::Upstream::parse(u).map_err(|e| format!("{}: {e}", at("upstream")))?;
        }
    }
    cfg.listeners = listeners;
    Ok(ProxySettings { enabled, config: cfg })
}

fn listener_key(
    l: &mut oojuro_proxy::Listener,
    k: &str,
    v: &toml::Value,
    at: &dyn Fn(&str) -> String,
    string: &dyn Fn(&str, &toml::Value) -> Result<String>,
) -> Result<()> {
    match k {
        "listen" => l.listen = parse_listen(&string(k, v)?).map_err(|e| format!("{}: {e}", at(k)))?,
        "writer" => {
            let w = string(k, v)?;
            if !w.is_empty() {
                crate::paths::check_writer(&w)?;
            }
            l.writer = (!w.is_empty()).then_some(w);
        }
        "anthropic_upstream" => l.anthropic_upstream = string(k, v)?,
        "openai_upstream" => l.openai_upstream = string(k, v)?,
        _ => {
            return Err(format!(
                "{}: unknown key (known: enabled, listen, writer, anthropic_upstream, openai_upstream, listeners, record_tailed, seen_per_writer, queue_jobs, queue_mb, max_body_mb)",
                at(k)
            )
            .into());
        }
    }
    Ok(())
}

/// `127.0.0.1:8790`, `[::1]:8790`, `localhost:8790` or a bare port (on 127.0.0.1).
pub fn parse_listen(s: &str) -> Result<std::net::SocketAddr> {
    let s = s.trim();
    if let Ok(p) = s.parse::<u16>() {
        return Ok(std::net::SocketAddr::from(([127, 0, 0, 1], p)));
    }
    if let Some(p) = s.strip_prefix("localhost:") {
        return Ok(std::net::SocketAddr::from(([127, 0, 0, 1], p.parse::<u16>()?)));
    }
    Ok(s.parse()?)
}

fn table_at<'a>(t: &'a toml::Table, section: Option<&str>) -> Option<&'a toml::Table> {
    match section {
        None => Some(t),
        Some(w) => t.get("agents")?.as_table()?.get(w)?.as_table(),
    }
}

/// Set `key = value` in a settings file, at the top level or in
/// `[agents."<section>"]`, keeping the rest of the file (comments
/// included) as it is. The file is replaced atomically.
pub fn set_value(path: &Path, section: Option<&str>, key: &str, value: &toml::Value) -> Result<()> {
    check_value(key, value)?;
    if key == PAUSED && section.is_some() {
        return Err("`paused` is a global switch (no agent section)".into());
    }
    let old = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let old_table: toml::Table =
        toml::from_str(&old).map_err(|e| format!("{} does not parse; fix it first: {e}", path.display()))?;
    // what the file must read as afterwards
    let mut want = old_table.clone();
    {
        let mut t = &mut want;
        if let Some(w) = section {
            let agents = t.entry("agents").or_insert_with(|| toml::Value::Table(toml::Table::new()));
            let agents = agents.as_table_mut().ok_or("`agents` in the settings file is not a table")?;
            let sec = agents.entry(w).or_insert_with(|| toml::Value::Table(toml::Table::new()));
            t = sec.as_table_mut().ok_or("agent section is not a table")?;
        }
        t.insert(key.to_string(), value.clone());
    }
    let edited = edit_lines(&old, section, key, value)
        .filter(|text| toml::from_str::<toml::Table>(text).is_ok_and(|t| t == want));
    let text = match edited {
        Some(t) => t,
        None => toml::to_string(&want)?, // keeps every value, not the comments
    };
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
        && !dir.exists()
    {
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text.as_bytes())?;
    fs::File::open(&tmp)?.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Line-level edit: replace the key's line in its section, or add it at the
/// end of the section, or add the section. `None` when the file's layout is
/// not simple enough (the caller then rewrites it).
fn edit_lines(old: &str, section: Option<&str>, key: &str, value: &toml::Value) -> Option<String> {
    let lines: Vec<&str> = old.split_inclusive('\n').collect();
    let is_header = |l: &str| {
        let t = l.trim_start();
        t.starts_with('[')
    };
    let header_is = |l: &str, w: &str| -> bool {
        let probe = format!("{}\n__oojuro_probe = 1\n", l.trim_end());
        toml::from_str::<toml::Table>(&probe)
            .ok()
            .is_some_and(|t| table_at(&t, Some(w)).is_some_and(|s| s.contains_key("__oojuro_probe")))
    };
    let (start, end) = match section {
        None => (0, lines.iter().position(|l| is_header(l)).unwrap_or(lines.len())),
        Some(w) => match lines.iter().position(|l| is_header(l) && header_is(l, w)) {
            Some(h) => (h + 1, lines[h + 1..].iter().position(|l| is_header(l)).map_or(lines.len(), |p| h + 1 + p)),
            None => {
                let mut out = old.to_string();
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&format!("[agents.{}]\n{key} = {value}\n", toml::Value::String(w.to_string())));
                return Some(out);
            }
        },
    };
    let key_re = regex::Regex::new(&format!(r#"^\s*(?:{k}|"{k}")\s*="#, k = regex::escape(key))).ok()?;
    let new_line = format!("{key} = {value}\n");
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    match (start..end).find(|&i| key_re.is_match(lines[i])) {
        Some(i) => out[i] = new_line,
        None => {
            // after the section's last non-blank line
            let at = (start..end).rev().find(|&i| !lines[i].trim().is_empty()).map_or(start, |i| i + 1);
            if at > 0 && !out[at - 1].ends_with('\n') {
                out[at - 1].push('\n');
            }
            out.insert(at, new_line);
        }
    }
    Some(out.concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard("codex:*", "codex:-home-x"));
        assert!(wildcard("*", ""));
        assert!(wildcard("claude-code:*-repo", "claude-code:-home-x-repo"));
        assert!(!wildcard("codex:*", "claude-code:x"));
        assert!(wildcard("h?man:*", "human:me"));
    }

    #[test]
    fn line_edits_keep_comments() {
        let old = "# my settings\nrecord = true # keep\n\n[agents.\"codex:x\"]\n# codex is noisy\ntop_k = 5\n";
        let e = edit_lines(old, None, "inject", &toml::Value::Boolean(true)).unwrap();
        assert_eq!(
            e,
            "# my settings\nrecord = true # keep\ninject = true\n\n[agents.\"codex:x\"]\n# codex is noisy\ntop_k = 5\n"
        );
        let e = edit_lines(old, Some("codex:x"), "top_k", &toml::Value::Integer(7)).unwrap();
        assert!(e.contains("# codex is noisy\ntop_k = 7\n") && e.starts_with("# my settings\n"));
        let e = edit_lines(old, Some("human:me"), "record", &toml::Value::Boolean(false)).unwrap();
        assert!(e.ends_with("top_k = 5\n\n[agents.\"human:me\"]\nrecord = false\n"));
        let e = edit_lines("", None, "inject", &toml::Value::Boolean(false)).unwrap();
        assert_eq!(e, "inject = false\n");
    }

    #[test]
    fn proxy_table() {
        let base = std::env::var_os("TMPDIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("oojuro-proxy-cfg-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let g = d.join("config.toml");
        fs::write(&g, "record = true\n").unwrap();
        let p = proxy_settings(&g, &d).unwrap();
        assert!(!p.enabled && !p.config.record_tailed);
        assert_eq!(p.config.listeners[0].listen.to_string(), "127.0.0.1:8790");
        fs::write(
            &g,
            "[proxy]\nenabled = true\nrecord_tailed = true\nlisten = \"localhost:9001\"\nwriter = \"w1\"\nopenai_upstream = \"http://127.0.0.1:1/x\"\n\n[[proxy.listeners]]\nlisten = \"9002\"\n",
        )
        .unwrap();
        let p = proxy_settings(&g, &d).unwrap();
        assert!(p.enabled && p.config.record_tailed);
        let l = &p.config.listeners;
        assert_eq!((l[0].listen.port(), l[0].writer.as_deref()), (9001, Some("w1")));
        assert_eq!((l[1].listen.port(), l[1].writer.as_deref()), (9002, None));
        assert_eq!(l[1].openai_upstream, "http://127.0.0.1:1/x");
        assert_eq!(p.config.seen_path, Some(d.join(PROXY_SEEN)));
        // the table is not a recording setting
        let r = Config::load(&g, None).unwrap().resolve(None).unwrap();
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        for bad in
            ["[proxy]\nlisten = \"0.0.0.0:1\"\n", "[proxy]\nbogus = 1\n", "[proxy]\nanthropic_upstream = \"ftp://x\"\n"]
        {
            fs::write(&g, bad).unwrap();
            assert!(proxy_settings(&g, &d).is_err(), "{bad}");
        }
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn cli_values() {
        assert_eq!(parse_cli_value("inject", "on").unwrap(), toml::Value::Boolean(true));
        assert_eq!(parse_cli_value("top_k", "40").unwrap(), toml::Value::Integer(40));
        assert!(parse_cli_value("top_k", "0").is_err());
        assert!(parse_cli_value("max_query_bytes", "999999999").is_err());
        assert_eq!(
            parse_cli_value("exclude", "*.env, ~/secret/**").unwrap(),
            toml::Value::Array(vec![toml::Value::String("*.env".into()), toml::Value::String("~/secret/**".into())])
        );
        assert_eq!(
            parse_cli_value("exclude", r#"["a/*.pem"]"#).unwrap(),
            toml::Value::Array(vec![toml::Value::String("a/*.pem".into())])
        );
        assert!(parse_cli_value("bogus", "1").is_err());
    }
}
