//! Secret redaction on ingest: every text is masked before it is stored.
//!
//! Each secret becomes `[REDACTED:<kind>:<first 8 hex of sha256(secret)>]`.
//! The hash keeps identical secrets linkable across records without
//! revealing them. Rules run in order (specific formats first), each on the
//! output of the previous one, so a placeholder is never re-matched.
//!
//! Kinds: `pem_private_key`, `jwt`, `aws_key_id`, `github`, `anthropic`,
//! `openrouter`, `openai`, `slack`, `stripe`, `google_api`, vendor prefixes
//! (`supabase`, `sentry`, `linear`, `huggingface`, `npm`, `gitlab`, `e2b`,
//! `fireworks`, `xai`, `pinecone`), `aws_secret`, `url_credentials`, `bearer`,
//! `assignment` (`password=` / `token:` / `API_KEY=` style values) and
//! `high_entropy` (long random strings next to key/secret/token/password words).
use regex::{Captures, Regex};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// (secret, whole match) -> is it a secret?
type Check = fn(&str, &str) -> bool;
type KindOf = fn(&str) -> &'static str;

struct Rule {
    kind: &'static str,
    re: Regex,
    /// Capture group holding the secret (0 = the whole match).
    group: usize,
    check: Check,
    /// Kind decided by the matched secret (multi-vendor rule).
    kind_of: Option<KindOf>,
    /// Look at the text before the match (high-entropy context rule).
    context: bool,
}

pub struct Redactor {
    rules: Vec<Rule>,
    /// Optional false-positive audit: a reservoir sample of hits per kind,
    /// and a plain redactor to mask the sampled contexts.
    audit: Option<(std::sync::Mutex<Audit>, Box<Redactor>)>,
}

/// One sampled redaction, described without the secret: its shape
/// (character classes, run-length coded), length, entropy, and the
/// surrounding text with every secret in it masked.
#[derive(Clone, Debug)]
pub struct Hit {
    pub kind: &'static str,
    pub len: usize,
    pub entropy: f64,
    pub shape: String,
    pub before: String,
    pub after: String,
}

#[derive(Default)]
pub struct Audit {
    pub per_kind: usize,
    pub seen: BTreeMap<&'static str, u64>,
    pub samples: BTreeMap<&'static str, Vec<Hit>>,
    rng: u64,
}

/// Character-class shape, run-length coded: `A`=upper, `a`=lower, `9`=digit,
/// other characters as themselves (`sk-proj-Ab12` -> `a{2}-a{4}-Aa9{2}`).
pub fn shape(s: &str) -> String {
    let mut out = String::new();
    let mut last: Option<char> = None;
    let mut run = 0;
    let flush = |c: Option<char>, run: usize, out: &mut String| {
        if let Some(c) = c {
            out.push(c);
            if run > 1 {
                out.push_str(&format!("{{{run}}}"));
            }
        }
    };
    for ch in s.chars() {
        let c = match ch {
            'A'..='Z' => 'A',
            'a'..='z' => 'a',
            '0'..='9' => '9',
            c if c.is_ascii_punctuation() => c,
            _ => '?',
        };
        if Some(c) == last {
            run += 1;
        } else {
            flush(last, run, &mut out);
            last = Some(c);
            run = 1;
        }
    }
    flush(last, run, &mut out);
    out
}

/// Redaction counts by kind.
pub type Counts = BTreeMap<&'static str, u64>;

pub fn placeholder(kind: &str, secret: &str) -> String {
    let h = Sha256::digest(secret.as_bytes());
    let mut hex = String::with_capacity(8);
    for b in &h[..4] {
        hex.push_str(&format!("{b:02x}"));
    }
    format!("[REDACTED:{kind}:{hex}]")
}

/// Shannon entropy in bits per character.
pub fn entropy(s: &str) -> f64 {
    let mut freq = [0u32; 256];
    let mut n = 0u32;
    for b in s.bytes() {
        freq[b as usize] += 1;
        n += 1;
    }
    if n == 0 {
        return 0.0;
    }
    let n = n as f64;
    freq.iter().filter(|&&c| c > 0).map(|&c| {
        let p = c as f64 / n;
        -p * p.log2()
    }).sum()
}

#[derive(Default, Clone, Copy)]
struct Classes {
    lower: bool,
    upper: bool,
    digit: bool,
    other: bool,
}

fn classes(s: &str) -> Classes {
    let mut c = Classes::default();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' => c.lower = true,
            b'A'..=b'Z' => c.upper = true,
            b'0'..=b'9' => c.digit = true,
            _ => c.other = true,
        }
    }
    c
}

fn n_classes(c: Classes) -> u32 {
    c.lower as u32 + c.upper as u32 + c.digit as u32 + c.other as u32
}

const PLACEHOLDER_WORDS: &[&str] = &[
    "example", "placeholder", "your_", "your-", "yourtoken", "xxxx", "****", "....", "changeme", "redacted", "dummy",
    "sample", "<", ">", "${", "{{", "%s", "todo", "fixme", "insert", "replace", "000000", "123456", "abcdef", "test",
    "fake", "mock", "stub", "\u{2026}",
];

fn is_placeholder(v: &str) -> bool {
    let l = v.to_ascii_lowercase();
    PLACEHOLDER_WORDS.iter().any(|w| l.contains(w))
        || v.starts_with('$')
        || v.starts_with('%')
        || v.bytes().all(|b| b == v.as_bytes()[0])
}

fn always(_: &str, _: &str) -> bool {
    true
}

fn has_body(m: &str, _: &str) -> bool {
    // header (~27-40 chars) plus a real base64 body
    m.len() >= 100
}

fn has_digit(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_digit())
}

fn openai_check(s: &str, _: &str) -> bool {
    let c = classes(s);
    c.digit && c.lower && c.upper && !is_placeholder(s)
}

fn slack_check(s: &str, _: &str) -> bool {
    has_digit(s) && !is_placeholder(s)
}

/// A dotted identifier (`process.env.TOKEN`, `config.api.key`): code, not a secret.
fn dotted_identifier(v: &str) -> bool {
    v.contains('.')
        && v.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        && v.split('.').filter(|p| p.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_')).count() >= 2
}

/// Identifier-like: every `_`/`-`/`/`-separated part is letters only or digits only
/// (`VENDOR_API_SECRET`, `my-token-name-2`, `src/components/Auth`).
fn wordish(v: &str) -> bool {
    let parts: Vec<&str> = v.split(['_', '-', '/', '.']).filter(|p| !p.is_empty()).collect();
    !parts.is_empty()
        && parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_alphabetic()) || p.bytes().all(|b| b.is_ascii_digit()))
}

/// Code that names a value instead of holding it: `process.env.X!`,
/// `config?.accessToken`, `std::env::var`, `z.string`.
fn code_reference(v: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*(?:(?:\.|\?\.|!\.|::)[A-Za-z_$][A-Za-z0-9_$]*)+$").expect("code regex")
    });
    let t = v.trim_end_matches(['!', '?', ',', ';', ')']);
    re.is_match(t)
        || ["process.env", "os.environ", "import.meta", "getenv", "env::var", "env.", "secrets."]
            .iter()
            .any(|w| v.contains(w))
}

/// An environment variable *name* (`SA_COMPUTER_TEMPLATE`, `NEXT_KEY=`,
/// `12:NEXT_KEY=` from grep output): an empty assignment followed by the next name.
fn env_name(v: &str) -> bool {
    let upper_snake = |t: &str| {
        t.len() >= 2
            && t.as_bytes()[0].is_ascii_uppercase()
            && t.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    };
    let t = v.trim_end_matches('=');
    upper_snake(t) || (v.ends_with('=') && t.rsplit(':').next().is_some_and(|x| upper_snake(x) && x.contains('_')))
}

/// A camelCase identifier (`e2bApiKey`, `sessionToken2`): a variable, not a value.
fn camel_identifier(v: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^[a-z][a-z0-9]*(?:[A-Z][a-z0-9]+)+$").expect("camel regex"));
    v.len() <= 40 && re.is_match(v)
}

/// `12:rest` or `name:12:rest` (grep output), not a value.
fn line_ref(v: &str) -> bool {
    let mut parts = v.split(':');
    let first = parts.next().unwrap_or("");
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    (digits(first) && v.contains(':')) || parts.next().is_some_and(digits)
}

fn is_uuid(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| if matches!(i, 8 | 13 | 18 | 23) { c == b'-' } else { c.is_ascii_hexdigit() })
}

/// Value of a `key=value` / `"token": "value"` assignment.
fn assignment_check(v: &str, whole: &str) -> bool {
    if v.len() < 8 || is_placeholder(v) {
        return false;
    }
    if matches!(v.as_bytes()[0], b'/' | b'.' | b'~' | b'@' | b'#' | b'-' | b'+' | b'*' | b'=' | b'!' | b'?' | b':') {
        return false; // paths, flags, operators, `::` paths
    }
    let l = v.to_ascii_lowercase();
    if l.starts_with("http") || l.starts_with("file:") || l == "required" || l == "optional" || l == "undefined" {
        return false;
    }
    if v.bytes().all(|b| b.is_ascii_digit()) || dotted_identifier(v) || code_reference(v) || env_name(v) {
        return false;
    }
    if wordish(v.trim_end_matches(['!', '?', ','])) || (v.contains('/') && wordiness(v) >= 0.4) || camel_identifier(v) {
        return false; // identifiers, words, paths
    }
    // versions (`^3.1146.0`), ARNs and format strings name things, they are not secrets
    if v.starts_with(['^', '~']) || v.starts_with("arn:") || ["%s", "%d", "%i", "%r", "{}"].iter().any(|f| v.contains(f)) {
        return false;
    }
    if whole.contains("arn:aws") || whole.to_ascii_lowercase().contains("secretsmanager") || whole.ends_with('(') {
        return false; // ARNs name secrets; `secret = hmac(...)` is a call
    }
    // code operators and grep line references (`file:12:`)
    if ["==", "=>", "->"].iter().any(|o| v.contains(o)) || line_ref(v) {
        return false;
    }
    // `ref:OrgFeature`, `input_tokens:BIGINT`: a type or reference, not a value
    if let Some((_, rest)) = v.rsplit_once(':')
        && wordish(rest)
    {
        return false;
    }
    if v.len() > 4096 {
        return false;
    }
    // a UUID under an `..._id` / `..._ref` key is a reference, not the secret
    let w = whole.trim_end_matches('(');
    let key = w[..w.len() - v.len().min(w.len())].to_ascii_lowercase();
    let key = key.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    if is_uuid(v) && !["passw", "secret", "token", "api"].iter().any(|w| key.contains(w)) {
        return false;
    }
    if key.contains("public") {
        return false; // public keys and NEXT_PUBLIC_* values are published by design
    }
    let c = classes(v);
    n_classes(c) >= 2 && entropy(v) >= 2.5
}

/// Well-known local defaults (`postgres:postgres@127.0.0.1`) are not secrets.
fn url_cred_check(pw: &str, whole: &str) -> bool {
    let l = pw.to_ascii_lowercase();
    let user = whole.split("://").nth(1).and_then(|r| r.split(':').next()).unwrap_or("");
    let host = whole.rsplit('@').next().unwrap_or("");
    let local = host.starts_with("127.") || host.starts_with("localhost") || host.starts_with("[::1]");
    let lp = pw.to_ascii_lowercase();
    !(is_placeholder(pw)
        || pw.len() < 4
        || pw.contains(['&', '[', ']', '(', ')', '{', '}', '^', '\\', '$'])
        || ["%5b", "%3c", "%7b"].iter().any(|e| lp.contains(e)) // url-encoded `[`, `<`, `{` placeholders
        || wordish(pw)
        || (local && n_classes(classes(pw)) < 2)
        || l == user.to_ascii_lowercase()
        || matches!(
            l.as_str(),
            "password" | "pass" | "passwd" | "pwd" | "secret" | "pw" | "user" | "token" | "postgres" | "root" | "admin"
                | "test" | "guest"
        )
        || l.starts_with("[redacted"))
}

fn signature_check(s: &str, _: &str) -> bool {
    s.len() >= 16 && entropy(s) >= 3.0 && !is_placeholder(s)
}

fn bearer_check(t: &str, _: &str) -> bool {
    let c = classes(t);
    t.len() >= 20
        && !is_placeholder(t)
        && !t.ends_with("..")
        && c.digit
        && (c.lower || c.upper)
        && !wordish(t)
        && wordiness(t) < 0.5
        && entropy(t) >= 3.5
}

fn vendor_kind(s: &str) -> &'static str {
    match s {
        _ if s.starts_with("sbp_") || s.starts_with("sb_secret_") => "supabase",
        _ if s.starts_with("sntry") => "sentry",
        _ if s.starts_with("lin_") => "linear",
        _ if s.starts_with("hf_") => "huggingface",
        _ if s.starts_with("npm_") => "npm",
        _ if s.starts_with("glpat-") => "gitlab",
        _ if s.starts_with("e2b_") => "e2b",
        _ if s.starts_with("fw_") => "fireworks",
        _ if s.starts_with("xai-") => "xai",
        _ if s.starts_with("pcsk_") => "pinecone",
        _ if s.starts_with("whsec_") => "stripe",
        _ => "vendor",
    }
}

fn vendor_check(s: &str, _: &str) -> bool {
    has_digit(s) && !is_placeholder(s)
}

/// Words that make a nearby long random token a secret. `redacted` is an
/// earlier placeholder: the token may be the rest of a secret that a
/// terminal wrapped onto the next line.
const CONTEXT_WORDS: &[&str] =
    &["key", "secret", "token", "passw", "pwd", "credential", "auth", "bearer", "apikey", "redacted"];
const STRONG_WORDS: &[&str] = &["secret", "api_key", "api-key", "apikey", "token", "passw"];
/// Substrings that make a nearby long token an id, a hash or a count, not a secret.
const NOT_SECRET_WORDS: &[&str] = &[
    "sha", "hash", "commit", "digest", "checksum", "integrity", "uuid", "etag", "nonce", "cursor", "page", "request_id",
    "requestid", "trace", "span", "msg_", "toolu_", "call_", "thread", "turn_", "file_", "_id\"", "id:", "token_count",
    "tokens", "diff --git", ".spec", ".test", ".ts", ".md", "_id=", "_id:", "x-amz-", "public", "arn:", "signature",
    "project", "secretsmanager", "continuation", "policy=",
];

/// Share of a token covered by word runs (`[A-Z]?[a-z]{3,}` or `[A-Z]{4,}`):
/// paths, file, env and identifier names are mostly words; random keys are not.
fn wordiness(t: &str) -> f64 {
    let b = t.as_bytes();
    let (mut covered, mut i) = (0usize, 0usize);
    while i < b.len() {
        let start = i;
        let mut up = 0;
        while i < b.len() && b[i].is_ascii_uppercase() {
            i += 1;
            up += 1;
        }
        let low = i;
        while i < b.len() && b[i].is_ascii_lowercase() {
            i += 1;
        }
        let lows = i - low;
        if lows >= 3 && up <= 1 {
            covered += i - start;
        } else if up >= 4 && lows == 0 {
            covered += up;
        } else if lows >= 3 {
            covered += lows + 1;
        }
        if i == start {
            i += 1;
        }
    }
    covered as f64 / b.len().max(1) as f64
}

/// Typed ids (`prj_..`, `we_..`, `acct_..`, GraphQL node ids `I_kwDO..`,
/// Twilio `AC`/`CL` + 32 hex): names, not secrets.
fn typed_id(t: &str) -> bool {
    let b = t.as_bytes();
    let us = b.iter().position(|&c| c == b'_');
    let lower_prefix = us.filter(|&p| (2..=6).contains(&p) && b[..p].iter().all(u8::is_ascii_lowercase));
    let node_id = us.filter(|&p| (1..=5).contains(&p) && b[..p].iter().all(u8::is_ascii_uppercase));
    lower_prefix.is_some()
        || node_id.is_some()
        || (b.len() == 34 && b[..2].iter().all(u8::is_ascii_uppercase) && b[2..].iter().all(u8::is_ascii_hexdigit))
}

/// A path: two or more `/` with a word segment.
fn path_like(t: &str) -> bool {
    t.matches('/').count() >= 2
        && t.split('/').any(|p| p.len() >= 4 && p.bytes().all(|b| b.is_ascii_lowercase() || b == b'-' || b == b'_'))
}

/// Does `ctx` hold one of `words` as a word (or a camelCase hump)? `tokens`,
/// `oauth`, `authuser`, `keyboard` do not count; `token`, `apiKey`,
/// `CRON_SECRET`, `x-api-key` do.
fn has_word(ctx: &str, words: &[&str]) -> bool {
    let lower = ctx.to_ascii_lowercase();
    let (lb, ob) = (lower.as_bytes(), ctx.as_bytes());
    words.iter().any(|w| {
        lower.match_indices(w).any(|(i, _)| {
            let before_ok = i == 0 || !ob[i - 1].is_ascii_alphabetic() || ob[i].is_ascii_uppercase();
            let j = i + w.len();
            let after_ok = j >= lb.len() || !ob[j].is_ascii_alphabetic();
            before_ok && after_ok
        })
    })
}

/// Character-class changes per character (upper/lower/digit): random keys
/// switch often (~0.5), words and paths rarely.
fn transitions(t: &str) -> f64 {
    let cls = |b: u8| match b {
        b'a'..=b'z' => 0,
        b'A'..=b'Z' => 1,
        b'0'..=b'9' => 2,
        _ => 3,
    };
    let b = t.as_bytes();
    let n = b.windows(2).filter(|w| cls(w[0]) != cls(w[1])).count();
    n as f64 / b.len().max(1) as f64
}

/// A long token next to a key/secret/token/password word, random-looking.
fn high_entropy_ok(token: &str, before: &str) -> bool {
    if token.len() < 24 || token.len() > 512 || is_placeholder(token) {
        return false;
    }
    let lower = before.to_ascii_lowercase();
    if !has_word(before, CONTEXT_WORDS) || NOT_SECRET_WORDS.iter().any(|w| lower.contains(w)) {
        return false;
    }
    let c = classes(token);
    if !c.digit || wordish(token) || typed_id(token) || path_like(token) {
        return false;
    }
    if token.bytes().all(|b| b.is_ascii_hexdigit()) {
        // hashes and ids are hex too: only directly after a strong word (`token: <hex>`)
        let near = before.trim_end_matches([' ', ':', '=', '"', '\'', '`']);
        let mut from = near.len().saturating_sub(12);
        while !near.is_char_boundary(from) {
            from += 1;
        }
        let tail = &near[from..];
        let ends = STRONG_WORDS.iter().any(|w| tail.to_ascii_lowercase().ends_with(w));
        return token.len() >= 32 && ends && entropy(token) >= 3.3;
    }
    c.lower && c.upper && entropy(token) >= 3.8 && transitions(token) >= 0.3 && wordiness(token) < 0.4
}

/// Is `at` inside an earlier rule's `[REDACTED:...]` placeholder?
fn inside_placeholder(src: &str, at: usize) -> bool {
    let mut from = at.saturating_sub(64);
    while !src.is_char_boundary(from) {
        from += 1;
    }
    let before = &src[from..at];
    before.rfind("[REDACTED:").is_some_and(|p| !before[p..].contains(']'))
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

fn rule(kind: &'static str, re: &str, group: usize, check: Check) -> Rule {
    Rule { kind, re: Regex::new(re).expect("redaction regex"), group, check, kind_of: None, context: false }
}

/// Key words of the assignment rule (case-insensitive).
const KEY_WORDS: &str = r"(?:passw(?:or)?d|passwd|pwd|secret|token|api[_-]?key|apikey|access[_-]?key|private[_-]?key|auth[_-]?key|credentials?)";

impl Redactor {
    pub fn new() -> Self {
        let mut rules = vec![
            rule(
                "pem_private_key",
                r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----(?:[A-Za-z0-9+/=\s]|\\[nrt])*(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----)?",
                0,
                has_body,
            ),
            rule("jwt", r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{16,}", 0, always),
            rule("aws_key_id", r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b", 0, always),
            rule("github", r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{50,})", 0, always),
            rule("anthropic", r"\bsk-ant-[A-Za-z0-9_-]{24,}", 0, always),
            rule("openrouter", r"\bsk-or-v1-[A-Za-z0-9]{40,}", 0, always),
            rule(
                "openai",
                r"\bsk-(?:(?:proj|svcacct|admin)-[A-Za-z0-9_-]{40,}|[A-Za-z0-9]{20,}T3BlbkFJ[A-Za-z0-9_-]{20,}|[A-Za-z0-9]{40,64}\b)",
                0,
                openai_check,
            ),
            // the rest of a key that a terminal wrapped onto another line
            rule("openai", r"[A-Za-z0-9_-]{8,}T3BlbkFJ[A-Za-z0-9_-]{8,}", 0, openai_check),
            rule("slack", r"\bxox[abprs]-[A-Za-z0-9-]{10,}", 0, slack_check),
            rule("stripe", r"\b[sr]k_(?:live|test)_[A-Za-z0-9]{16,}", 0, always),
            rule("google_api", r"\bAIza[0-9A-Za-z_-]{35}", 0, always),
        ];
        let mut vendor = rule(
            "vendor",
            r"\b(?:sbp_[a-f0-9]{40}|sb_secret_[A-Za-z0-9_-]{20,}|sntry[su]_[A-Za-z0-9+/=_-]{40,}|lin_(?:api|oauth)_[A-Za-z0-9]{30,}|hf_[A-Za-z]{34,}|npm_[A-Za-z0-9]{36}|glpat-[A-Za-z0-9_-]{20,}|e2b_[A-Za-z0-9]{30,}|fw_[A-Za-z0-9]{20,}|xai-[A-Za-z0-9]{40,}|pcsk_[A-Za-z0-9_]{40,}|whsec_[A-Za-z0-9+/=]{24,})",
            0,
            vendor_check,
        );
        vendor.kind_of = Some(vendor_kind);
        rules.push(vendor);
        rules.push(rule(
            "aws_secret",
            r#"(?i)(?:aws_?secret_?access_?key|secret_?access_?key)(?:\\?["'])?\s{0,3}[:=]\s{0,3}(?:\\?["'])?([A-Za-z0-9/+]{40})(?:[^A-Za-z0-9/+=]|$)"#,
            1,
            always,
        ));
        rules.push(rule(
            "url_credentials",
            r#"\b[a-zA-Z][a-zA-Z0-9+.-]{1,20}://[^\s:/@"'<>\\]{1,64}:([^\s:/@"'<>\\]{3,128})@[A-Za-z0-9.\[\]-]+"#,
            1,
            url_cred_check,
        ));
        rules.push(rule(
            "url_signature",
            r"(?i)[?&](?:x-amz-signature|x-amz-security-token|x-goog-signature|signature|sig)=([A-Za-z0-9%/+=_.-]{16,})",
            1,
            signature_check,
        ));
        rules.push(rule("bearer", r"(?i)\b(?:bearer|basic)\s+([A-Za-z0-9._~+/=-]{16,})", 1, bearer_check));
        rules.push(rule(
            "assignment",
            &format!(
                r#"(?i)\b[A-Za-z0-9_.-]{{0,40}}?{KEY_WORDS}[A-Za-z0-9_.-]{{0,40}}(?:\\?["'])?\s{{0,3}}[:=]\s{{0,3}}(?:\\?["'])?([^\s"'`,;&|<>(){{}}\[\]\\]{{8,}})(\()?"#
            ),
            1,
            assignment_check,
        ));
        rules.push(rule(
            "assignment",
            r#"(?i)--(?:password|passwd|token|secret|api-key|apikey|access-key|auth-token)[ =](?:\\?["'])?([^\s"'`\\]{8,256})"#,
            1,
            assignment_check,
        ));
        let mut he = rule("high_entropy", r"[A-Za-z0-9+/_-]{24,}(?:\.[A-Za-z0-9+/_-]{16,})*={0,2}", 0, always);
        he.context = true;
        rules.push(he);
        Self { rules, audit: None }
    }

    /// A redactor that also keeps `per_kind` sampled hits of each kind.
    pub fn with_audit(per_kind: usize, seed: u64) -> Self {
        let mut r = Self::new();
        r.audit = Some((std::sync::Mutex::new(Audit { per_kind, rng: seed | 1, ..Audit::default() }), Box::new(Self::new())));
        r
    }

    pub fn audit(&self) -> Option<std::sync::MutexGuard<'_, Audit>> {
        self.audit.as_ref().map(|a| a.0.lock().unwrap())
    }

    #[allow(clippy::too_many_arguments)]
    fn sample(
        a: &std::sync::Mutex<Audit>,
        plain: &Redactor,
        kind: &'static str,
        src: &str,
        (start, end): (usize, usize),
        (g_start, g_end): (usize, usize),
        secret: &str,
    ) {
        let mut g = a.lock().unwrap();
        let n = {
            let c = g.seen.entry(kind).or_default();
            *c += 1;
            *c
        };
        g.rng ^= g.rng << 13;
        g.rng ^= g.rng >> 7;
        g.rng ^= g.rng << 17;
        let per = g.per_kind as u64;
        let slot = if n <= per { Some((n - 1) as usize) } else { Some((g.rng % n) as usize).filter(|&j| (j as u64) < per) };
        let Some(slot) = slot else { return };
        drop(g);
        let mut from = start.saturating_sub(48);
        while !src.is_char_boundary(from) {
            from += 1;
        }
        let mut to = (end + 48).min(src.len());
        while !src.is_char_boundary(to) {
            to -= 1;
        }
        // mask every secret in the context (plain redactor: no audit recursion)
        let mut c = Counts::new();
        // a token cut by the window edge would escape its rule: drop it
        let tok = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'/' | b'=' | b'.');
        let mut before = &src[from..start];
        if from > 0 {
            let cut = before.bytes().position(|b| !tok(b)).unwrap_or(before.len());
            before = &before[cut..];
        }
        let mut after = &src[end..to];
        if to < src.len() {
            let cut = after.bytes().rposition(|b| !tok(b)).map_or(0, |p| p + 1);
            after = &after[..cut];
        }
        let hit = Hit {
            kind,
            len: secret.len(),
            entropy: entropy(secret),
            shape: shape(secret),
            before: plain.redact(&format!("{before}{}", &src[start..g_start]), &mut c),
            after: plain.redact(&format!("{}{after}", &src[g_end..end]), &mut c),
        };
        let mut g = a.lock().unwrap();
        let v = g.samples.entry(kind).or_default();
        if slot < v.len() {
            v[slot] = hit;
        } else {
            v.push(hit);
        }
    }

    /// Mask every secret in `text`; counts go into `counts` by kind.
    pub fn redact(&self, text: &str, counts: &mut Counts) -> String {
        let mut cur: Option<String> = None;
        for r in &self.rules {
            let src = cur.as_deref().unwrap_or(text);
            let out = r.re.replace_all(src, |caps: &Captures| {
                let whole = caps.get(0).unwrap();
                let Some(g) = caps.get(r.group) else { return whole.as_str().to_string() };
                if inside_placeholder(src, whole.start()) {
                    return whole.as_str().to_string();
                }
                let secret = g.as_str();
                let ok = if r.context {
                    let start = whole.start();
                    // a token glued to a longer word run is not a standalone value
                    let prev = src[..start].chars().next_back();
                    let mut from = start.saturating_sub(40);
                    while !src.is_char_boundary(from) {
                        from += 1;
                    }
                    // glued to a word, an env reference (`$X`), a host or a path: not standalone
                    prev.is_none_or(|c| !c.is_ascii_alphanumeric() && !matches!(c, '$' | '.' | '/'))
                        && high_entropy_ok(secret, &src[from..start])
                } else {
                    (r.check)(if r.group == 0 { whole.as_str() } else { secret }, whole.as_str())
                };
                if !ok {
                    return whole.as_str().to_string();
                }
                let kind = r.kind_of.map_or(r.kind, |f| f(secret));
                *counts.entry(kind).or_default() += 1;
                if let Some((a, plain)) = &self.audit {
                    Self::sample(a, plain, kind, src, (whole.start(), whole.end()), (g.start(), g.end()), secret);
                }
                let w = whole.as_str();
                let (a, b) = (g.start() - whole.start(), g.end() - whole.start());
                format!("{}{}{}", &w[..a], placeholder(kind, secret), &w[b..])
            });
            if let std::borrow::Cow::Owned(s) = out {
                cur = Some(s);
            }
        }
        cur.unwrap_or_else(|| text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic secrets, assembled at run time (none is a real credential).
    fn rnd(seed: u64, n: usize, alphabet: &[u8]) -> String {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                alphabet[(x % alphabet.len() as u64) as usize] as char
            })
            .collect()
    }
    const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    const UPNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    fn red(s: &str) -> (String, Counts) {
        let r = Redactor::new();
        let mut c = Counts::new();
        let out = r.redact(s, &mut c);
        (out, c)
    }

    fn assert_masked(input: &str, secret: &str, kind: &str) {
        let (out, c) = red(input);
        assert!(!out.contains(secret), "{kind}: secret survived in {out}");
        assert!(out.contains(&placeholder(kind, secret)), "{kind}: placeholder missing in {out}");
        assert_eq!(c.get(kind), Some(&1), "{kind}: counts {c:?}");
    }

    #[test]
    fn known_formats() {
        let aws = format!("AKIA{}", rnd(1, 16, UPNUM));
        assert_masked(&format!("export AWS_ACCESS_KEY_ID={aws}\n"), &aws, "aws_key_id");
        let asia = format!("ASIA{}", rnd(2, 16, UPNUM));
        assert_masked(&format!("key {asia} used"), &asia, "aws_key_id");
        let sec = rnd(3, 40, B64);
        assert_masked(&format!("aws_secret_access_key = {sec}\n"), &sec, "aws_secret");
        let gh = format!("ghp_{}", rnd(4, 36, ALNUM));
        assert_masked(&format!("token {gh} ok"), &gh, "github");
        let gho = format!("gho_{}", rnd(5, 36, ALNUM));
        assert_masked(&format!("x={gho}"), &gho, "github");
        assert_eq!(shape("sk-proj-Ab12"), "a{2}-a{4}-Aa9{2}");
        let pat = format!("github_pat_{}_{}", rnd(6, 22, ALNUM), rnd(7, 59, ALNUM));
        assert_masked(&format!("use {pat}"), &pat, "github");
        let ant = format!("sk-ant-api03-{}", rnd(8, 90, ALNUM));
        assert_masked(&format!("ANTHROPIC_API_KEY={ant}"), &ant, "anthropic");
        let oai = format!("sk-proj-{}", rnd(9, 120, ALNUM));
        assert_masked(&format!("\"{oai}\""), &oai, "openai");
        let oai2 = format!("sk-{}T3BlbkFJ{}", rnd(10, 20, ALNUM), rnd(11, 20, ALNUM));
        assert_masked(&format!("k: {oai2}"), &oai2, "openai");
        let slack = format!("xoxb-{}-{}-{}", rnd(12, 12, b"0123456789"), rnd(13, 13, b"0123456789"), rnd(14, 24, ALNUM));
        assert_masked(&format!("SLACK={slack}"), &slack, "slack");
        let stripe = format!("sk_live_{}", rnd(15, 24, ALNUM));
        assert_masked(&format!("stripe {stripe}"), &stripe, "stripe");
        let rk = format!("rk_live_{}", rnd(16, 24, ALNUM));
        assert_masked(&rk, &rk, "stripe");
        let g = format!("AIza{}", rnd(17, 35, ALNUM));
        assert_masked(&format!("?key={g}&x=1"), &g, "google_api");
        let jwt = format!("eyJ{}.eyJ{}.{}", rnd(18, 30, ALNUM), rnd(19, 60, ALNUM), rnd(20, 43, ALNUM));
        assert_masked(&format!("Authorization: {jwt}"), &jwt, "jwt");
        let sbp = format!("sbp_{}", rnd(21, 40, b"0123456789abcdef"));
        assert_masked(&format!("SUPABASE_ACCESS_TOKEN={sbp}"), &sbp, "supabase");
    }

    #[test]
    fn pem_blocks_plain_and_json_escaped() {
        let body = rnd(30, 64, B64);
        let pem = format!("-----BEGIN RSA PRIVATE KEY-----\n{body}\n{body}\n-----END RSA PRIVATE KEY-----");
        let (out, c) = red(&format!("here:\n{pem}\nthere"));
        assert!(!out.contains(&body));
        assert_eq!(c.get("pem_private_key"), Some(&1));
        assert!(out.starts_with("here:\n[REDACTED:pem_private_key:") && out.ends_with("]\nthere"));
        // inside a JSON string (tool call input), newlines are escaped
        let esc = pem.replace('\n', "\\n");
        let (out, c) = red(&format!("{{\"content\":\"{esc}\"}}"));
        assert!(!out.contains(&body));
        assert_eq!(c.get("pem_private_key"), Some(&1));
        // a mention of the header alone is not a key
        let (out, c) = red("if line.starts_with(\"-----BEGIN PRIVATE KEY-----\") {");
        assert!(c.is_empty(), "{out}");
    }

    #[test]
    fn assignments_and_context() {
        let pw = format!("{}!{}", rnd(40, 6, ALNUM), rnd(41, 5, ALNUM));
        assert_masked(&format!("DB_PASSWORD={pw}\n"), &pw, "assignment");
        assert_masked(&format!("{{\"password\": \"{pw}\"}}"), &pw, "assignment");
        assert_masked(&format!("{{\\\"api_key\\\":\\\"{pw}\\\"}}"), &pw, "assignment");
        assert_masked(&format!("mysql --password={pw} -h db"), &pw, "assignment");
        let url_pw = rnd(42, 18, ALNUM);
        assert_masked(&format!("postgres://app:{url_pw}@db.example.net:5432/x"), &url_pw, "url_credentials");
        let bearer = rnd(43, 40, ALNUM);
        assert_masked(&format!("-H 'Authorization: Bearer {bearer}'"), &bearer, "bearer");
        let he = rnd(44, 48, ALNUM);
        assert_masked(&format!("the signing secret is {he} (rotate it)"), &he, "high_entropy");
        let sig = rnd(45, 43, B64);
        assert_masked(&format!("https://b.s3.amazonaws.com/f.pdf?Expires=1&Signature={sig}&x=1"), &sig, "url_signature");
        // the tail of an OpenAI key that a terminal wrapped onto the next line
        let tail = format!("{}T3BlbkFJ{}", rnd(46, 18, ALNUM), rnd(47, 20, ALNUM));
        let (out, c) = red(&format!("OPENAI_API_KEY=sk-proj-{}\n   {tail}", rnd(48, 50, ALNUM)));
        assert!(!out.contains(&tail) && out.contains(&placeholder("openai", &tail)));
        assert_eq!(c.get("openai"), Some(&2), "head and wrapped tail");
        // a third wrapped piece without the marker: random, right after a placeholder
        let piece = rnd(49, 40, ALNUM);
        assert_masked(&format!("KEY=[REDACTED:openai:0d98ed61]    {piece}  "), &piece, "high_entropy");
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        for s in [
            "max_tokens=4096 and input_tokens: 1200",
            "password: z.string().min(8)",
            "const token = process.env.API_TOKEN;",
            "token=${TOKEN} secret=$SECRET",
            "PASSWORD_MIN_LENGTH=8",
            "token_type: \"bearer\"",
            "api_key = os.environ[\"OPENAI_API_KEY\"]",
            "commit 3f2a9c1e5b7d4a6f8e0c2b4d6f8a0c2e4b6d8f0a fixes the token refresh",
            "sha256: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "integrity sha512-Vf3xYz0WxL+kq7lXf9z0aT2mBn4QhJ8sRk1pYc5vUe6wNz3oGt7iH2jK4lM8nO0pQ==",
            "see src/components/SecretManagerPanel/index.tsx for the token form",
            "the VENDOR_API_SECRET_KEY_NAME variable",
            "user: postgres://user:password@localhost/db",
            "sk-learn is a library; risk-assessment-framework-for-models",
            "Bearer token required",
            "new VendorClient({ apiKey: process.env.VENDOR_API_KEY! })",
            "const token = config?.accessToken(tenantId);",
            "VENDOR_API_KEY= APP_MODEL_NAME= APP_SECRET_NAME=",
            "use crate::secret::Redactor;",
            "postgres://postgres:postgres@127.0.0.1:54322/postgres",
            "{\"max_output_tokens\":900,\"workdir\":\"/home/u/work/project-2026\"}",
            "max_output_tokens: 1300, branch agent/r2-cache-cleanup-20260101 and feature/oauth-scopes-fix-20261012",
            "const STORAGE_KEY = \"app-home-layout-v2\";",
            "authuser=1&project=my-project-name-123456 oauth flow",
            "original_token_count\":927,\"output\":\"src/tests/e2e/widgets/renderWidgetStatus.spec.ts\"",
            "secret key file: app/fonts/SourceSerif_400RegularItalic2.ttf",
            "aws secretsmanager list: app/v2/prod/LIMITS_CONFIG_JSON",
            "12:PAYMENTS_SECRET_KEY= 13:PARTNER_CREDENTIAL_KEY=",
            "\"@vendor/client-secrets\": \"^2.71.0\"",
            "valueFrom = \"arn:aws:secretsmanager:us-east-1:123456789012:secret:svc/app-AbC123\"",
            "const accessToken = signed.data!.session_token;",
            "Authorization: Bearer eyJhbGciOiJIUzI1NiIs...",
            "headers: { authorization: \"Bearer cron-secret-for-local-tests\" }",
            "postgresql://postgres:local-development@127.0.0.1:5432/postgres",
            "https://&lt;user&gt;:&lt;password&gt;@example.com/hook",
            "const secretKey = hmac512Digest(Buffer.from(seed));",
            "ready site=webmail.invalid credential_id=3f2a9c1e-5b7d-4a6f-8e0c-2b4d6f8a0c2e fields=email",
            "tests/secret.test.ts:12:import { x } from y",
            "u={kind:inputTokens===image?a:b}",
            "EXPECTED_PROJECT_ID=\"prj_Zq81Lm2Nx7Kd0Pw4Rt6Yv9Bc\" # an identifier",
            "PAYMENTS_WEBHOOK_SECRET {\"id\":\"we_9QwErTy1UiOp2AsDf3GhJkLz\"}",
            "\"thinkingSignature\": \"c2lnbmF0dXJlLWJsb2ItZm9yLWEtc3ludGhldGljLWNhc2UtOTg3NjU0MzIx\"",
            "read token; H=3f2a9c1e5b7d4a6f8e0c2b4d6f8a0c2e4b6d8f0a; done",
            "encryptionPublicKey: 'MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE9x8Y'",
            "None app/v2/prod/METRICS_SIGNING_KEY None app/v2/prod/QUEUE_ROUTING_KEY None",
            "curl -H \"Authorization: Bearer $SERVICE_KEY\" \"$API_URL/rest/v1/items?select=id\"",
            "credentials : ref:TeamPlanFeature | output_tokens:BIGINT",
            "publicKeyToken=\"7c1e5a9d3b2f8e64\" credential=3f2a9c1e-5b7d-4a6f-8e0c-2b4d6f8a0c2e",
            "auth token for item in I_kwDOAb12_Cd34EF56gh78Ij I_kwDOAb12_Kl90MN12op34Qr",
            "\"issuer\": \"https://auth.vendor.net/Xk29fLp3QmZ8rT4vWb7Y\"",
            "secretsmanager:us-east-1:123456789012:secret:app/v2/prod/MODEL_API_KEY-Ab12Cd",
            "m = re.match(r'https://x-token:([^@]+)@git.example.net', line)",
            "url=f\"postgresql://op.ref:{pw}@db.example.net:6543/app\"",
            "postgresql://dbuser.tenant:%5BYOUR-PASSWORD%5D@db.example.net",
            "Client.create({ apiKey: vendorApiKey2, timeoutMs: 5 })",
            "for item in PVTI_lADOXy12Zw34Vu56Ts78Rq9 token PVTI_lADOXy12Zw34Vu56Ts78Rq0",
        ] {
            let (out, c) = red(s);
            assert!(c.is_empty(), "false positive on {s:?}: {out}");
            assert_eq!(out, s);
        }
    }

    #[test]
    fn identical_secrets_share_a_placeholder_and_are_not_rematched() {
        let gh = format!("ghp_{}", rnd(50, 36, ALNUM));
        let (out, c) = red(&format!("a {gh} b {gh} c token={gh}"));
        assert_eq!(c.get("github"), Some(&3));
        let p = placeholder("github", &gh);
        assert_eq!(out.matches(&p).count(), 3);
        assert_eq!(out, format!("a {p} b {p} c token={p}"));
        let (again, c2) = red(&out);
        assert!(c2.is_empty());
        assert_eq!(again, out);
    }

    #[test]
    fn placeholder_format() {
        // sha256("abc") = ba7816bf...
        assert_eq!(placeholder("jwt", "abc"), "[REDACTED:jwt:ba7816bf]");
    }
}
