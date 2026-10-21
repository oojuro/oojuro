//! Settings precedence: defaults < global < project < global agent sections
//! (`*` before exact) < project agent sections; `exclude` is the union;
//! `paused` is global only; edits keep comments.
mod common;
use common::Scratch;
use oojuro::config::{self, Config, Settings};
use std::fs;

#[test]
fn precedence_union_and_sources() {
    let s = Scratch::new("cfg");
    let global = s.path("home/config.toml");
    fs::create_dir_all(s.path("home")).unwrap();
    fs::write(
        &global,
        r#"
paused = false
top_k = 30
budget_tokens = 1000
min_bits = 6
exclude = ["*.pem"]
mystery = 1

[agents."codex:*"]
record = false
top_k = 5

[agents."codex:-w-repo"]
top_k = 7
recency = 0.5
"#,
    )
    .unwrap();
    let repo = s.path("w/repo");
    fs::create_dir_all(repo.join("src/deep")).unwrap();
    fs::write(
        repo.join(".oojuro.toml"),
        r#"
top_k = 40
inject = true
exclude = ["secrets/**", ".env"]
paused = true

[agents."codex:-w-repo"]
record = true
max_query_bytes = 262144
"#,
    )
    .unwrap();

    // no files at all: defaults
    let none = Config::load(&s.path("nope.toml"), None).unwrap().resolve(Some("x")).unwrap();
    assert_eq!(none.settings, Settings::default());
    assert!(none.sources.values().all(|v| v == "default") && !none.paused);

    // discovered from a directory deep inside the project
    let c = Config::discover(&global, &repo.join("src/deep")).unwrap();
    assert_eq!(c.project.as_ref().unwrap().path, repo.join(".oojuro.toml"));

    // a person: global, then project top level
    let r = c.resolve(Some("human:ann")).unwrap();
    assert_eq!(r.settings.top_k, 40, "project overrides global");
    assert_eq!(r.settings.budget_tokens, 1000, "global overrides default");
    assert_eq!(r.settings.min_bits, 6.0);
    assert!(r.settings.inject && r.settings.record);
    assert_eq!(r.sources["top_k"], repo.join(".oojuro.toml").display().to_string());
    assert_eq!(r.sources["budget_tokens"], global.display().to_string());
    assert_eq!(r.sources["recency"], "default");
    // exclude: union; a project pattern with a slash is anchored at the project
    let rel = repo.join("secrets/**").display().to_string();
    assert_eq!(r.settings.exclude, vec!["*.pem".to_string(), rel, ".env".to_string()]);
    // paused is global only
    assert!(!r.paused);
    assert!(r.warnings.iter().any(|w| w.contains("`paused` is global only")));
    assert!(r.warnings.iter().any(|w| w.contains("unknown setting `mystery`")));

    // an agent: wildcard section, then exact, then the project's section
    let r = c.resolve(Some("codex:-w-repo")).unwrap();
    assert_eq!(r.settings.top_k, 7, "global exact section beats project top level and the * section");
    assert_eq!(r.settings.recency, 0.5);
    assert!(r.settings.record, "project agent section beats the global * section");
    assert_eq!(r.settings.max_query_bytes, 262_144);
    assert!(r.sources["record"].ends_with("[agents.\"codex:-w-repo\"]"));
    let r = c.resolve(Some("codex:-elsewhere")).unwrap();
    assert!(!r.settings.record && r.settings.top_k == 5);

    // paused from the global file
    fs::write(&global, "paused = true\n").unwrap();
    assert!(Config::discover(&global, &repo).unwrap().resolve(None).unwrap().paused);

    // wrong types are errors naming the file
    fs::write(&global, "top_k = \"many\"\n").unwrap();
    let e = Config::discover(&global, &repo).unwrap().resolve(None).unwrap_err().to_string();
    assert!(e.contains("config.toml") && e.contains("top_k"), "{e}");
    fs::write(&global, "exclude = [\"\"]\n").unwrap();
    assert!(Config::load(&global, None).unwrap().resolve(None).is_err());
    fs::write(&global, "top_k = \n").unwrap();
    assert!(Config::load(&global, None).is_err(), "a file that does not parse");
}

#[test]
fn set_value_keeps_comments_and_validates() {
    let s = Scratch::new("cfgset");
    let p = s.path("h/config.toml");
    // creates the file (and a private directory)
    config::set_value(&p, None, "inject", &toml::Value::Boolean(false)).unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "inject = false\n");
    fs::write(&p, "# recorder settings\nrecord = true  # always\n\n[agents.\"codex:x\"]\n# quiet\ntop_k = 3\n")
        .unwrap();
    config::set_value(&p, None, "paused", &toml::Value::Boolean(true)).unwrap();
    config::set_value(&p, Some("codex:x"), "top_k", &toml::Value::Integer(9)).unwrap();
    config::set_value(&p, Some("human:me"), "record", &toml::Value::Boolean(false)).unwrap();
    config::set_value(&p, None, "exclude", &config::parse_cli_value("exclude", "*.env, ~/private/**").unwrap())
        .unwrap();
    let text = fs::read_to_string(&p).unwrap();
    assert!(text.starts_with("# recorder settings\nrecord = true  # always\npaused = true\nexclude = "), "{text}");
    assert!(text.contains("# quiet\ntop_k = 9\n"), "{text}");
    let c = Config::load(&p, None).unwrap();
    let r = c.resolve(Some("codex:x")).unwrap();
    assert!(r.paused && r.settings.top_k == 9);
    assert_eq!(r.settings.exclude, vec!["*.env", "~/private/**"]);
    assert!(!c.resolve(Some("human:me")).unwrap().settings.record);
    // invalid values never reach the file
    assert!(config::set_value(&p, None, "top_k", &toml::Value::Integer(0)).is_err());
    assert!(config::set_value(&p, None, "nope", &toml::Value::Integer(1)).is_err());
    assert!(config::set_value(&p, Some("codex:x"), "paused", &toml::Value::Boolean(true)).is_err());
    // a multi-line value is rewritten safely (values kept)
    fs::write(&p, "exclude = [\n  \"a\",\n  \"b\",\n]\ntop_k = 4\n").unwrap();
    config::set_value(&p, None, "exclude", &toml::Value::Array(vec![toml::Value::String("c".into())])).unwrap();
    let r = Config::load(&p, None).unwrap().resolve(None).unwrap();
    assert_eq!((r.settings.exclude.clone(), r.settings.top_k), (vec!["c".to_string()], 4));
    // a broken file is not overwritten
    fs::write(&p, "top_k = = 4\n").unwrap();
    assert!(config::set_value(&p, None, "top_k", &toml::Value::Integer(5)).is_err());
    assert_eq!(fs::read_to_string(&p).unwrap(), "top_k = = 4\n");
}
