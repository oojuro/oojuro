//! Protected same-user local read socket. One bounded request and response per connection.
use super::api::{self, Failure, Namespace, Owner};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    mem,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Component, Path},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
const DEADLINE: Duration = Duration::from_secs(5);
const PEERS: usize = 8;
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::SeqCst);
}
struct Signals {
    int: libc::sigaction,
    term: libc::sigaction,
}
impl Signals {
    fn install() -> Result<Self, Failure> {
        STOP.store(false, Ordering::SeqCst);
        // SAFETY: initialized sigaction pointers and an async-signal-safe atomic-only handler.
        unsafe {
            let mut action: libc::sigaction = mem::zeroed();
            action.sa_sigaction = stop as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            let mut int = mem::zeroed();
            let mut term = mem::zeroed();
            if libc::sigaction(libc::SIGINT, &action, &mut int) != 0 {
                return Err("signal_unavailable");
            }
            if libc::sigaction(libc::SIGTERM, &action, &mut term) != 0 {
                libc::sigaction(libc::SIGINT, &int, std::ptr::null_mut());
                return Err("signal_unavailable");
            }
            Ok(Self { int, term })
        }
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        // SAFETY: restore the exact process handlers saved by install.
        unsafe {
            libc::sigaction(libc::SIGINT, &self.int, std::ptr::null_mut());
            libc::sigaction(libc::SIGTERM, &self.term, std::ptr::null_mut());
        }
    }
}
fn uid() -> u32 {
    unsafe { libc::geteuid() }
}
fn peer(stream: &UnixStream) -> bool {
    let mut cred: libc::ucred = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: valid socket and writable correctly sized credentials buffer.
    unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        ) == 0
            && len as usize == mem::size_of::<libc::ucred>()
            && cred.uid == uid()
    }
}
fn path_check(path: &Path) -> Result<(), Failure> {
    if !path.is_absolute()
        || path.as_os_str().as_bytes().len() > 103
        || path.as_os_str().as_bytes().contains(&0)
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("invalid_socket_path");
    }
    let parent = path.parent().ok_or("invalid_socket_path")?;
    super::config::check_private_path(parent).map_err(|_| "private_socket_parent_required")?;
    Ok(())
}
fn wait(stream: &UnixStream, event: i16, deadline: Instant) -> Result<(), Failure> {
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .ok_or("socket_deadline")?;
        let mut fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: event,
            revents: 0,
        };
        let ms = left.as_millis().max(1).min(i32::MAX as u128) as i32;
        // SAFETY: one live socket poll descriptor for the remaining absolute deadline.
        let result = unsafe { libc::poll(&mut fd, 1, ms) };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            return Err("socket_deadline");
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err("socket_unavailable");
        }
    }
}
fn read_line(stream: &mut UnixStream, limit: usize, deadline: Instant) -> Result<Vec<u8>, Failure> {
    let mut out = vec![];
    let mut buf = [0u8; 4096];
    loop {
        wait(stream, libc::POLLIN, deadline)?;
        match stream.read(&mut buf) {
            Ok(0) => return Err("incomplete_request"),
            Ok(n) => {
                let end = buf[..n].iter().position(|b| *b == b'\n');
                let take = end.unwrap_or(n);
                if out.len().checked_add(take).is_none_or(|v| v > limit) {
                    return Err("frame_bound");
                }
                out.extend_from_slice(&buf[..take]);
                if let Some(i) = end {
                    if i + 1 != n {
                        return Err("multiple_frames_refused");
                    }
                    return Ok(out);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err("socket_unavailable"),
        }
    }
}
fn write_line(stream: &mut UnixStream, bytes: &[u8], deadline: Instant) -> Result<(), Failure> {
    let mut cursor = 0;
    while cursor < bytes.len() {
        wait(stream, libc::POLLOUT, deadline)?;
        match stream.write(&bytes[cursor..]) {
            Ok(0) => return Err("socket_unavailable"),
            Ok(n) => cursor += n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err("socket_unavailable"),
        }
    }
    Ok(())
}
fn reply(
    stream: &mut UnixStream,
    namespace: Namespace,
    result: Result<Value, Failure>,
    deadline: Instant,
) -> Result<(), Failure> {
    let value = match result {
        Ok(result) => json!({"version":1,"namespace":namespace.name(),"ok":true,"result":result}),
        Err(error) => json!({"version":1,"namespace":namespace.name(),"ok":false,"error":error}),
    };
    let mut bytes = api::encode(&value, api::OUTPUT_BYTES - 1).or_else(|_| {
        api::encode(
            &json!({"version":1,"namespace":namespace.name(),"ok":false,"error":"output_bound"}),
            4096,
        )
    })?;
    bytes.push(b'\n');
    write_line(stream, &bytes, deadline)
}
struct Count(Arc<AtomicUsize>);
impl Drop for Count {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Bound {
    path: std::path::PathBuf,
    dev: u64,
    ino: u64,
}
impl Drop for Bound {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.file_type().is_socket() && m.dev() == self.dev && m.ino() == self.ino)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
/// Owns an already authenticated open authority. Requests never append/capture/prune.
pub(super) fn serve(path: &Path, owner: Owner) -> Result<(), Failure> {
    path_check(path)?;
    if fs::symlink_metadata(path).is_ok() {
        return Err("socket_already_exists");
    }
    let listener = UnixListener::bind(path).map_err(|_| "socket_bind_refused")?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "socket_bind_refused")?;
    let _bound = Bound {
        path: path.to_path_buf(),
        dev: metadata.dev(),
        ino: metadata.ino(),
    };
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| "private_socket_required")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "socket_unavailable")?;
    let _signals = Signals::install()?;
    let namespace = owner.namespace();
    let owner = Arc::new(Mutex::new(owner));
    let active = Arc::new(AtomicUsize::new(0));
    let mut workers = vec![];
    while !STOP.load(Ordering::SeqCst) {
        workers.retain(|h: &thread::JoinHandle<()>| !h.is_finished());
        match listener.accept() {
            Ok((mut stream, _)) => {
                if !peer(&stream) || stream.set_nonblocking(true).is_err() {
                    continue;
                }
                let deadline = Instant::now() + DEADLINE;
                if active.fetch_add(1, Ordering::SeqCst) >= PEERS {
                    active.fetch_sub(1, Ordering::SeqCst);
                    let _ = reply(&mut stream, namespace, Err("reader_busy"), deadline);
                    continue;
                }
                let count = Count(active.clone());
                let owner = owner.clone();
                match thread::Builder::new()
                    .name("native-read".into())
                    .spawn(move || {
                        let _count = count;
                        let result = (|| {
                            let bytes = read_line(&mut stream, api::REQUEST_BYTES - 1, deadline)?;
                            let value: Value =
                                serde_json::from_slice(&bytes).map_err(|_| "invalid_request")?;
                            let owner = owner.try_lock().map_err(|_| "reader_busy")?;
                            owner.execute(value)
                        })();
                        let _ = reply(&mut stream, namespace, result, deadline);
                    }) {
                    Ok(worker) => workers.push(worker),
                    Err(_) => return Err("reader_unavailable"),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err("socket_unavailable"),
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}
fn connect(path: &Path, deadline: Instant) -> Result<UnixStream, Failure> {
    path_check(path)?;
    let m = fs::symlink_metadata(path).map_err(|_| "reader_unavailable")?;
    if !m.file_type().is_socket() || m.uid() != uid() || m.mode() & 0o777 != 0o600 {
        return Err("private_socket_required");
    }
    // SAFETY: create a private owned descriptor, initialize sockaddr_un, and retain ownership in UnixStream.
    let stream = unsafe {
        let fd = libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        );
        if fd < 0 {
            return Err("reader_unavailable");
        }
        let stream = UnixStream::from_raw_fd(fd);
        let mut address: libc::sockaddr_un = mem::zeroed();
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (dst, src) in address.sun_path.iter_mut().zip(path.as_os_str().as_bytes()) {
            *dst = *src as libc::c_char;
        }
        let result = libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            mem::size_of_val(&address) as libc::socklen_t,
        );
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err("reader_unavailable");
            }
            wait(&stream, libc::POLLOUT, deadline)?;
            if stream
                .take_error()
                .map_err(|_| "reader_unavailable")?
                .is_some()
            {
                return Err("reader_unavailable");
            }
        }
        stream
    };
    if !peer(&stream) {
        return Err("reader_peer_refused");
    }
    Ok(stream)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    version: u32,
    namespace: Namespace,
    ok: bool,
    result: Option<Value>,
    error: Option<String>,
}
/// One explicit bounded request. No reconnection, inference or legacy/direct-open fallback.
pub(super) fn call(path: &Path, namespace: Namespace, envelope: Value) -> Result<Value, Failure> {
    if envelope.get("version").and_then(Value::as_u64) != Some(1)
        || envelope.get("namespace").and_then(Value::as_str) != Some(namespace.name())
    {
        return Err("namespace_or_version_refused");
    }
    let mut bytes = api::encode(&envelope, api::REQUEST_BYTES - 1)?;
    bytes.push(b'\n');
    let deadline = Instant::now() + DEADLINE;
    let mut stream = connect(path, deadline)?;
    write_line(&mut stream, &bytes, deadline)?;
    let reply = read_line(&mut stream, api::OUTPUT_BYTES - 1, deadline)?;
    let response: Response = serde_json::from_slice(&reply).map_err(|_| "invalid_response")?;
    if response.version != 1 || response.namespace != namespace {
        return Err("response_namespace_refused");
    }
    match (response.ok, response.result, response.error) {
        (true, Some(result), None) => Ok(result),
        (false, None, Some(_)) => Err("read_refused"),
        _ => Err("invalid_response"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ancestor_policy_refuses_foreign_owner_even_without_public_write_permission() {
        assert!(!super::super::config::trusted_ancestor(1001, 0o755, 1000));
        assert!(!super::super::config::trusted_ancestor(1001, 0o700, 1000));
        assert!(!super::super::config::trusted_ancestor(1001, 0o1777, 1000));
        assert!(super::super::config::trusted_ancestor(1000, 0o700, 1000));
        assert!(super::super::config::trusted_ancestor(1000, 0o755, 1000));
        assert!(super::super::config::trusted_ancestor(0, 0o755, 1000));
        assert!(super::super::config::trusted_ancestor(0, 0o1777, 1000));
        assert!(!super::super::config::trusted_ancestor(0, 0o777, 1000));
        assert!(!super::super::config::trusted_ancestor(1000, 0o777, 1000));
    }
}
