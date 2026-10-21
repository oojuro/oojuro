//! oojuro-bench: the shared benchmark harness CLI. See docs/BENCH_HARNESS.md.
use oojuro_bench::{
    ArmSpec, Isolation, Result, arms, beir, gates, memqa, report,
    scorecard::{Ctx, Scorecard},
    serve, sys, turnloop,
};
use std::{collections::BTreeMap, path::PathBuf};

const USAGE: &str = "usage: oojuro-bench <command> [flags]
  serve    --arm nine|tantivy|sqlite            JSON-lines arm server on stdin/stdout
  beir     --dataset beir-scifact [--split dev|heldout|test] [--k 100]
  turnloop --set swe-agent [--split dev] [--trajectories 50] [--background 20000] [--k 20]
           [--thinking index|drop] [--config key=value]... [--pool 200 --dump-pools FILE]
  memqa    --set longmemeval [--split dev] [--k 50] [--limit N] [--dump-pools FILE]
  gates                                         byte-exact, as-of, freshness, crash, determinism
  report   SCORECARD.json... [--baseline LABEL] [--out report.md]
common: --arm [label=]nine|tantivy|sqlite|cmd:<argv> (repeat)  --runs 3  --out FILE
        --data DIR (default $OOJURO_DATA or ~/.cache/oojuro-data)  --work DIR  --mem-max 3G  --in-process  --keep  --no-lock (smoke tests only)";

struct Args {
    flags: Vec<(String, String)>,
    switches: Vec<String>,
    positional: Vec<String>,
}

const SWITCHES: &[&str] = &["--in-process", "--keep", "--no-lock", "--help"];

impl Args {
    fn parse(raw: &[String]) -> Result<Self> {
        let mut a = Args { flags: Vec::new(), switches: Vec::new(), positional: Vec::new() };
        let mut i = 0;
        while i < raw.len() {
            let s = &raw[i];
            if SWITCHES.contains(&s.as_str()) {
                a.switches.push(s.clone());
            } else if let Some(name) = s.strip_prefix("--") {
                let value = raw.get(i + 1).ok_or_else(|| format!("--{name} needs a value"))?;
                a.flags.push((name.to_string(), value.clone()));
                i += 1;
            } else {
                a.positional.push(s.clone());
            }
            i += 1;
        }
        Ok(a)
    }
    fn get(&self, name: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
    fn all(&self, name: &str) -> Vec<&str> {
        self.flags.iter().filter(|(n, _)| n == name).map(|(_, v)| v.as_str()).collect()
    }
    fn num(&self, name: &str, default: usize) -> Result<usize> {
        Ok(self.get(name).map(str::parse).transpose()?.unwrap_or(default))
    }
    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn ctx(a: &Args, command: &str) -> Result<Ctx> {
    let work = a.get("work").map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir().join("oojuro-bench").join(format!("{command}-{}-{}", std::process::id(), sys::unique_suffix()))
    });
    std::fs::create_dir_all(&work)?;
    let data = a
        .get("data")
        .map(String::from)
        .or_else(|| std::env::var("OOJURO_DATA").ok())
        .unwrap_or_else(|| format!("{}/.cache/oojuro-data", home()));
    let in_process = a.has("--in-process");
    Ok(Ctx {
        work,
        data: PathBuf::from(data.replacen('~', &home(), 1)),
        runs: a.num("runs", 3)?,
        in_process,
        isolation: (!in_process).then(|| Isolation { memory_max: a.get("mem-max").unwrap_or("3G").to_string() }),
        keep: a.has("--keep"),
        lock: !a.has("--no-lock"),
    })
}

fn arm_specs(a: &Args) -> Result<Vec<ArmSpec>> {
    let given = a.all("arm");
    let list: Vec<&str> = if given.is_empty() { arms::BUILTIN.to_vec() } else { given };
    list.into_iter().map(ArmSpec::parse).collect()
}

fn out(a: &Args, suite: &str, dataset: &str, split: &str) -> PathBuf {
    a.get("out").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from("bench-results").join(format!("{suite}-{dataset}-{split}-{}.json", sys::now_utc().replace(':', "")))
    })
}

fn summarize(card: &Scorecard, path: &std::path::Path) {
    eprintln!("{}", report::render(std::slice::from_ref(card), None));
    eprintln!("scorecard: {}", path.display());
}

fn main() -> Result<()> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = raw.first().cloned() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let a = Args::parse(&raw[1..])?;
    if a.has("--help") || command == "help" || command == "--help" {
        println!("{USAGE}");
        return Ok(());
    }
    match command.as_str() {
        "serve" => serve(arms::builtin(a.get("arm").ok_or("serve needs --arm")?)?),
        "beir" => {
            let ctx = ctx(&a, "beir")?;
            let dataset = a.get("dataset").ok_or("beir needs --dataset, e.g. beir-scifact")?.to_string();
            let split = a.get("split").unwrap_or("dev").to_string();
            let o = beir::Options {
                out: out(&a, "beir", &dataset, &split),
                dataset,
                split,
                k: a.num("k", 100)?,
                batch: a.num("batch", 1000)?,
                arms: arm_specs(&a)?,
                run_file: a.get("run-file").map(PathBuf::from),
                limit_queries: a.get("limit-queries").map(str::parse).transpose()?,
            };
            summarize(&beir::run(&ctx, &o)?, &o.out);
            Ok(())
        }
        "turnloop" => {
            let ctx = ctx(&a, "turnloop")?;
            let set = a.get("set").unwrap_or("swe-agent").to_string();
            let split = a.get("split").unwrap_or("dev").to_string();
            let mut config = BTreeMap::new();
            for kv in a.all("config") {
                let (k, v) = kv.split_once('=').ok_or("--config wants key=value")?;
                config.insert(k.to_string(), v.to_string());
            }
            let o = turnloop::Options {
                out: out(&a, "turnloop", &set, &split),
                set,
                split,
                trajectories: a.num("trajectories", 50)?,
                background: a.num("background", 20_000)?,
                k: a.num("k", 20)?,
                batch: a.num("batch", 1000)?,
                thinking: a.get("thinking").unwrap_or("index").to_string(),
                config,
                arms: arm_specs(&a)?,
                pool: a.num("pool", 20)?,
                dump: a.get("dump-pools").map(PathBuf::from),
            };
            summarize(&turnloop::run(&ctx, &o)?, &o.out);
            Ok(())
        }
        "memqa" => {
            let ctx = ctx(&a, "memqa")?;
            let set = a.get("set").unwrap_or("longmemeval").to_string();
            let split = a.get("split").unwrap_or("dev").to_string();
            let o = memqa::Options {
                out: out(&a, "memqa", &set, &split),
                set,
                split,
                k: a.num("k", 50)?,
                limit: a.get("limit").map(str::parse).transpose()?,
                arms: arm_specs(&a)?,
                dump: a.get("dump-pools").map(PathBuf::from),
            };
            summarize(&memqa::run(&ctx, &o)?, &o.out);
            Ok(())
        }
        "gates" => {
            let ctx = ctx(&a, "gates")?;
            let o = gates::Options { out: out(&a, "gates", "synthetic", "all"), arms: arm_specs(&a)? };
            summarize(&gates::run(&ctx, &o)?, &o.out);
            Ok(())
        }
        "report" => {
            let mut cards = Vec::new();
            for p in &a.positional {
                cards.push(serde_json::from_slice::<Scorecard>(&std::fs::read(p).map_err(|e| format!("{p}: {e}"))?)?);
            }
            if cards.is_empty() {
                return Err("report needs scorecard files".into());
            }
            let text = report::render(&cards, a.get("baseline"));
            match a.get("out") {
                Some(path) => std::fs::write(path, text)?,
                None => print!("{text}"),
            }
            Ok(())
        }
        other => {
            eprintln!("unknown command {other:?}\n{USAGE}");
            std::process::exit(2);
        }
    }
}
