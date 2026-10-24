//! Hosted capture and project history CLI. Local journals use `oojuro-native`.
use oojuro::timearg::parse_time;
use std::io::Write;
type Result<T> = oojuro::Result<T>;
const USAGE: &str = "usage: oojuro [--json] cloud <command> [options]

  cloud connect-api --connection FILE --api-key-file PRIVATE_FILE --label NAME [--base-url URL] [--reuse-journal ID]
  cloud capture-policy --connection FILE (--reuse-journal ID | --new-journal-per-session)
  cloud connect-project --connection FILE --project ID --read-key-file PRIVATE_FILE [--base-url URL]
  cloud connect --connection FILE --source ID --journal ID --label NAME [--base-url URL]
  cloud connection | capture-status | capture-check --connection FILE
  cloud capture-local-status --connection FILE --state DIR  verified local transcript acknowledgement counters
  cloud capture --connection FILE --state DIR --source-root DIR --adapter codex|claude-code --accept-local-capture [--once]
  cloud mcp --connection FILE                              project-scoped hosted history tools over stdio
  cloud records --connection FILE [--from T] [--through T] [--agent ID] [--limit N] [--cursor-file FILE]
  cloud search --connection FILE --request-id ID --query-file FILE [--from T] [--through T] [--agent ID] [--k N]
  cloud get --connection FILE --journal ID --id ID          hosted project metadata / exact text

Local journals: oojuro-native <command>. See docs/LOCAL_JOURNAL_QUICKSTART.md.";
struct Cli {
    json: bool,
    args: Vec<String>,
}
fn main() {
    if let Err(error) = run() {
        eprintln!("oojuro: {error}");
        std::process::exit(2);
    }
}
fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice().is_empty()
        || matches!(
            args.first().map(String::as_str),
            Some("help" | "--help" | "-h")
        )
    {
        println!("{USAGE}");
        return Ok(());
    }
    let json = args.first().is_some_and(|arg| arg == "--json");
    if json {
        args.remove(0);
    }
    if args.first().map(String::as_str) != Some("cloud") {
        return Err(format!("unsupported_command\n{USAGE}").into());
    }
    args.remove(0);
    cloud(&Cli { json, args })
}

/// Command options: flags with values, repeated flags, switches, and the rest.
struct Opts {
    vals: Vec<(String, String)>,
    switches: Vec<String>,
    rest: Vec<String>,
}

impl Opts {
    fn parse(args: &[String], with_value: &[&str], switches: &[&str]) -> Result<Self> {
        let mut o = Opts {
            vals: Vec::new(),
            switches: Vec::new(),
            rest: Vec::new(),
        };
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
        self.vals
            .iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }
    fn has(&self, k: &str) -> bool {
        self.switches.iter().any(|s| s == k)
    }
    fn time(&self, k: &str) -> Result<Option<i64>> {
        self.one(k).map(parse_time).transpose()
    }
}

fn cloud(cli: &Cli) -> Result<()> {
    if cli.args.first().is_some_and(|c| c == "connect-project") {
        let o = Opts::parse(
            &cli.args,
            &["--connection", "--project", "--read-key-file", "--base-url"],
            &[],
        )?;
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
    if cli.args.first().is_some_and(|c| {
        matches!(
            c.as_str(),
            "connect-api"
                | "capture-policy"
                | "connect"
                | "connection"
                | "capture-status"
                | "capture-local-status"
                | "capture-check"
                | "capture"
        )
    }) {
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
        "search" => &[
            "--connection",
            "--request-id",
            "--query-file",
            "--from",
            "--through",
            "--agent",
            "--k",
        ],
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
        let request = o.one("--request-id").ok_or(Error::Request)?;
        let text = String::from_utf8(read_private(
            std::path::Path::new(o.one("--query-file").ok_or(Error::Request)?),
            4096,
        )?)
        .map_err(|_| Error::Request)?;
        let selection = Selection {
            from: o.time("--from").map_err(|_| Error::Request)?,
            through: o.time("--through").map_err(|_| Error::Request)?,
            agent_id: o.one("--agent").map(str::to_owned),
            ..Selection::default()
        };
        let k = o
            .one("--k")
            .map(|v| v.parse().map_err(|_| Error::Request))
            .transpose()?
            .unwrap_or(20);
        let page = ProjectClient::open(std::path::Path::new(connection))?
            .search(request, &text, &selection, k)?;
        let mut bytes = serde_json::to_vec(&page).map_err(|_| Error::Response)?;
        bytes.push(b'\n');
        bytes
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
            "--reuse-journal",
            "--connection",
            "--base-url",
            "--source",
            "--journal",
            "--label",
            "--state",
            "--source-root",
            "--adapter",
        ],
        &[
            "--once",
            "--accept-local-capture",
            "--new-journal-per-session",
        ],
    )?;
    let names: std::collections::BTreeSet<_> = o.vals.iter().map(|(k, _)| k).collect();
    if names.len() != o.vals.len() || o.rest.len() != 1 {
        return Err("invalid_capture_arguments".into());
    }
    let command = o.rest[0].as_str();
    let allowed: &[&str] = match command {
        "connect-api" => &[
            "--connection",
            "--api-key-file",
            "--base-url",
            "--label",
            "--reuse-journal",
        ],
        "capture-policy" => &["--connection", "--reuse-journal"],
        "connect" => &[
            "--connection",
            "--base-url",
            "--source",
            "--journal",
            "--label",
        ],
        "connection" | "capture-status" | "capture-check" => &["--connection"],
        "capture-local-status" => &["--connection", "--state"],
        "capture" => &["--connection", "--state", "--source-root", "--adapter"],
        _ => return Err("invalid_capture_command".into()),
    };
    if o.vals.iter().any(|(k, _)| !allowed.contains(&k.as_str()))
        || o.switches.iter().any(|flag| match command {
            "capture" => !matches!(flag.as_str(), "--once" | "--accept-local-capture"),
            "capture-policy" => flag != "--new-journal-per-session",
            _ => true,
        })
    {
        return Err("invalid_capture_arguments".into());
    }
    let val = |name: &str| o.one(name).ok_or("missing_capture_argument");
    let connection = std::path::Path::new(val("--connection")?);
    let value = match command {
        "connect-api" => serde_json::to_value(capture::connect_api_reusing(
            connection,
            o.one("--base-url").unwrap_or("https://oojuro.com"),
            std::path::Path::new(val("--api-key-file")?),
            val("--label")?,
            o.one("--reuse-journal"),
        )?)?,
        "capture-policy" => {
            if o.one("--reuse-journal").is_some() == o.has("--new-journal-per-session") {
                return Err("choose_one_journal_policy".into());
            }
            serde_json::to_value(capture::set_policy(connection, o.one("--reuse-journal"))?)?
        }
        "connect" => serde_json::to_value(capture::connect(
            connection,
            o.one("--base-url").unwrap_or("https://oojuro.com"),
            val("--source")?,
            val("--journal")?,
            val("--label")?,
        )?)?,
        "connection" => serde_json::to_value(capture::resume_approval(connection)?)?,
        "capture-status" => serde_json::to_value(capture::status(connection)?)?,
        "capture-local-status" => serde_json::to_value(capture::local_status(
            connection,
            std::path::Path::new(val("--state")?),
        )?)?,
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
