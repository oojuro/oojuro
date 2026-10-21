//! [`Oojuro`]: the library entry point for other processes. It talks to the
//! recorder over its socket when one serves the journal, else opens the
//! journal itself for the one call (and closes it, so a recorder can start).
use crate::{
    Result,
    journal::{self, Journal},
    paths::Paths,
    proto::{self, Request},
    types::{Filters, ForgetReport, Record, Ref, Remembered, SearchArgs, Stats, Target},
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Via {
    /// The recorder answers on this socket.
    Recorder { socket: PathBuf, pid: u64 },
    /// No recorder serves this journal: open it for each call.
    Direct,
}

#[derive(Clone, Debug)]
pub struct Oojuro {
    pub paths: Paths,
    pub via: Via,
}

/// Longest wait for one answer line from the recorder (a bulk forget with
/// its proof is the slowest call).
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(900);

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
}

fn send(socket: &Path, req: &Request, timeout: Option<Duration>) -> Result<BufReader<UnixStream>> {
    let s = UnixStream::connect(socket)?;
    s.set_read_timeout(timeout)?;
    let mut line = serde_json::to_vec(req)?;
    line.push(b'\n');
    (&s).write_all(&line)?;
    Ok(BufReader::new(s))
}

fn read_reply(r: &mut BufReader<UnixStream>) -> Result<Value> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Err("the recorder closed the connection without answering".into());
    }
    let v: Value = serde_json::from_str(&line)?;
    if v["ok"].as_bool() == Some(true) {
        Ok(v)
    } else {
        Err(v["error"].as_str().unwrap_or("recorder error").to_string().into())
    }
}

/// One request to the recorder on `socket`; the answer's `result`.
pub fn call(socket: &Path, req: &Request, timeout: Duration) -> Result<Value> {
    let mut r = send(socket, req, Some(timeout))?;
    Ok(read_reply(&mut r)?["result"].take())
}

/// Ask the recorder on `socket` who it is; `None` when nobody answers.
pub fn hello(socket: &Path) -> Option<Value> {
    hello_within(socket, Duration::from_secs(5))
}

/// [`hello`], waiting at most `timeout` for the answer.
pub fn hello_within(socket: &Path, timeout: Duration) -> Option<Value> {
    let mut r = send(socket, &Request::Hello, Some(timeout)).ok()?;
    read_reply(&mut r).ok().map(|v| v["result"].clone())
}

impl Oojuro {
    /// Use the recorder when one answers on `paths.socket` for this very
    /// store, else direct access.
    pub fn open(paths: Paths) -> Self {
        Self::open_within(paths, Duration::from_secs(5)).0
    }

    /// [`Self::open`], waiting at most `timeout` for the recorder's hello;
    /// also returns the hello of the recorder that serves this store.
    pub fn open_within(paths: Paths, timeout: Duration) -> (Self, Option<Value>) {
        match hello_within(&paths.socket, timeout) {
            Some(h)
                if h["version"].as_u64() == Some(u64::from(proto::VERSION))
                    && h["store"].as_str().map(Path::new).map(canonical) == Some(canonical(&paths.store)) =>
            {
                let via = Via::Recorder { socket: paths.socket.clone(), pid: h["pid"].as_u64().unwrap_or(0) };
                (Self { paths, via }, Some(h))
            }
            _ => (Self { paths, via: Via::Direct }, None),
        }
    }

    /// Always open the journal directly (no recorder).
    pub fn direct(paths: Paths) -> Self {
        Self { paths, via: Via::Direct }
    }

    pub fn served_by(&self) -> String {
        match &self.via {
            Via::Recorder { pid, .. } => format!("oojurod (pid {pid})"),
            Via::Direct => "direct".into(),
        }
    }

    fn open_journal(&self, create: bool) -> Result<Journal> {
        let r = if create { Journal::open(&self.paths.store) } else { Journal::open_existing(&self.paths.store) };
        r.map_err(|e| {
            let m = e.to_string();
            let l = m.to_ascii_lowercase();
            if l.contains("another process")
                || l.contains("would block")
                || l.contains("wouldblock")
                || l.contains("locked")
            {
                format!(
                    "the journal at {} is held by another process (a recorder, or another oojuro call); \
                     calls go through a recorder only when it answers on {} (an older oojurod build does not: \
                     restart it from this build), or read a copy with --store",
                    self.paths.store.display(),
                    self.paths.socket.display()
                )
                .into()
            } else {
                e
            }
        })
    }

    /// One request; the answer's `result`.
    pub fn request(&self, req: &Request) -> Result<Value> {
        self.request_within(req, REPLY_TIMEOUT)
    }

    /// [`Self::request`], waiting at most `timeout` for the recorder's answer.
    pub fn request_within(&self, req: &Request, timeout: Duration) -> Result<Value> {
        match &self.via {
            Via::Recorder { socket, .. } => {
                let mut r = send(socket, req, Some(timeout))?;
                Ok(read_reply(&mut r)?["result"].take())
            }
            Via::Direct => {
                let append = matches!(req, Request::Append { .. });
                let mut j = self.open_journal(append || matches!(req, Request::Remember { .. }))?;
                if append {
                    // captured records follow the recording settings, as in the recorder
                    j.capture().set_policy(Box::new(crate::policy::RecordingPolicy::new(&self.paths.config)));
                }
                let v = proto::execute(&mut j, req)?;
                j.close()?;
                Ok(v)
            }
        }
    }

    fn typed<T: DeserializeOwned>(&self, req: &Request) -> Result<T> {
        Ok(serde_json::from_value(self.request(req)?)?)
    }

    pub fn remember(&self, writer: &str, text: &str, key: Option<&str>, at_us: Option<i64>) -> Result<Remembered> {
        self.typed(&Request::Remember { writer: writer.into(), text: text.into(), key: key.map(Into::into), at_us })
    }

    pub fn search(&self, args: &SearchArgs) -> Result<Vec<Ref>> {
        self.typed(&Request::Search(args.clone()))
    }

    pub fn get_id(&self, id: u64) -> Result<Option<Record>> {
        self.typed(&Request::GetId { id })
    }

    pub fn get_key(&self, writer: &str, key: &str, as_of_us: Option<i64>) -> Result<Option<Record>> {
        self.typed(&Request::GetKey { writer: writer.into(), key: key.into(), as_of_us })
    }

    pub fn history(&self, writer: &str, key: &str) -> Result<Vec<Record>> {
        self.typed(&Request::History { writer: writer.into(), key: key.into() })
    }

    pub fn range(&self, filters: &Filters, limit: usize) -> Result<Vec<Record>> {
        self.typed(&Request::Range { filters: filters.clone(), limit })
    }

    pub fn forget(&self, target: &Target, proof: bool) -> Result<ForgetReport> {
        self.typed(&Request::Forget { target: target.clone(), proof })
    }

    pub fn count(&self, filters: &Filters) -> Result<u64> {
        self.typed(&Request::Count { filters: filters.clone() })
    }

    pub fn stats(&self) -> Result<Stats> {
        let mut s: Stats = self.typed(&Request::Stats)?;
        s.served_by = self.served_by();
        Ok(s)
    }

    /// Every record matching the filters, oldest first; returns the count.
    pub fn export(&self, filters: &Filters, sink: &mut dyn FnMut(Record) -> Result<()>) -> Result<u64> {
        match &self.via {
            Via::Recorder { socket, .. } => {
                let mut r = send(socket, &Request::Export { filters: filters.clone() }, Some(REPLY_TIMEOUT))?;
                loop {
                    let mut v = read_reply(&mut r)?;
                    if v.get("item").is_some() {
                        sink(serde_json::from_value(v["item"].take())?)?;
                    } else {
                        return Ok(v["result"]["count"].as_u64().unwrap_or(0));
                    }
                }
            }
            Via::Direct => {
                let j = self.open_journal(false)?;
                let n = j.reader().export(filters, sink)?;
                j.close()?;
                Ok(n)
            }
        }
    }
}

impl Oojuro {
    /// Search with the defaults most callers want.
    pub fn search_simple(&self, query: &str, k: usize, max_query_bytes: usize) -> Result<Vec<Ref>> {
        self.search(&SearchArgs {
            query: query.into(),
            k,
            filters: Filters::default(),
            excerpt_bytes: crate::DEFAULT_EXCERPT_BYTES,
            max_query_bytes,
        })
    }
}

pub use journal::exists as journal_exists;
