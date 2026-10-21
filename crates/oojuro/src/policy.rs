//! The recorder's settings policy: `paused`, `record`, `record_thinking`,
//! `index_thinking` and `exclude`, resolved per record from the global
//! file, the project file found from the agent's working directory, and
//! the writer's `[agents."..."]` sections. Files are re-read when they
//! change (checked once per pass); new project files are noticed within
//! a few seconds.
use crate::{
    config::{Config, File},
    exclude::{Exclude, tool_call_paths},
    paths::find_project_config,
};
use oojuro_capture::{
    Rec,
    ingest::{Decision, Policy},
    kind,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

/// How long a directory's "nearest project file" answer is trusted.
const PROJECT_TTL: Duration = Duration::from_secs(5);

/// A settings file as last read: parsed (or absent), or why it does not parse.
type Parsed = Result<Option<File>, String>;

struct Decider {
    paused: bool,
    record: bool,
    record_thinking: bool,
    index_thinking: bool,
    exclude: Exclude,
}

pub struct RecordingPolicy {
    global: PathBuf,
    home: Option<PathBuf>,
    /// The global file as last read (`Err`: it does not parse; nothing is
    /// recorded until it does).
    global_file: Result<Option<File>, String>,
    global_mtime: Option<SystemTime>,
    /// cwd -> nearest project file, and when that was looked up.
    nearest: HashMap<String, (Option<PathBuf>, Instant)>,
    /// project file -> (mtime, parsed or the parse error)
    projects: HashMap<PathBuf, (Option<SystemTime>, Parsed)>,
    decided: HashMap<(String, Option<PathBuf>), Arc<Decider>>,
    /// Problems already reported (each once).
    reported: std::collections::HashSet<String>,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl RecordingPolicy {
    pub fn new(global: &Path) -> Self {
        let mut p = Self {
            global: global.to_path_buf(),
            home: crate::paths::user_home().ok(),
            global_file: Ok(None),
            global_mtime: None,
            nearest: HashMap::new(),
            projects: HashMap::new(),
            decided: HashMap::new(),
            reported: Default::default(),
        };
        p.reload_global();
        p
    }

    fn report(&mut self, msg: String) {
        if self.reported.insert(msg.clone()) {
            eprintln!("oojurod: settings: {msg}");
        }
    }

    fn reload_global(&mut self) {
        self.global_mtime = mtime(&self.global);
        self.global_file = File::load(&self.global, false).map_err(|e| e.to_string());
        if let Err(e) = &self.global_file {
            let e = e.clone();
            self.report(format!("{e} (nothing is recorded until it parses)"));
        }
        self.decided.clear();
    }

    fn project_for(&mut self, cwd: &str) -> Option<PathBuf> {
        if cwd.is_empty() || !cwd.starts_with('/') {
            return None;
        }
        if let Some((p, at)) = self.nearest.get(cwd)
            && at.elapsed() < PROJECT_TTL
        {
            return p.clone();
        }
        let p = find_project_config(Path::new(cwd));
        if let Some(pp) = &p
            && !self.projects.contains_key(pp)
        {
            let parsed = File::load(pp, true).map_err(|e| e.to_string());
            self.projects.insert(pp.clone(), (mtime(pp), parsed));
        }
        let changed = self.nearest.get(cwd).is_some_and(|(old, _)| *old != p);
        self.nearest.insert(cwd.to_string(), (p.clone(), Instant::now()));
        if changed {
            self.decided.clear();
        }
        p
    }

    fn decider(&mut self, writer: &str, project: Option<PathBuf>) -> Arc<Decider> {
        let key = (writer.to_string(), project.clone());
        if let Some(d) = self.decided.get(&key) {
            return Arc::clone(d);
        }
        let closed = || Decider {
            paused: true,
            record: false,
            record_thinking: false,
            index_thinking: false,
            exclude: Exclude::default(),
        };
        let project_file = match project.as_ref().and_then(|p| self.projects.get(p)) {
            Some((_, Ok(f))) => f.clone(),
            Some((_, Err(e))) => {
                let e = e.clone();
                self.report(format!("{e} (nothing is recorded under that project until it parses)"));
                let d = Arc::new(closed());
                self.decided.insert(key, Arc::clone(&d));
                return d;
            }
            None => None,
        };
        let global_file = match &self.global_file {
            Ok(f) => f.clone(),
            Err(_) => {
                let d = Arc::new(closed());
                self.decided.insert(key, Arc::clone(&d));
                return d;
            }
        };
        let cfg = Config { global: global_file, project: project_file };
        let d = match cfg.resolve(Some(writer)) {
            Ok(r) => {
                for w in &r.warnings {
                    self.report(w.clone());
                }
                match Exclude::new(&r.settings.exclude, self.home.as_deref()) {
                    Ok(exclude) => Decider {
                        paused: r.paused,
                        record: r.settings.record,
                        record_thinking: r.settings.record_thinking,
                        index_thinking: r.settings.index_thinking,
                        exclude,
                    },
                    Err(e) => {
                        self.report(format!(
                            "bad exclude pattern: {e} (nothing is recorded for {writer} until it is fixed)"
                        ));
                        // fail closed: an exclusion that cannot be applied stops recording
                        closed()
                    }
                }
            }
            Err(e) => {
                self.report(format!("{e} (nothing is recorded for {writer} until it is fixed)"));
                closed()
            }
        };
        let d = Arc::new(d);
        self.decided.insert(key, Arc::clone(&d));
        d
    }
}

impl Policy for RecordingPolicy {
    fn refresh(&mut self) {
        if mtime(&self.global) != self.global_mtime {
            self.reload_global();
        }
        let mut changed = false;
        for (p, (m, f)) in self.projects.iter_mut() {
            let now = mtime(p);
            if now != *m {
                *m = now;
                *f = File::load(p, true).map_err(|e| e.to_string());
                changed = true;
            }
        }
        if changed {
            self.decided.clear();
        }
    }

    fn decide(&mut self, rec: &Rec, source: &str) -> Decision {
        let project = self.project_for(&rec.cwd);
        let d = self.decider(&rec.writer, project);
        if d.paused {
            return Decision::Withhold("paused");
        }
        if !d.record {
            return Decision::Withhold("record_off");
        }
        if rec.kind == kind::THINKING && !d.record_thinking {
            return Decision::Withhold("thinking_off");
        }
        if !d.exclude.is_empty() {
            let cwd = (!rec.cwd.is_empty()).then_some(rec.cwd.as_str());
            if d.exclude.matches(source, None)
                || (rec.kind == kind::TOOL_CALL && tool_call_paths(&rec.text).iter().any(|p| d.exclude.matches(p, cwd)))
            {
                return Decision::Withhold("excluded");
            }
        }
        let indexed =
            kind::indexed(rec.kind) && !(rec.kind == kind::THINKING && !d.index_thinking) && !rec.from_oojuro();
        Decision::Store { indexed }
    }
}
