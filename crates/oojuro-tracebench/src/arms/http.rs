//! Minimal HTTP/1.1 JSON client for the server arms that speak REST
//! (`meili`, `qdrant`, `quickwit`, `openobserve`): a keep-alive connection
//! pool over `std::net`, one
//! connection per concurrent caller, no async runtime (the same approach as
//! the ClickHouse arm's client). Bodies may be `Content-Length` or chunked.
use crate::Result;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpStream,
    sync::Mutex,
    time::{Duration, Instant},
};

pub(crate) struct Http {
    addr: String,
    /// Extra request headers (e.g. `Authorization`), each ending in CRLF.
    headers: String,
    idle: Mutex<Vec<(BufReader<TcpStream>, Instant)>>,
}

enum Mode {
    Length(u64),
    Chunked { left: u64, done: bool, first: bool },
    Eof,
}

/// A response body (de-chunked).
pub(crate) struct Body<R: BufRead> {
    r: R,
    mode: Mode,
}

fn read_line(r: &mut impl BufRead) -> io::Result<String> {
    let mut s = String::new();
    if r.read_line(&mut s)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"));
    }
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

impl<R: BufRead> Read for Body<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match &mut self.mode {
            Mode::Length(left) => {
                if *left == 0 {
                    return Ok(0);
                }
                let want = buf.len().min(*left as usize);
                let n = self.r.read(&mut buf[..want])?;
                if n == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short body"));
                }
                *left -= n as u64;
                Ok(n)
            }
            Mode::Eof => self.r.read(buf),
            Mode::Chunked { left, done, first } => loop {
                if *done {
                    return Ok(0);
                }
                if *left > 0 {
                    let want = buf.len().min(*left as usize);
                    let n = self.r.read(&mut buf[..want])?;
                    if n == 0 {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short chunk"));
                    }
                    *left -= n as u64;
                    return Ok(n);
                }
                if !*first {
                    let l = read_line(&mut self.r)?;
                    if !l.is_empty() {
                        return Err(io::Error::other(format!("bad chunk terminator {l:?}")));
                    }
                }
                *first = false;
                let l = read_line(&mut self.r)?;
                let size = l.split(';').next().unwrap_or("").trim();
                let n = u64::from_str_radix(size, 16).map_err(|_| io::Error::other(format!("bad chunk size {l:?}")))?;
                if n == 0 {
                    while !read_line(&mut self.r)?.is_empty() {}
                    *done = true;
                    return Ok(0);
                }
                *left = n;
            },
        }
    }
}

/// Status line and headers; returns (code, body mode, connection: close).
fn read_head(c: &mut impl BufRead) -> Result<(u16, Mode, bool)> {
    let status = read_line(c)?;
    let code: u16 = status.split_whitespace().nth(1).and_then(|x| x.parse().ok()).ok_or("bad status line")?;
    let (mut len, mut chunked, mut close) = (None, false, false);
    loop {
        let l = read_line(c)?;
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':').unwrap_or((&l, ""));
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "content-length" => len = v.parse::<u64>().ok(),
            "transfer-encoding" => chunked = v.eq_ignore_ascii_case("chunked"),
            "connection" => close = v.eq_ignore_ascii_case("close"),
            _ => {}
        }
    }
    let mode = if chunked {
        Mode::Chunked { left: 0, done: false, first: true }
    } else if let Some(n) = len {
        Mode::Length(n)
    } else {
        close = true;
        Mode::Eof
    };
    Ok((code, mode, close))
}

/// Percent-encoding for a query-string value.
pub(crate) fn url_encode(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// Standard base64 (for a Basic auth header).
pub(crate) fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                o.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                o.push('=');
            }
        }
    }
    o
}

impl Http {
    pub(crate) fn new(addr: String, headers: String) -> Self {
        Self { addr, headers, idle: Mutex::new(Vec::new()) }
    }

    fn connect(&self) -> io::Result<BufReader<TcpStream>> {
        let s = TcpStream::connect(&self.addr)?;
        s.set_nodelay(true)?;
        Ok(BufReader::with_capacity(1 << 16, s))
    }

    fn conn(&self) -> io::Result<BufReader<TcpStream>> {
        loop {
            let c = self.idle.lock().unwrap().pop();
            match c {
                // servers close idle keep-alive connections after a while
                Some((c, t)) if t.elapsed() < Duration::from_secs(4) => return Ok(c),
                Some(_) => continue,
                None => return self.connect(),
            }
        }
    }

    fn head(&self, method: &str, path: &str, ctype: &str, len: usize) -> String {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\n\
             Connection: keep-alive\r\n{}\r\n",
            self.headers
        )
    }

    /// Send a request and return (status code, body bytes). A pooled
    /// connection the server closed while idle is retried once on a fresh
    /// one (nothing reached the server).
    pub(crate) fn call(&self, method: &str, path: &str, ctype: &str, body: &[u8]) -> Result<(u16, Vec<u8>)> {
        let head = self.head(method, path, ctype, body.len());
        let mut c = self.conn()?;
        let send = |c: &mut BufReader<TcpStream>| -> io::Result<()> {
            let s = c.get_mut();
            s.write_all(head.as_bytes())?;
            s.write_all(body)?;
            s.flush()
        };
        let mut fresh = false;
        if send(&mut c).is_err() {
            c = self.connect()?;
            fresh = true;
            send(&mut c)?;
        }
        let parsed = read_head(&mut c);
        let (code, mode, close) = match parsed {
            Ok(x) => x,
            Err(_) if !fresh => {
                // stale pooled connection closed under us before answering
                c = self.connect()?;
                send(&mut c)?;
                read_head(&mut c)?
            }
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        {
            let mut b = Body { r: &mut c, mode };
            b.read_to_end(&mut out)?;
        }
        if !close {
            self.idle.lock().unwrap().push((c, Instant::now()));
        }
        Ok((code, out))
    }

    /// JSON request; non-2xx answers become errors with the server's message.
    pub(crate) fn json(&self, method: &str, path: &str, body: Option<&serde_json::Value>) -> Result<serde_json::Value> {
        let b = match body {
            Some(v) => serde_json::to_vec(v)?,
            None => Vec::new(),
        };
        self.json_raw(method, path, "application/json", &b)
    }

    /// Like [`Self::json`] with a pre-serialised body of type `ctype`.
    pub(crate) fn json_raw(&self, method: &str, path: &str, ctype: &str, body: &[u8]) -> Result<serde_json::Value> {
        let (code, out) = self.call(method, path, ctype, body)?;
        if !(200..300).contains(&code) {
            return Err(format!("{method} {path}: HTTP {code}: {}", String::from_utf8_lossy(&out)).into());
        }
        if out.is_empty() {
            return Ok(serde_json::Value::Null);
        }
        Ok(serde_json::from_slice(&out)?)
    }

    /// A GET whose body is read incrementally (server-sent events) on a
    /// dedicated connection. Returns the status code, a handle to shut the
    /// connection down from another thread, and the body reader.
    pub(crate) fn stream(&self, path: &str) -> Result<(u16, TcpStream, Body<BufReader<TcpStream>>)> {
        let mut c = self.connect()?;
        let handle = c.get_ref().try_clone()?;
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: text/event-stream\r\nConnection: keep-alive\r\n{}\r\n",
            self.headers
        );
        c.get_mut().write_all(head.as_bytes())?;
        let (code, mode, _) = read_head(&mut c)?;
        Ok((code, handle, Body { r: c, mode }))
    }

    pub(crate) fn clear(&self) {
        self.idle.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_and_url_encoding() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(super::url_encode("a b,c"), "a%20b%2Cc");
    }
}
