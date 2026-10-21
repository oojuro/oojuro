//! Where things live, and who is writing.
//!
//! Everything sits under one home directory, `$OOJURO_HOME` (default
//! `~/.oojuro`, mode 700): the journal `journal/`, the recorder's socket
//! `oojurod.sock` (mode 600) and the settings `config.toml`. Each can be
//! overridden on its own (`--store`, `--socket`, `--config`).
use crate::Result;
use std::path::{Path, PathBuf};

pub const ENV_HOME: &str = "OOJURO_HOME";
pub const ENV_WRITER: &str = "OOJURO_WRITER";
pub const SOCKET_NAME: &str = "oojurod.sock";
pub const STORE_NAME: &str = "journal";
pub const CONFIG_NAME: &str = "config.toml";
pub const PROJECT_CONFIG: &str = ".oojuro.toml";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
    pub store: PathBuf,
    pub socket: PathBuf,
    pub config: PathBuf,
}

/// The user's home directory.
pub fn user_home() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?))
}

impl Paths {
    /// `home` (else `$OOJURO_HOME`, else `~/.oojuro`) and the parts inside
    /// it, each replaced when given. The socket sits next to the journal
    /// directory: with `--store DIR` and no `--socket` it is
    /// `DIR/../oojurod.sock` (so the default store's socket is
    /// `~/.oojuro/oojurod.sock` either way).
    pub fn resolve(
        home: Option<PathBuf>,
        store: Option<PathBuf>,
        socket: Option<PathBuf>,
        config: Option<PathBuf>,
    ) -> Result<Self> {
        let home = match home.or_else(|| std::env::var_os(ENV_HOME).map(PathBuf::from)) {
            Some(h) => h,
            None => user_home()?.join(".oojuro"),
        };
        let socket = socket.unwrap_or_else(|| match store.as_deref().map(std::path::absolute) {
            Some(Ok(s)) => s.parent().map_or_else(|| home.join(SOCKET_NAME), |p| p.join(SOCKET_NAME)),
            _ => home.join(SOCKET_NAME),
        });
        Ok(Self {
            store: store.unwrap_or_else(|| home.join(STORE_NAME)),
            socket,
            config: config.unwrap_or_else(|| home.join(CONFIG_NAME)),
            home,
        })
    }

    /// Everything inside one directory (tests, demos).
    pub fn under(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            store: home.join(STORE_NAME),
            socket: home.join(SOCKET_NAME),
            config: home.join(CONFIG_NAME),
        }
    }
}

/// The login name (`$USER`, `$LOGNAME`, else the numeric uid).
pub fn os_user() -> String {
    for k in ["USER", "LOGNAME"] {
        if let Ok(u) = std::env::var(k)
            && !u.is_empty()
        {
            return u;
        }
    }
    // SAFETY: getuid has no failure mode.
    format!("uid{}", unsafe { libc::getuid() })
}

/// Writer ID for a person using the CLI by hand.
pub fn human_writer() -> String {
    format!("human:{}", os_user())
}

/// The writer ID the recorder gives an agent running in `cwd`: Claude Code
/// sets `CLAUDECODE=1` for the processes it starts, and its project key is
/// the launch directory encoded like `oojuro_capture::encode_dir`.
pub fn detected_agent_writer() -> Option<String> {
    if std::env::var("CLAUDECODE").ok().as_deref() == Some("1") {
        let cwd = std::env::current_dir().ok()?;
        return Some(format!("claude-code:{}", oojuro_capture::encode_dir(&cwd.to_string_lossy())));
    }
    None
}

/// Writer for an explicit call: the given one, else `$OOJURO_WRITER`, else
/// (when `agent` is set: an agent's MCP server) the recorder's ID for the
/// detected agent, else `human:<os user>`.
pub fn writer(explicit: Option<&str>, agent: bool) -> String {
    if let Some(w) = explicit.filter(|w| !w.is_empty()) {
        return w.to_string();
    }
    if let Ok(w) = std::env::var(ENV_WRITER)
        && !w.is_empty()
    {
        return w;
    }
    if agent && let Some(w) = detected_agent_writer() {
        return w;
    }
    human_writer()
}

/// A writer ID must be printable, without control characters, at most 256 bytes.
pub fn check_writer(w: &str) -> Result<()> {
    if w.is_empty() || w.len() > 256 || w.chars().any(char::is_control) {
        return Err(format!("invalid writer ID {w:?} (1-256 bytes, no control characters)").into());
    }
    Ok(())
}

/// The nearest `.oojuro.toml` at or above `dir`.
pub fn find_project_config(dir: &Path) -> Option<PathBuf> {
    let mut d = Some(dir);
    while let Some(x) = d {
        let p = x.join(PROJECT_CONFIG);
        if p.is_file() {
            return Some(p);
        }
        d = x.parent();
    }
    None
}
