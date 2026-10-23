//! `oojuro`: use the journal like SQLite (`docs/FRONT_DOOR.md`).
use oojuro::{
    DEFAULT_EXCERPT_BYTES, DEFAULT_K, Filters, Oojuro, Record, Ref, SearchArgs, Target,
    config::{self, Config},
    mcp::McpServer,
    paths::{self, Paths},
    periods::{By, Per, PeriodsArgs},
    timearg::{parse_duration, parse_time},
    when::{self, Scope, When},
};
use std::{
    io::{IsTerminal, Read, Write},
    path::PathBuf,
};

type Result<T> = oojuro::Result<T>;

const USAGE: &str =
    "usage: oojuro [--home DIR] [--store DIR] [--socket PATH] [--config FILE] [--direct] [--json] <command>

  remember [--key K] [--writer W] [--at TIME] TEXT...|-    store a note (a key keeps versions; newest wins)
  search   QUERY...|- [-k N] [--kind K]... [--key K] [--writer W]... [--from T] [--to T] [--when PHRASE] [--as-of T] [--excerpt-bytes N]
  get      ID | --key K [--writer W] [--as-of T]           one record, exact
  history  KEY [--writer W]                                every version of a key
  range    [FROM [TO]] [--kind K]... [--writer W]... [--key K] [--when PHRASE] [--as-of T] [--limit N]
  periods  [QUERY...|-] [--by volume|errors|repeat|unusual|text] [--when PHRASE] [--per task|session|writer]
           [--window 5m|1h|1d] [--writer W]... [-k N] [--samples N] [--include-waiting] [--from T] [--to T]
                                                           stretches of activity ranked by how they looked
  forget   --id ID | --key K | --older-than T | --last DUR   [--writer W] [--no-proof] [--yes]
  cloud connect-api --connection FILE --api-key-file PRIVATE_FILE --label NAME [--base-url URL]
  cloud connect-project --connection FILE --project ID --read-key-file PRIVATE_FILE [--base-url URL]
  cloud connect --connection FILE --source ID --journal ID --label NAME [--base-url URL]
  cloud connection | capture-status | capture-check --connection FILE
  cloud capture-local-status --connection FILE --state DIR  verified local transcript acknowledgement counters
  cloud capture --connection FILE --state DIR --source-root DIR --adapter codex|claude-code --accept-local-capture [--once]
  cloud mcp --connection FILE                              project-scoped hosted history tools over stdio
  cloud records --connection FILE [--from T] [--through T] [--agent ID] [--limit N] [--cursor-file FILE]
  cloud search --connection FILE --request-id ID --query-file FILE [--from T] [--through T] [--agent ID] [--k N]
  cloud get --connection FILE --journal ID --id ID          hosted project metadata / exact text
  stats                                                    what the journal holds
  export   [--from T] [--to T] [--when PHRASE] [--kind K]... [--writer W]... [--key K]   JSON lines on stdout
  pause | resume                                           stop / restart recording
  inject   on|off [--agent W]                              injection before each turn (Claude Code hooks)
  config   get [KEY] [--agent W] | set KEY VALUE [--project | --agent W] | path
  mcp                                                      MCP server on stdio (remember, search, get, forget)
  hook     EVENT                                           Claude Code hook (JSON on stdin; always exits 0)
  hooks    print                                           the Claude Code settings entry for the hooks

TIME: now, RFC 3339, YYYY-MM-DD, @unix-seconds, or a duration ago (90s, 15m, 1h, 7d).
PHRASE (--when, in the `timezone` setting or the machine's zone): yesterday, last tuesday afternoon,
  past 3 days, oct 14, since monday, between X and Y; anchors found by search: before \"deploy\",
  after the first \"failed test\", the day \"build broke\", around @4211 (--tz Z, --span DUR).
Writer: --writer, else $OOJURO_WRITER, else human:<user>.";

struct Cli {
    paths: Paths,
    /// The location options given (repeated in the hook commands `hooks print` shows).
    globals: Vec<String>,
    direct: bool,
    json: bool,
    cmd: String,
    args: Vec<String>,
}

fn parse() -> Result<Cli> {
    let mut it = std::env::args().skip(1).peekable();
    let (mut home, mut store, mut socket, mut config) = (None, None, None, None);
    let (mut direct, mut json) = (false, false);
    let mut cmd = None;
    let mut args = Vec::new();
    let mut globals = Vec::new();
    while let Some(x) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        let mut global = |name: &str, v: String| {
            globals.extend([name.to_string(), v.clone()]);
            Some(PathBuf::from(v))
        };
        match x.as_str() {
            "--home" => home = global("--home", val("--home")?),
            "--store" => store = global("--store", val("--store")?),
            "--socket" => socket = global("--socket", val("--socket")?),
            "--config" => config = global("--config", val("--config")?),
            "--direct" => direct = true,
            "--json" => json = true,
            "-h" | "--help" | "help" if cmd.is_none() => return Err(USAGE.into()),
            _ if cmd.is_none() => cmd = Some(x),
            _ => args.push(x),
        }
    }
    let paths = Paths::resolve(home, store, socket, config)?;
    Ok(Cli { paths, globals, direct, json, cmd: cmd.ok_or(USAGE)?, args })
}

/// Command options: flags with values, repeated flags, switches, and the rest.
struct Opts {
    vals: Vec<(String, String)>,
    switches: Vec<String>,
    rest: Vec<String>,
}

impl Opts {
    fn parse(args: &[String], with_value: &[&str], switches: &[&str]) -> Result<Self> {
        let mut o = Opts { vals: Vec::new(), switches: Vec::new(), rest: Vec::new() };
        let mut it = args.iter();
        let mut only_rest = false;
        while let Some(a) = it.next() {
            if only_rest {
                o.rest.push(a.clone());
            } else if a == "--" {
                only_rest = true;
            } else if with_value.contains(&a.as_str()) {
                let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
                o.vals.push((a.clone(), v.clone()));
            } else if switches.contains(&a.as_str()) {
                o.switches.push(a.clone());
            } else if a.starts_with("--") || (a.starts_with('-') && a.len() == 2 && a != "-") {
                return Err(format!("unknown option {a}\n\n{USAGE}").into());
            } else {
                o.rest.push(a.clone());
            }
        }
        Ok(o)
    }
    fn one(&self, k: &str) -> Option<&str> {
        self.vals.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
    fn all(&self, k: &str) -> Vec<String> {
        self.vals.iter().filter(|(n, _)| n == k).map(|(_, v)| v.clone()).collect()
    }
    fn has(&self, k: &str) -> bool {
        self.switches.iter().any(|s| s == k)
    }
    fn time(&self, k: &str) -> Result<Option<i64>> {
        self.one(k).map(parse_time).transpose()
    }
}

/// The words given, or stdin for `-` / nothing.
fn text_arg(rest: &[String], what: &str) -> Result<String> {
    if rest.is_empty() || rest == ["-"] {
        if std::io::stdin().is_terminal() {
            return Err(format!("give the {what} as arguments, or pipe it in with -").into());
        }
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        return Ok(s);
    }
    Ok(rest.join(" "))
}

fn one_line(s: &str) -> String {
    s.chars().map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).collect()
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn print_ref(r: &Ref) {
    let key = r.key.as_deref().map(|k| format!(" key={k}")).unwrap_or_default();
    println!("#{}  {:.1} bits  {}  {}  {}{}", r.id, r.score_bits + 0.0, r.ts, r.writer, r.kind, key);
    println!("    [{}..{} of {}] {}", r.start, r.end, r.len, one_line(&r.excerpt));
}

fn print_record(r: &Record, header_only: bool) {
    let key = r.key.as_deref().map(|k| format!(" key={k} v{}", r.seq)).unwrap_or_default();
    let tool = if r.tool.is_empty() { String::new() } else { format!(" tool={}", r.tool) };
    println!("#{}  {}  {}  {}{}{}  ({} bytes)", r.id, r.ts, r.writer, r.kind, key, tool, r.text.len());
    if !header_only {
        println!("{}", r.text);
    }
}

fn filters(o: &Opts) -> Result<Filters> {
    Ok(Filters {
        from_us: o.time("--from")?,
        to_us: o.time("--to")?,
        as_of_us: o.time("--as-of")?,
        kinds: o.all("--kind"),
        key: o.one("--key").map(str::to_string),
        writers: o.all("--writer"),
        outside_context_of: None,
    })
}

fn settings(cli: &Cli, writer: Option<&str>) -> Result<config::Resolved> {
    Config::discover(&cli.paths.config, &std::env::current_dir()?)?.resolve(writer)
}

/// The time zone for `--when`: `--tz`, else the `timezone` setting, else the machine's.
fn tz(cli: &Cli, o: &Opts) -> Result<Option<String>> {
    if let Some(z) = o.one("--tz") {
        return Ok(Some(z.to_string()));
    }
    let z = settings(cli, None)?.settings.timezone;
    Ok((!z.is_empty()).then_some(z))
}

/// Resolve `--when` (if given) and narrow the filters to it; the meaning
/// is echoed on stderr.
fn apply_when(cli: &Cli, oj: &Oojuro, o: &Opts, f: &mut Filters) -> Result<Option<When>> {
    let Some(p) = o.one("--when") else { return Ok(None) };
    let scope = Scope {
        writers: f.writers.clone(),
        outside_context_of: o.one("--outside-context-of").map(str::to_string),
        span_us: o.one("--span").map(parse_duration).transpose()?,
        tz: tz(cli, o)?,
    };
    let w = oj.when(p, &scope)?;
    when::narrow(f, &w);
    eprintln!("when: {}", when_line(&w));
    Ok(Some(w))
}

fn when_line(w: &When) -> String {
    let mut s = format!("{:?} = {} ({})", w.phrase, w.local, w.tz);
    for a in &w.anchors {
        s.push_str(&format!("; anchor {:?} = #{} at {} ({})", a.query, a.id, a.ts, a.note));
    }
    s
}

fn local_time(us: i64, tz: &jiff::tz::TimeZone, fmt: &str) -> String {
    jiff::Timestamp::from_microsecond(us).map(|t| t.to_zoned(tz.clone()).strftime(fmt).to_string()).unwrap_or_default()
}

fn main() {
    // a hook never fails or blocks the agent: whatever happens, exit 0
    let hook = std::env::args().skip(1).find(|a| !a.starts_with('-')).is_some_and(|c| c == "hook");
    match run() {
        Ok(code) => std::process::exit(if hook { 0 } else { code }),
        Err(e) => {
            eprintln!("oojuro: {e}");
            std::process::exit(if hook { 0 } else { 2 });
        }
    }
}

fn open(cli: &Cli) -> Oojuro {
    if cli.direct { Oojuro::direct(cli.paths.clone()) } else { Oojuro::open(cli.paths.clone()) }
}

fn cloud(cli: &Cli) -> Result<()> {
    if cli.args.first().is_some_and(|c| c == "connect-project") {
        let o = Opts::parse(&cli.args, &["--connection", "--project", "--read-key-file", "--base-url"], &[])?;
        let names: std::collections::BTreeSet<_> = o.vals.iter().map(|(key, _)| key).collect();
        if names.len() != o.vals.len() || o.rest != ["connect-project"] {
            return Err("invalid_project_connection_arguments".into());
        }
        oojuro::hosted::connect_project(
            std::path::Path::new(o.one("--connection").ok_or("missing_connection")?),
            o.one("--base-url").unwrap_or("https://oojuro.com"),
            o.one("--project").ok_or("missing_project")?,
            std::path::Path::new(o.one("--read-key-file").ok_or("missing_read_key_file")?),
        )?;
        println!("{{\"project_read_connection_saved\":true}}");
        return Ok(());
    }
    if cli.args.first().is_some_and(|c| matches!(c.as_str(), "connect-api" | "connect" | "connection" | "capture-status" | "capture-local-status" | "capture-check" | "capture")) {
        return capture_command(cli);
    }
    if cli.args.first().is_some_and(|c| c == "mcp") {
        let options = Opts::parse(&cli.args, &["--connection"], &[])?;
        if options.vals.len() != 1 || options.rest != ["mcp"] {
            return Err("invalid_hosted_mcp_arguments".into());
        }
        let path = std::path::Path::new(options.one("--connection").ok_or("missing_connection")?);
        return oojuro::hosted_mcp::HostedMcpServer::open(path)?
            .serve(std::io::stdin().lock(), std::io::stdout().lock());
    }
    use oojuro::hosted::{Error, ProjectClient, Selection, read_private};
    let o = Opts::parse(
        &cli.args,
        &[
            "--connection",
            "--from",
            "--through",
            "--agent",
            "--limit",
            "--cursor-file",
            "--journal",
            "--id",
            "--request-id",
            "--query-file",
            "--k",
        ],
        &[],
    )
    .map_err(|_| Error::Request)?;
    let names: std::collections::BTreeSet<_> = o.vals.iter().map(|(key, _)| key).collect();
    if names.len() != o.vals.len() || o.rest.len() != 1 {
        return Err(Error::Request.into());
    }
    let allowed: &[&str] = match o.rest[0].as_str() {
        "records" => &[
            "--connection",
            "--from",
            "--through",
            "--agent",
            "--limit",
            "--cursor-file",
        ],
        "get" => &["--connection", "--journal", "--id"],
        "search" => &["--connection", "--request-id", "--query-file", "--from", "--through", "--agent", "--k"],
        _ => return Err(Error::Request.into()),
    };
    if o.vals
        .iter()
        .any(|(key, _)| !allowed.contains(&key.as_str()))
    {
        return Err(Error::Request.into());
    }
    let connection = o.one("--connection").ok_or(Error::Request)?;
    let output = if o.rest[0] == "records" {
        let cursor = o
            .one("--cursor-file")
            .map(|p| {
                let bytes = read_private(std::path::Path::new(p), 2 * 1024 * 1024 + 1)?;
                let text = String::from_utf8(bytes).map_err(|_| Error::Request)?;
                Ok::<_, Error>(text.strip_suffix('\n').unwrap_or(&text).to_owned())
            })
            .transpose()?;
        let selection = Selection {
            from: o.time("--from").map_err(|_| Error::Request)?,
            through: o.time("--through").map_err(|_| Error::Request)?,
            agent_id: o.one("--agent").map(str::to_owned),
            limit: o
                .one("--limit")
                .map(|v| v.parse().map_err(|_| Error::Request))
                .transpose()?,
            cursor,
        };
        let page = ProjectClient::open(std::path::Path::new(connection))?.records(&selection)?;
        let mut bytes = serde_json::to_vec(&page).map_err(|_| Error::Response)?;
        bytes.push(b'\n');
        bytes
    } else if o.rest[0] == "search" {
        let request=o.one("--request-id").ok_or(Error::Request)?;
        let text=String::from_utf8(read_private(std::path::Path::new(o.one("--query-file").ok_or(Error::Request)?),4096)?).map_err(|_|Error::Request)?;
        let selection=Selection { from:o.time("--from").map_err(|_|Error::Request)?, through:o.time("--through").map_err(|_|Error::Request)?,
            agent_id:o.one("--agent").map(str::to_owned), ..Selection::default() };
        let k=o.one("--k").map(|v|v.parse().map_err(|_|Error::Request)).transpose()?.unwrap_or(20);
        let page=ProjectClient::open(std::path::Path::new(connection))?.search(request,&text,&selection,k)?;
        let mut bytes=serde_json::to_vec(&page).map_err(|_|Error::Response)?;bytes.push(b'\n');bytes
    } else {
        let journal = o.one("--journal").ok_or(Error::Request)?;
        let id = o.one("--id").ok_or(Error::Request)?;
        let text = ProjectClient::open(std::path::Path::new(connection))?.content(journal, id)?;
        if cli.json {
            let mut bytes = serde_json::to_vec(&text).map_err(|_| Error::Response)?;
            bytes.push(b'\n');
            bytes
        } else {
            text.into_bytes()
        }
    };
    std::io::stdout().lock().write_all(&output)?;
    Ok(())
}

fn capture_command(cli: &Cli) -> Result<()> {
    use oojuro::hosted_capture as capture;
    let o = Opts::parse(
        &cli.args,
        &[
            "--api-key-file",
            "--connection",
            "--base-url",
            "--source",
            "--journal",
            "--label",
            "--state",
            "--source-root",
            "--adapter",
        ],
        &["--once", "--accept-local-capture"],
    )?;
    let names: std::collections::BTreeSet<_> = o.vals.iter().map(|(k, _)| k).collect();
    if names.len() != o.vals.len() || o.rest.len() != 1 {
        return Err("invalid_capture_arguments".into());
    }
    let command = o.rest[0].as_str();
    let allowed: &[&str] = match command {
        "connect-api" => &["--connection","--api-key-file","--base-url","--label"],
        "connect" => &[
            "--connection",
            "--base-url",
            "--source",
            "--journal",
            "--label",
        ],
        "connection" | "capture-status" | "capture-check" => &["--connection"],
        "capture-local-status" => &["--connection","--state"],
        "capture" => &["--connection", "--state", "--source-root", "--adapter"],
        _ => return Err("invalid_capture_command".into()),
    };
    if o.vals.iter().any(|(k, _)| !allowed.contains(&k.as_str()))
        || command != "capture" && !o.switches.is_empty()
    {
        return Err("invalid_capture_arguments".into());
    }
    let val = |name: &str| o.one(name).ok_or("missing_capture_argument");
    let connection = std::path::Path::new(val("--connection")?);
    let value = match command {
        "connect-api" => serde_json::to_value(capture::connect_api(connection,
            o.one("--base-url").unwrap_or("https://oojuro.com"),
            std::path::Path::new(val("--api-key-file")?),val("--label")?)?)?,
        "connect" => serde_json::to_value(capture::connect(
            connection,
            o.one("--base-url").unwrap_or("https://oojuro.com"),
            val("--source")?,
            val("--journal")?,
            val("--label")?,
        )?)?,
        "connection" => serde_json::to_value(capture::resume_approval(connection)?)?,
        "capture-status" => serde_json::to_value(capture::status(connection)?)?,
        "capture-local-status" => serde_json::to_value(capture::local_status(connection,std::path::Path::new(val("--state")?))?)?,
        "capture-check" => {
            capture::verify_delivery(connection)?;
            serde_json::json!({"connection_check_accepted":true,"real_history_verified":false})
        }
        "capture" => {
            if !o.has("--accept-local-capture") {
                return Err("explicit_local_capture_consent_required".into());
            }
            serde_json::to_value(capture::capture(
                connection,
                std::path::Path::new(val("--state")?),
                std::path::Path::new(val("--source-root")?),
                val("--adapter")?,
                o.has("--once"),
            )?)?
        }
        _ => unreachable!(),
    };
    println!("{}", serde_json::to_string(&value)?);
    Ok(())
}

fn run() -> Result<i32> {
    let cli = parse()?;
    let a = &cli.args;
    match cli.cmd.as_str() {
        "cloud" => cloud(&cli)?,
        "remember" | "put" => {
            let o = Opts::parse(a, &["--key", "--writer", "--at"], &[])?;
            let writer = paths::writer(o.one("--writer"), false);
            let text = text_arg(&o.rest, "text")?;
            let r = open(&cli).remember(&writer, &text, o.one("--key"), o.time("--at")?)?;
            if cli.json {
                print_json(&r)?;
            } else {
                let key = r.key.as_deref().map(|k| format!(", key {k} v{}", r.version)).unwrap_or_default();
                println!("remembered #{} ({}{key}, {}, {} bytes)", r.id, r.writer, r.ts, r.bytes);
                if r.redacted > 0 {
                    println!("masked {} secret(s) before storing", r.redacted);
                }
            }
        }
        "search" => {
            let o = Opts::parse(
                a,
                &[
                    "-k",
                    "--kind",
                    "--key",
                    "--writer",
                    "--from",
                    "--to",
                    "--as-of",
                    "--excerpt-bytes",
                    "--when",
                    "--tz",
                    "--span",
                    "--outside-context-of",
                ],
                &[],
            )?;
            let query = text_arg(&o.rest, "query")?;
            let s = settings(&cli, o.all("--writer").first().map(String::as_str))?;
            let oj = open(&cli);
            let mut f = filters(&o)?;
            apply_when(&cli, &oj, &o, &mut f)?;
            let args = SearchArgs {
                query,
                k: o.one("-k").map(str::parse).transpose()?.unwrap_or(DEFAULT_K),
                filters: f,
                excerpt_bytes: o.one("--excerpt-bytes").map(str::parse).transpose()?.unwrap_or(DEFAULT_EXCERPT_BYTES),
                max_query_bytes: s.settings.max_query_bytes as usize,
            };
            let refs = oj.search(&args)?;
            if cli.json {
                print_json(&refs)?;
            } else if refs.is_empty() {
                println!("no matches");
            } else {
                refs.iter().for_each(print_ref);
            }
        }
        "get" => {
            let o = Opts::parse(a, &["--key", "--writer", "--as-of"], &[])?;
            let oj = open(&cli);
            let rec = match (o.one("--key"), o.rest.as_slice()) {
                (Some(k), []) => oj.get_key(&paths::writer(o.one("--writer"), false), k, o.time("--as-of")?)?,
                (None, [id]) => {
                    oj.get_id(id.trim_start_matches('#').parse().map_err(|_| format!("not a record id: {id}"))?)?
                }
                _ => return Err("get takes a record id or --key K".into()),
            };
            match rec {
                Some(r) if cli.json => print_json(&r)?,
                Some(r) => print_record(&r, false),
                None => {
                    eprintln!("oojuro: no such record");
                    return Ok(1);
                }
            }
        }
        "history" => {
            let o = Opts::parse(a, &["--writer"], &[])?;
            let [key] = o.rest.as_slice() else { return Err("history takes one key".into()) };
            let v = open(&cli).history(&paths::writer(o.one("--writer"), false), key)?;
            if cli.json {
                print_json(&v)?;
            } else if v.is_empty() {
                println!("no versions of {key}");
            } else {
                for r in &v {
                    print_record(r, true);
                    println!("    {}", one_line(&r.text));
                }
            }
        }
        "range" => {
            let o = Opts::parse(
                a,
                &[
                    "--from",
                    "--to",
                    "--as-of",
                    "--kind",
                    "--writer",
                    "--key",
                    "--limit",
                    "--when",
                    "--tz",
                    "--span",
                    "--outside-context-of",
                ],
                &["--text"],
            )?;
            let mut f = filters(&o)?;
            if let Some(x) = o.rest.first() {
                f.from_us = Some(parse_time(x)?);
            }
            if let Some(x) = o.rest.get(1) {
                f.to_us = Some(parse_time(x)?);
            }
            let limit = o.one("--limit").map(str::parse).transpose()?.unwrap_or(100);
            let oj = open(&cli);
            apply_when(&cli, &oj, &o, &mut f)?;
            let v = oj.range(&f, limit)?;
            if cli.json {
                print_json(&v)?;
            } else {
                for r in &v {
                    print_record(r, true);
                    if o.has("--text") {
                        println!("{}", r.text);
                    } else {
                        let line = one_line(&r.text);
                        let cut: String = line.chars().take(160).collect();
                        println!("    {cut}{}", if line.chars().count() > 160 { " ..." } else { "" });
                    }
                }
                if v.len() == limit {
                    eprintln!("(first {limit}; --limit N for more)");
                }
            }
        }
        "forget" => {
            let o = Opts::parse(
                a,
                &["--id", "--key", "--older-than", "--last", "--writer"],
                &["--no-proof", "--yes", "-y"],
            )?;
            let writer = o.one("--writer").map(str::to_string);
            let given = ["--id", "--key", "--older-than", "--last"].iter().filter(|k| o.one(k).is_some()).count();
            if given != 1 || !o.rest.is_empty() {
                return Err("forget takes exactly one of --id ID, --key K, --older-than T, --last DUR".into());
            }
            let (target, f) = if let Some(id) = o.one("--id") {
                (
                    Target::Id {
                        id: id.trim_start_matches('#').parse().map_err(|_| format!("not a record id: {id}"))?,
                    },
                    None,
                )
            } else if let Some(k) = o.one("--key") {
                let w = paths::writer(writer.as_deref(), false);
                (
                    Target::Key { key: k.into(), writer: w.clone() },
                    Some(Filters { key: Some(k.into()), writers: vec![w], ..Filters::default() }),
                )
            } else if let Some(t) = o.one("--older-than") {
                let before = parse_time(t)?;
                (
                    Target::OlderThan { before_us: before, writer: writer.clone() },
                    Some(Filters {
                        to_us: Some(before - 1),
                        writers: writer.iter().cloned().collect(),
                        ..Filters::default()
                    }),
                )
            } else {
                let d = parse_duration(o.one("--last").unwrap())?;
                let now = oojuro_capture::time::now_us();
                (
                    Target::Window { from_us: now - d, to_us: i64::MAX / 2, writer: writer.clone() },
                    Some(Filters {
                        from_us: Some(now - d),
                        writers: writer.iter().cloned().collect(),
                        ..Filters::default()
                    }),
                )
            };
            let oj = open(&cli);
            if let Some(f) = &f
                && !(o.has("--yes") || o.has("-y"))
                && std::io::stdin().is_terminal()
            {
                let n = oj.count(f)?;
                if n == 0 {
                    println!("nothing to forget");
                    return Ok(0);
                }
                let scope = writer.as_deref().map_or("every writer".to_string(), |w| w.to_string());
                print!("forget {n} record(s) of {scope} for good? [y/N] ");
                std::io::stdout().flush()?;
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                if !matches!(line.trim(), "y" | "Y" | "yes") {
                    println!("kept everything");
                    return Ok(1);
                }
            }
            let r = oj.forget(&target, !o.has("--no-proof"))?;
            if cli.json {
                print_json(&r)?;
            } else {
                println!(
                    "forgot {} record(s){}",
                    r.forgotten,
                    if r.writers.is_empty() { String::new() } else { format!(" of {}", r.writers.join(", ")) }
                );
                if let Some(p) = &r.proof {
                    println!(
                        "proof: {} ({} needle text(s); {} rows decoded: {} still hold it, {} only because another live record has the same text; {} files read: {} contain it)",
                        if p.proven { "the text is gone" } else { "NOT PROVEN" },
                        p.needles,
                        p.rows_scanned,
                        p.dead_rows_with_text,
                        p.live_rows_with_text,
                        p.files_scanned,
                        p.files_with_text
                    );
                    if !p.proven {
                        return Ok(3);
                    }
                }
            }
        }
        "stats" => {
            let s = open(&cli).stats()?;
            let r = settings(&cli, None)?;
            if cli.json {
                let mut v = serde_json::to_value(&s)?;
                v["settings"] =
                    serde_json::json!({ "record": r.settings.record, "paused": r.paused, "inject": r.settings.inject });
                print_json(&v)?;
            } else {
                println!("journal {} (served by {})", s.store, s.served_by);
                println!(
                    "recording {}, injection {}",
                    if r.paused {
                        "PAUSED"
                    } else if r.settings.record {
                        "on"
                    } else {
                        "off"
                    },
                    if r.settings.inject { "on" } else { "off" }
                );
                println!(
                    "{} records, {} writers, {:.1} MB on disk",
                    s.records,
                    s.writers.len(),
                    s.disk_bytes as f64 / 1e6
                );
                println!("by kind: {}", s.kinds.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", "));
                for w in &s.writers {
                    println!(
                        "  {:<56} {:>9}  {}",
                        w.writer,
                        w.records,
                        w.kinds.iter().map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join(" ")
                    );
                }
            }
        }
        "export" => {
            let o = Opts::parse(
                a,
                &[
                    "--from",
                    "--to",
                    "--as-of",
                    "--kind",
                    "--writer",
                    "--key",
                    "--when",
                    "--tz",
                    "--span",
                    "--outside-context-of",
                ],
                &[],
            )?;
            let mut f = filters(&o)?;
            let oj = open(&cli);
            apply_when(&cli, &oj, &o, &mut f)?;
            let out = std::io::stdout();
            let mut w = std::io::BufWriter::new(out.lock());
            let n = oj.export(&f, &mut |r| {
                serde_json::to_writer(&mut w, &r)?;
                w.write_all(b"\n")?;
                Ok(())
            })?;
            w.flush()?;
            eprintln!("exported {n} record(s)");
        }
        "periods" => return periods_cmd(&cli),
        "pause" | "resume" => {
            let paused = cli.cmd == "pause";
            config::set_value(&cli.paths.config, None, config::PAUSED, &toml::Value::Boolean(paused))?;
            if paused {
                println!(
                    "recording paused: oojurod skips what agents do until `oojuro resume` ({})",
                    cli.paths.config.display()
                );
            } else {
                println!("recording resumed ({})", cli.paths.config.display());
            }
        }
        "inject" => {
            let o = Opts::parse(a, &["--agent"], &["--project"])?;
            let [v] = o.rest.as_slice() else { return Err("inject on|off".into()) };
            let value = config::parse_cli_value("inject", v)?;
            let path = target_file(&cli, &o)?;
            config::set_value(&path, o.one("--agent"), "inject", &value)?;
            println!(
                "inject = {value} in {} (used by `oojuro hook UserPromptSubmit`; `oojuro hooks print` shows the setup)",
                path.display()
            );
        }
        "config" => return config_cmd(&cli),
        "hook" => return Ok(hook_cmd(&cli)),
        "hooks" => {
            let exe = std::env::current_exe()?;
            let mut cmd = vec![oojuro::hook::shell_quote(&exe.to_string_lossy())];
            cmd.extend(cli.globals.iter().map(|g| oojuro::hook::shell_quote(g)));
            match a.first().map(String::as_str) {
                Some("print") | None => print_json(&oojuro::hook::settings_snippet(&cmd.join(" ")))?,
                Some(other) => return Err(format!("hooks print, not {other}").into()),
            }
        }
        "mcp" => {
            let o = Opts::parse(a, &["--writer"], &[])?;
            let writer = paths::writer(o.one("--writer"), true);
            paths::check_writer(&writer)?;
            let server = McpServer::new(cli.paths.clone(), writer);
            let stdin = std::io::stdin();
            server.serve(stdin.lock(), std::io::stdout().lock())?;
        }
        other => return Err(format!("unknown command {other}\n\n{USAGE}").into()),
    }
    Ok(0)
}

/// `oojuro hook EVENT`: the hook JSON on stdin, the injection (if any) on
/// stdout, problems on stderr; never fails.
fn hook_cmd(cli: &Cli) -> i32 {
    let mut input = String::new();
    if let Err(e) = std::io::stdin().take(oojuro::hook::MAX_INPUT_BYTES).read_to_string(&mut input) {
        eprintln!("oojuro hook: reading stdin: {e}");
        return 0;
    }
    let event = cli.args.first().map(String::as_str);
    let run = std::panic::catch_unwind(|| oojuro::hook::run(event, &cli.paths, &input));
    match run {
        Ok(out) => {
            for e in &out.errors {
                eprintln!("oojuro hook {}: {e}", out.event);
            }
            if !out.stdout.is_empty() {
                let mut so = std::io::stdout().lock();
                let _ = so.write_all(out.stdout.as_bytes()).and_then(|_| so.flush());
            }
        }
        Err(_) => eprintln!("oojuro hook: internal error (nothing recorded or injected)"),
    }
    0
}

/// The file `config set` / `inject` writes: global, or the project's.
fn target_file(cli: &Cli, o: &Opts) -> Result<PathBuf> {
    if o.has("--project") {
        let cwd = std::env::current_dir()?;
        return Ok(paths::find_project_config(&cwd).unwrap_or_else(|| cwd.join(paths::PROJECT_CONFIG)));
    }
    Ok(cli.paths.config.clone())
}

fn config_cmd(cli: &Cli) -> Result<i32> {
    let o = Opts::parse(&cli.args, &["--agent", "--writer"], &["--project"])?;
    let agent = o.one("--agent").or(o.one("--writer"));
    match o.rest.first().map(String::as_str) {
        Some("get") | None => {
            let r = settings(cli, agent)?;
            let v = serde_json::to_value(&r.settings)?;
            let keys: Vec<&str> = match o.rest.get(1) {
                Some(k) if config::KEYS.contains(&k.as_str()) => vec![k.as_str()],
                Some(k) if k == config::PAUSED => vec![],
                Some(k) => {
                    return Err(format!("unknown setting {k} (known: {}, paused)", config::KEYS.join(", ")).into());
                }
                None => config::KEYS.to_vec(),
            };
            if cli.json {
                let mut out = serde_json::Map::new();
                for k in &keys {
                    out.insert(k.to_string(), serde_json::json!({ "value": v[k], "source": r.sources[*k] }));
                }
                if o.rest.get(1).is_none_or(|k| k == config::PAUSED) {
                    out.insert("paused".into(), serde_json::json!({ "value": r.paused, "source": cli.paths.config }));
                }
                print_json(&out)?;
            } else {
                for k in &keys {
                    println!("{k} = {}    # {}", v[k], r.sources[*k]);
                }
                if o.rest.get(1).is_none_or(|k| k == config::PAUSED) {
                    println!("paused = {}    # {}", r.paused, cli.paths.config.display());
                }
            }
            for w in &r.warnings {
                eprintln!("warning: {w}");
            }
        }
        Some("set") => {
            let (Some(k), Some(raw)) = (o.rest.get(1), o.rest.get(2)) else {
                return Err("config set KEY VALUE".into());
            };
            let value = config::parse_cli_value(k, raw)?;
            let path = target_file(cli, &o)?;
            config::set_value(&path, agent, k, &value)?;
            let section = agent.map(|w| format!(" [agents.\"{w}\"]")).unwrap_or_default();
            println!("{k} = {value} in {}{section}", path.display());
        }
        Some("path") => {
            println!("global  {}", cli.paths.config.display());
            match paths::find_project_config(&std::env::current_dir()?) {
                Some(p) => println!("project {}", p.display()),
                None => println!("project (none: no .oojuro.toml at or above this directory)"),
            }
        }
        Some(other) => return Err(format!("config get|set|path, not {other}").into()),
    }
    Ok(0)
}

fn periods_cmd(cli: &Cli) -> Result<i32> {
    let o = Opts::parse(
        &cli.args,
        &[
            "--by",
            "--when",
            "--per",
            "--window",
            "--writer",
            "-k",
            "--samples",
            "--from",
            "--to",
            "--tz",
            "--outside-context-of",
            "--min-bits",
        ],
        &["--include-waiting"],
    )?;
    let query = if o.rest.is_empty() { None } else { Some(text_arg(&o.rest, "query")?) };
    let s = settings(cli, o.all("--writer").first().map(String::as_str))?;
    let tz = tz(cli, &o)?;
    let args = PeriodsArgs {
        query,
        by: o.one("--by").map(By::parse).transpose()?,
        per: o.one("--per").map(Per::parse).transpose()?.unwrap_or_default(),
        window_us: o.one("--window").map(parse_duration).transpose()?,
        from_us: o.time("--from")?,
        to_us: o.time("--to")?,
        when: o.one("--when").map(str::to_string),
        writers: o.all("--writer"),
        k: o.one("-k").map(str::parse).transpose()?.unwrap_or(10),
        samples: o.one("--samples").map(str::parse).transpose()?.unwrap_or(3),
        include_waiting: o.has("--include-waiting"),
        outside_context_of: o.one("--outside-context-of").map(str::to_string),
        tz: tz.clone(),
        min_bits: o.one("--min-bits").map(str::parse).transpose()?.unwrap_or(s.settings.min_bits),
        max_query_bytes: s.settings.max_query_bytes as usize,
    };
    let r = open(cli).periods(&args)?;
    if cli.json {
        print_json(&r)?;
        return Ok(0);
    }
    let z = when::time_zone(tz.as_deref())?;
    let by = serde_json::to_value(r.by)?.as_str().unwrap_or("").to_string();
    let per = serde_json::to_value(r.per)?.as_str().unwrap_or("").to_string();
    println!(
        "periods by {by}, {} windows per {per}, {} .. {} ({:.1} ms, {} records read)",
        r.window,
        local_time(r.from_us, &z, "%Y-%m-%d %H:%M"),
        local_time(r.to_us, &z, "%Y-%m-%d %H:%M %Z"),
        r.elapsed_ms,
        r.scanned
    );
    if let Some(w) = &r.when {
        println!("when: {}", when_line(w));
    }
    if r.periods.is_empty() {
        println!("no periods");
    }
    for p in &r.periods {
        let unit = if p.score_unit == "bits" { "bits" } else { "records" };
        let who = if p.agent_id.is_empty() {
            String::new()
        } else if per == "task" {
            format!("  task {} ({} agent(s))", p.agent_id, p.agent_count)
        } else {
            format!("  agent {}", p.agent_id)
        };
        println!(
            "#{}  {:.1} {unit}  {} .. {} ({:.0} min)  {}{who}",
            p.rank,
            p.score,
            local_time(p.from_us, &z, "%a %Y-%m-%d %H:%M"),
            local_time(p.to_us, &z, "%H:%M"),
            p.minutes,
            p.writers.join(", ")
        );
        let mut line = format!(
            "    {} records, {} calls, {} errors{}, repeats {} stuck / {} waiting{}{}",
            p.events,
            p.calls,
            p.errors,
            if p.error_bits > 0.0 { format!(" ({:.1} bits)", p.error_bits) } else { String::new() },
            p.repeats_stuck,
            p.repeats_waiting,
            if p.repeat_bits > 0.0 { format!(" ({:.1} bits)", p.repeat_bits) } else { String::new() },
            if p.repeats_exact { " exact" } else { " (same tool and input length)" },
        );
        if p.unusual_bits > 0.0 {
            let u = &p.unusual;
            line.push_str(&format!(
                ", unusual {:.1} bits (rate {:.1}, mix {:.1}, errors {:.1}, new tools {:.1}, size {:.1})",
                p.unusual_bits, u.rate, u.mix, u.errors, u.new, u.size
            ));
        }
        println!("{line}");
        if !p.tools.is_empty() {
            let mut t: Vec<(&String, &u64)> = p.tools.iter().collect();
            t.sort_by(|a, b| b.1.cmp(a.1));
            println!("    tools: {}", t.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", "));
        }
        if !p.samples.is_empty() {
            println!(
                "    samples: {}",
                p.samples
                    .iter()
                    .map(|x| format!(
                        "#{} {} {}{}",
                        x.id,
                        x.why,
                        x.kind,
                        if x.tool.is_empty() { String::new() } else { format!(" {}", x.tool) }
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let w = p.writers.first().map(|w| format!(" --writer {w}")).unwrap_or_default();
        println!("    drill: oojuro range {} {}{w}   (or oojuro get ID)", p.from, p.to);
    }
    Ok(0)
}
