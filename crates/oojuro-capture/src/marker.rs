//! Records that come from Oojuro itself: memory lookups (calls to the
//! `oojuro` CLI or to an Oojuro MCP server, and their results) and context
//! Oojuro injected into a turn. They are stored, because the agent saw
//! them, but never indexed: a search (and so the per-turn injection) never
//! returns the journal's own echo, which would otherwise feed back into the
//! next turn.
//!
//! Marking is structural, the same on every capture path (log tailer,
//! hooks, OpenTelemetry, the model API proxy), and recorded as the metadata pair
//! `oojuro=<how>`:
//! - `context`: the text holds an injection envelope, a line starting with
//!   `<oojuro-context` (then a space or `>`) and a later line
//!   `</oojuro-context>`. Everything Oojuro injects is wrapped in one.
//! - `call`: a tool call that goes to Oojuro: a tool of an MCP server whose
//!   name contains `oojuro` (`mcp__oojuro__search`, a Codex call with
//!   namespace `oojuro`, a hook's `mcp_server.name`), or a shell command one
//!   of whose simple commands runs the program `oojuro` (`oojuro search
//!   ...`, `cd x && ./target/release/oojuro get 12`).
//! - `result`: the result of such a call (linked by its call id).
pub const META: &str = "oojuro";
pub const CONTEXT: &str = "context";
pub const CALL: &str = "call";
pub const RESULT: &str = "result";

/// First line of an injection envelope (followed by attributes and `>`).
pub const OPEN: &str = "<oojuro-context";
/// Last line of an injection envelope.
pub const CLOSE: &str = "</oojuro-context>";

/// Whether `text` holds an injection envelope (open tag at a line start,
/// close tag on a later line of its own).
pub fn has_context(text: &str) -> bool {
    if !text.contains(OPEN) {
        return false;
    }
    let mut open = false;
    for line in text.lines() {
        if !open {
            if let Some(rest) = line.strip_prefix(OPEN) {
                open = rest.starts_with(' ') || rest.starts_with('>');
            }
        } else if line.trim_end() == CLOSE {
            return true;
        }
    }
    false
}

/// An MCP server (or tool namespace) name that belongs to Oojuro.
fn oojuro_server(name: &str) -> bool {
    name.to_ascii_lowercase().contains("oojuro")
}

/// Whether a tool call goes to Oojuro. `tool` is the tool name,
/// `namespace` the MCP server or tool namespace when the source names it
/// (else empty), `input` the call's input (JSON or raw text).
pub fn is_oojuro_call(tool: &str, namespace: &str, input: &str) -> bool {
    if !namespace.is_empty() && oojuro_server(namespace) {
        return true;
    }
    // MCP tool names: `mcp__<server>__<tool>` (Claude Code), `<server>__<tool>`
    let parts: Vec<&str> = tool.split("__").collect();
    if parts.len() >= 2 && parts[..parts.len() - 1].iter().any(|p| oojuro_server(p)) {
        return true;
    }
    if !input.contains("oojuro") {
        return false;
    }
    commands(input).iter().any(|c| runs_oojuro(c))
}

/// Shell command lines in a tool input: the `command` / `cmd` / `script`
/// fields of a JSON object (a string, or an argv array whose script is the
/// argument after `-c`), else the raw input itself.
pub fn commands(input: &str) -> Vec<String> {
    let v: serde_json::Value = match serde_json::from_str(input) {
        Ok(v) => v,
        Err(_) => return vec![input.to_string()],
    };
    let Some(obj) = v.as_object() else {
        return v.as_str().map(|s| vec![s.to_string()]).unwrap_or_default();
    };
    let mut out = Vec::new();
    for k in ["command", "cmd", "script"] {
        match obj.get(k) {
            Some(serde_json::Value::String(s)) => out.push(s.clone()),
            Some(serde_json::Value::Array(a)) => {
                let argv: Vec<&str> = a.iter().filter_map(|x| x.as_str()).collect();
                match argv.iter().position(|x| x.starts_with('-') && x.ends_with('c') && x.len() <= 4) {
                    Some(i) if i + 1 < argv.len() => out.push(argv[i + 1].to_string()),
                    _ => out.push(argv.join(" ")),
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether any simple command of a shell line runs the program `oojuro`.
/// Separators: `;`, `&`, `|`, newlines, parentheses, braces, backticks and
/// `$(`. Leading `NAME=value` assignments and process wrappers (`env`,
/// `exec`, `command`, `nohup`, `time`, `sudo`, `nice [-n N]`,
/// `timeout DUR`) are skipped.
pub fn runs_oojuro(cmd: &str) -> bool {
    let seps: &[char] = &[';', '&', '|', '\n', '(', ')', '{', '}', '`'];
    for seg in cmd.replace("$(", "\n").split(seps) {
        let mut words = seg.split_whitespace().map(|w| w.trim_matches(|c| c == '"' || c == '\''));
        let mut prog = None;
        while let Some(w) = words.next() {
            let assignment = w
                .split_once('=')
                .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
            match w {
                _ if assignment => {}
                "env" | "exec" | "command" | "nohup" | "time" | "sudo" => {}
                "nice" | "timeout" => {
                    // their numeric argument(s)
                    let mut peek = words.clone();
                    while let Some(a) = peek.next() {
                        if a == "-n" || a.starts_with('-') || a.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                            words = peek.clone();
                        } else {
                            break;
                        }
                    }
                }
                _ => {
                    prog = Some(w);
                    break;
                }
            }
        }
        if prog.is_some_and(|p| p.rsplit('/').next() == Some("oojuro")) {
            return true;
        }
    }
    false
}

/// Add `oojuro=<how>` to record metadata (once).
pub fn mark(meta: &mut Vec<(&'static str, String)>, how: &str) {
    if !meta.iter().any(|m| m.0 == META) {
        meta.push((META, how.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelopes_are_found_only_as_their_own_lines() {
        let inj =
            "Earlier records from your journal:\n<oojuro-context v=\"1\" items=\"2\">\n[1] ...\n</oojuro-context>\n";
        assert!(has_context(inj));
        assert!(has_context(&format!(
            "<system-reminder>\nUserPromptSubmit hook additional context: {inj}</system-reminder>"
        )));
        // source code or docs that only mention the tags
        assert!(!has_context("let open = \"<oojuro-context v=1>\"; // ... </oojuro-context>"));
        assert!(!has_context("    12\t<oojuro-context v=\"1\">\n    13\t</oojuro-context>"));
        assert!(!has_context("<oojuro-contextual>\n</oojuro-context>"));
        assert!(!has_context("<oojuro-context v=\"1\">\nnever closed"));
    }

    #[test]
    fn calls_to_oojuro_by_structure() {
        assert!(is_oojuro_call("mcp__oojuro__search", "", r#"{"query":"deploy"}"#));
        assert!(is_oojuro_call("mcp__plugin_oojuro_journal__get", "", r#"{"id":3}"#));
        assert!(is_oojuro_call("search", "oojuro", r#"{"query":"x"}"#));
        assert!(!is_oojuro_call("mcp__memory__search", "", r#"{"query":"oojuro"}"#));
        let bash = |c: &str| is_oojuro_call("Bash", "", &serde_json::json!({ "command": c }).to_string());
        assert!(bash("oojuro search \"staging deploys\" -k 5"));
        assert!(bash("cd ~/repo && ./target/release/oojuro --json get 42 | jq ."));
        assert!(bash("OOJURO_HOME=/x nice -n 19 oojuro range 1h"));
        assert!(bash("timeout 5 env A=1 oojuro stats"));
        assert!(bash("echo hi; $(oojuro search x)"));
        assert!(!bash("cargo test -p oojuro"));
        assert!(!bash("grep -rn \"oojuro search\" docs/"));
        assert!(!bash("echo 'run oojuro search later'"));
        assert!(!bash("./target/release/oojurod stats"));
        // Codex argv and raw custom-tool input
        assert!(is_oojuro_call("shell", "", r#"{"command":["bash","-lc","oojuro search foo"]}"#));
        assert!(is_oojuro_call("exec_command", "", r#"{"cmd":"oojuro get 7"}"#));
        assert!(is_oojuro_call("exec", "", "oojuro history notes"));
        assert!(!is_oojuro_call("apply_patch", "", "*** Begin Patch\n+ oojuro is great\n"));
    }
}
