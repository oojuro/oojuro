//! The recorder's query socket (`docs/FRONT_DOOR.md`, "Reading while the
//! recorder writes").
//!
//! The recorder process is the only one that opens the store. It serves
//! other processes over a Unix socket (mode 600 in the private home
//! directory, and the peer's uid must be the recorder's): reads run on
//! per-connection threads through a [`Reader`] on the open store, in
//! parallel with ingest; writes (`remember`, `forget`) are queued to the
//! recorder's own loop, the store's single writer, which answers between
//! file commits.
use crate::{
    Result,
    journal::Reader,
    proto::{self, Request},
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

/// Longest request line accepted (a `remember` of the largest record,
/// JSON-escaped, fits).
pub const MAX_REQUEST_BYTES: u64 = 128 << 20;
pub const MAX_CONNECTIONS: usize = 64;

/// A write for the store's owner, with where to send the answer.
pub struct WriteJob {
    pub req: Request,
    pub reply: mpsc::Sender<std::result::Result<Value, String>>,
}

pub struct Server {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

/// The uid of the process at the other end of `s`.
pub fn peer_uid(s: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: valid fd, out pointer and length for SO_PEERCRED.
    let r = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (r == 0).then_some(cred.uid)
}

fn own_uid() -> u32 {
    // SAFETY: getuid has no failure mode.
    unsafe { libc::getuid() }
}

/// Bind the socket: refuse when another recorder answers on it, replace a
/// stale one, and make it private (600; its directory 700 when created).
pub fn bind(path: &Path) -> Result<UnixListener> {
    // sockaddr_un holds at most 107 path bytes on Linux (103 on macOS)
    let n = path.as_os_str().len();
    if n > 103 {
        return Err(format!(
            "socket path {} is {n} bytes, longer than Unix sockets allow (103); give a shorter --socket PATH or OOJURO_HOME",
            path.display()
        )
        .into());
    }
    if let Some(dir) = path.parent() {
        crate::journal::private_dir(dir)?;
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(format!("another oojurod is serving {}", path.display()).into());
        }
        std::fs::remove_file(path)?;
    }
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

impl Server {
    /// Serve `listener` until [`Server::stop`]. `hello` is the answer to
    /// `{"op":"hello"}`.
    pub fn start(
        listener: UnixListener,
        path: &Path,
        reader: Reader,
        writes: mpsc::Sender<WriteJob>,
        hello: Value,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let s = Arc::clone(&stop);
        let accept = std::thread::Builder::new()
            .name("oojurod-accept".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    if s.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(conn) = conn else { continue };
                    if peer_uid(&conn) != Some(own_uid()) {
                        continue; // another user: drop the connection
                    }
                    if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                        active.fetch_sub(1, Ordering::SeqCst);
                        let _ = reply(&conn, &Err("recorder busy: too many connections".into()));
                        continue;
                    }
                    let (reader, writes, hello, active) =
                        (reader.clone(), writes.clone(), hello.clone(), Arc::clone(&active));
                    let spawned = std::thread::Builder::new().name("oojurod-query".into()).spawn(move || {
                        handle(conn, &reader, &writes, &hello);
                        active.fetch_sub(1, Ordering::SeqCst);
                    });
                    if spawned.is_err() {
                        // the connection moved into the failed closure and is closed
                        continue;
                    }
                }
            })
            .expect("accept thread");
        Self { path: path.to_path_buf(), stop, accept: Some(accept) }
    }

    /// Stop accepting, wait for the accept thread and remove the socket.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.path); // wake the accept loop
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.accept.is_some() {
            self.shutdown();
        }
    }
}

fn reply(mut s: &UnixStream, r: &std::result::Result<Value, String>) -> std::io::Result<()> {
    let v = match r {
        Ok(v) => json!({ "ok": true, "result": v }),
        Err(e) => json!({ "ok": false, "error": e }),
    };
    let mut line = serde_json::to_vec(&v)?;
    line.push(b'\n');
    s.write_all(&line)?;
    s.flush()
}

fn handle(conn: UnixStream, reader: &Reader, writes: &mpsc::Sender<WriteJob>, hello: &Value) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(60)));
    let mut line = Vec::new();
    let mut br = BufReader::new((&conn).take(MAX_REQUEST_BYTES));
    if br.read_until(b'\n', &mut line).is_err() || line.is_empty() {
        return;
    }
    let req: Request = match serde_json::from_slice(&line) {
        Ok(r) => r,
        Err(e) => {
            let _ = reply(&conn, &Err(format!("bad request: {e}")));
            return;
        }
    };
    let out: std::result::Result<Value, String> = match &req {
        Request::Hello => Ok(hello.clone()),
        Request::Export { filters } => {
            let mut w = std::io::BufWriter::new(&conn);
            let r = reader.export(filters, &mut |rec| {
                let mut l = serde_json::to_vec(&json!({ "ok": true, "item": rec }))?;
                l.push(b'\n');
                w.write_all(&l)?;
                Ok(())
            });
            if w.flush().is_err() {
                return;
            }
            drop(w);
            r.map(|n| json!({ "count": n })).map_err(|e| e.to_string())
        }
        r if r.is_write() => {
            let (tx, rx) = mpsc::channel();
            match writes.send(WriteJob { req: req.clone(), reply: tx }) {
                Ok(()) => rx.recv().unwrap_or_else(|_| Err("the recorder stopped before answering".into())),
                Err(_) => Err("the recorder is shutting down".into()),
            }
        }
        r => proto::read(reader, r).map_err(|e| e.to_string()),
    };
    let _ = reply(&conn, &out);
}
