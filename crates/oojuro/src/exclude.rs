//! `exclude`: path globs never recorded.
//!
//! A pattern without `/` matches a file or directory name at any depth
//! (`.env`, `*.pem`, `secrets`). A pattern with `/` matches a whole path:
//! absolute (`/srv/keys/**`), under the home directory (`~/private/**`),
//! or, in the global file, at any depth (`config/*.key`). A path is
//! excluded when it or any directory above it matches. `**` crosses
//! directories, `*` and `?` do not, `[...]` is a character class.
use crate::Result;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct Exclude {
    names: Vec<regex::Regex>,
    paths: Vec<regex::Regex>,
    pub patterns: Vec<String>,
}

fn glob_regex(glob: &str) -> String {
    let mut re = String::new();
    let cs: Vec<char> = glob.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '*' if cs.get(i + 1) == Some(&'*') => {
                if cs.get(i + 2) == Some(&'/') {
                    re.push_str("(?:.*/)?");
                    i += 3;
                } else {
                    re.push_str(".*");
                    i += 2;
                }
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '[' => match cs[i + 1..].iter().position(|c| *c == ']') {
                Some(off) if off > 0 => {
                    let body: String = cs[i + 1..i + 1 + off].iter().collect();
                    let body = body.strip_prefix('!').map(|b| format!("^{b}")).unwrap_or(body);
                    re.push('[');
                    re.push_str(&body.replace('\\', "\\\\"));
                    re.push(']');
                    i += off + 2;
                    continue;
                }
                _ => re.push_str("\\["),
            },
            c => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    re
}

impl Exclude {
    pub fn new(patterns: &[String], home: Option<&Path>) -> Result<Self> {
        let mut x = Exclude { patterns: patterns.to_vec(), ..Self::default() };
        for p in patterns {
            let p = p.trim();
            if p.is_empty() {
                continue;
            }
            let p = p.trim_end_matches('/');
            let expanded = match (p.strip_prefix("~/"), home) {
                (Some(rest), Some(h)) => format!("{}/{rest}", h.display()),
                _ => p.to_string(),
            };
            if !expanded.contains('/') {
                x.names.push(regex::Regex::new(&format!("^{}$", glob_regex(&expanded)))?);
            } else if expanded.starts_with('/') {
                x.paths.push(regex::Regex::new(&format!("^{}$", glob_regex(&expanded)))?);
            } else {
                x.paths.push(regex::Regex::new(&format!("(?:^|/){}$", glob_regex(&expanded)))?);
            }
        }
        Ok(x)
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.paths.is_empty()
    }

    /// Whether `path` (absolute, or relative to `cwd` when given) or a
    /// directory above it matches a pattern.
    pub fn matches(&self, path: &str, cwd: Option<&str>) -> bool {
        if self.is_empty() || path.is_empty() {
            return false;
        }
        let joined;
        let mut p = path;
        if !p.starts_with('/')
            && let Some(c) = cwd.filter(|c| c.starts_with('/'))
        {
            joined = format!("{}/{}", c.trim_end_matches('/'), p.trim_start_matches("./"));
            p = &joined;
        }
        let mut cur = Some(Path::new(p));
        while let Some(x) = cur {
            let s = x.to_string_lossy();
            if s.is_empty() || s == "/" {
                break;
            }
            if self.paths.iter().any(|r| r.is_match(&s)) {
                return true;
            }
            if let Some(name) = x.file_name().map(|n| n.to_string_lossy())
                && self.names.iter().any(|r| r.is_match(&name))
            {
                return true;
            }
            cur = x.parent();
        }
        false
    }
}

/// File paths a tool call names in its input JSON (`file_path`, `path`,
/// `notebook_path`, `filePath`, `paths`).
pub fn tool_call_paths(input: &str) -> Vec<String> {
    let t = input.trim_start();
    if !t.starts_with('{') {
        return Vec::new();
    }
    let Ok(serde_json::Value::Object(m)) = serde_json::from_str::<serde_json::Value>(t) else { return Vec::new() };
    let mut out = Vec::new();
    for k in ["file_path", "path", "notebook_path", "filePath", "directory", "cwd"] {
        if let Some(s) = m.get(k).and_then(|v| v.as_str()) {
            out.push(s.to_string());
        }
    }
    if let Some(a) = m.get("paths").and_then(|v| v.as_array()) {
        out.extend(a.iter().filter_map(|v| v.as_str()).map(str::to_string));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let home = Path::new("/home/u");
        let x = Exclude::new(
            &[
                "*.pem".into(),
                ".env".into(),
                "~/private/**".into(),
                "/srv/keys".into(),
                "config/*.key".into(),
                "id_[rd]sa".into(),
            ],
            Some(home),
        )
        .unwrap();
        assert!(x.matches("/a/b/server.pem", None));
        assert!(x.matches("/a/.env", None));
        assert!(x.matches("/a/.env/inner.txt", None), "inside an excluded directory");
        assert!(x.matches("/home/u/private/notes.md", None));
        assert!(x.matches("/home/u/private/deep/er/x", None));
        assert!(x.matches("/srv/keys/a/b", None));
        assert!(x.matches("/repo/config/prod.key", None));
        assert!(x.matches("/home/u/.ssh/id_rsa", None));
        assert!(x.matches(".env", Some("/w")), "relative to the cwd");
        assert!(x.matches("sub/x.pem", Some("/w")));
        assert!(!x.matches("/a/b/server.pem.txt", None));
        assert!(!x.matches("/home/u/privateer/x", None));
        assert!(!x.matches("/srv/keys2/x", None));
        assert!(!x.matches("/repo/config/sub/prod.key", None));
        assert!(!x.matches("/home/u/.ssh/id_ecdsa", None));
        assert!(!Exclude::new(&[], None).unwrap().matches("/x", None));
    }

    #[test]
    fn paths_in_tool_calls() {
        assert_eq!(tool_call_paths(r#"{"file_path":"/a/.env","limit":3}"#), vec!["/a/.env"]);
        assert_eq!(tool_call_paths(r#"{"pattern":"x","path":"src"}"#), vec!["src"]);
        assert!(tool_call_paths("ls -la").is_empty());
    }
}
