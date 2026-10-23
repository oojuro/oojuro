//! Actual CLI processes and loopback HTTP, never production credentials.
mod common;
use common::Scratch;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::{Command, Output},
    thread,
};

fn id(c: char) -> String {
    c.to_string().repeat(64)
}
fn token() -> String {
    format!("ojp_{}", "synthetic_secret_".repeat(3))
}
fn page() -> Value {
    json!({"snapshot":id('f'),"next":"opaque_cursor_1","records":[{"journal":id('1'),"record":{
        "id":id('a'),"text_bytes":16,"generation":4,"capture":{"event_id":id('b'),"run_id":id('c')},
        "record":{"schema":1,"id":1,"writer":id('d'),"adapter":"fixture","agent_id":id('e'),"parent_id":null,
        "valid_at":1500000,"recorded_at":2000000,"kind":"output","tool":null,"status":null,"exit":null,"key":null,"attrs":{},"indexed":true}
    }}]})
}
fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn connection(s: &Scratch, base: &str) -> std::path::PathBuf {
    fs::set_permissions(&s.0, fs::Permissions::from_mode(0o700)).unwrap();
    let p = s.path("connection.json");
    private(
        &p,
        &serde_json::to_vec(
            &json!({"version":1,"base_url":base,"project":id('2'),"read_key":token()}),
        )
        .unwrap(),
    );
    p
}
fn run(s: &Scratch, path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_oojuro"))
        .arg("--home")
        .arg(s.path("unused-local-home"))
        .args(["cloud"])
        .args(args)
        .arg("--connection")
        .arg(path)
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy")
        .output()
        .unwrap()
}
fn server(reply: Vec<u8>) -> (String, thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let h = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut part = [0; 8192];
        loop {
            let n = stream.read(&mut part).unwrap();
            if n == 0 {
                break;
            }
            request.extend_from_slice(&part[..n]);
            if let Some(at) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&request[..at]).to_ascii_lowercase();
                let len = header
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .map(|s| s.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= at + 4 + len {
                    break;
                }
            }
            assert!(request.len() <= (2 << 20) + 8192);
        }
        stream.write_all(&reply).unwrap();
        request
    });
    (url, h)
}
fn response(media: &str, bytes: &[u8]) -> Vec<u8> {
    let mut raw=format!("HTTP/1.1 200 OK\r\nContent-Type: {media}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).into_bytes();
    raw.extend(bytes);
    raw
}
fn denied(out: &Output) {
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&out.stderr).contains(&token()));
}
#[test]
fn hosted_metadata_cursor_is_posted_and_no_local_journal_is_opened() {
    let s = Scratch::new("hosted-page");
    let expected = page();
    let (url, h) = server(response(
        "application/json",
        &serde_json::to_vec(&expected).unwrap(),
    ));
    let p = connection(&s, &url);
    let cursor = s.path("cursor");
    private(&cursor, b"opaque_previous_cursor\n");
    let out = run(
        &s,
        &p,
        &[
            "records",
            "--from",
            "@1",
            "--through",
            "@2",
            "--agent",
            &id('e'),
            "--limit",
            "2",
            "--cursor-file",
            cursor.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        expected
    );
    let request = String::from_utf8(h.join().unwrap()).unwrap();
    assert!(request.starts_with(&format!("POST /api/projects/{}/records HTTP/1.1", id('2'))));
    let (headers, body) = request.split_once("\r\n\r\n").unwrap();
    assert!(!headers.contains("opaque_previous_cursor"));
    assert!(headers.contains(&format!("authorization: Bearer {}", token())));
    assert!(!headers.to_ascii_lowercase().contains("cookie:"));
    assert!(
        !headers
            .to_ascii_lowercase()
            .contains("x-oojuro-min-generation")
    );
    assert!(body.contains("cursor=opaque_previous_cursor"));
    assert!(body.contains("from=1000000"));
    assert!(!s.path("unused-local-home").exists());
}
#[test]
fn hosted_exact_text_preserves_maximum_utf8_bom_nul_without_added_newline() {
    let s = Scratch::new("hosted-text");
    let text = format!("\u{feff}α🦉\0{}", "x".repeat((16 << 20) - 10));
    assert_eq!(text.len(), 16 << 20);
    let (url, h) = server(response("text/plain; charset=utf-8", text.as_bytes()));
    let p = connection(&s, &url);
    let out = run(&s, &p, &["get", "--journal", &id('1'), "--id", &id('a')]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, text.as_bytes());
    assert!(
        String::from_utf8(h.join().unwrap())
            .unwrap()
            .starts_with(&format!(
                "GET /api/projects/{}/journals/{}/records/{}/content HTTP/1.1",
                id('2'),
                id('1'),
                id('a')
            ))
    );
}
#[test]
fn hosted_partial_and_redirected_reads_return_no_content_and_redact_remote_errors() {
    let s = Scratch::new("hosted-errors");
    for raw in [
        format!("HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",token().len(),token()).into_bytes(),
        b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\npart\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 16777217\r\nConnection: close\r\n\r\n".to_vec(),
        response("text/plain",&[255]), response("text/html",b"untrusted markup"),
    ] {
        let (url,h)=server(raw);let p=connection(&s,&url);let out=run(&s,&p,&["get","--journal",&id('1'),"--id",&id('a')]);denied(&out);h.join().unwrap();
    }
}
#[test]
fn hosted_metadata_refuses_duplicate_fields_bad_integers_shapes_and_foreign_selections() {
    let s = Scratch::new("hosted-invalid");
    let wire = serde_json::to_string(&page()).unwrap();
    let mut cases = vec![
        wire.replace("\"generation\":4", "\"generation\":4.0000000000000001"),
        wire.replace(
            "\"generation\":4",
            "\"generation\":4,\"gener\\u0061tion\":4",
        ),
        wire.replace("\"next\":\"opaque_cursor_1\",", ""),
        wire.replace("\"text_bytes\":16", "\"text_bytes\":16777217"),
        wire.replace("\"schema\":1", "\"schema\":2"),
    ];
    for change in [0, 1, 2, 3] {
        let mut p = page();
        match change {
            0 => {
                p["records"][0]["record"]["record"]["text"] = json!("payload must be absent");
            }
            1 => {
                p["records"][0]["record"]["record"]["agent_id"] = json!(id('9'));
            }
            2 => {
                p["records"][0]["record"]["record"]["valid_at"] = json!(3000000);
            }
            _ => {
                p["records"][0]["record"]["capture"] = Value::Null;
            }
        }
        cases.push(serde_json::to_string(&p).unwrap());
    }
    for wire in cases {
        let (url, h) = server(response("application/json", wire.as_bytes()));
        let p = connection(&s, &url);
        denied(&run(
            &s,
            &p,
            &[
                "records",
                "--from",
                "@1",
                "--through",
                "@2",
                "--agent",
                &id('e'),
            ],
        ));
        h.join().unwrap();
    }
}
#[test]
fn hosted_connection_and_cursor_files_require_private_real_owner_files() {
    let s = Scratch::new("hosted-private");
    let p = connection(&s, "http://127.0.0.1:9");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    denied(&run(&s, &p, &["records"]));
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    let alias = s.path("alias");
    symlink(&p, &alias).unwrap();
    denied(&run(&s, &alias, &["records"]));
    fs::remove_file(alias).unwrap();
    let alias = s.path("hardlink");
    fs::hard_link(&p, &alias).unwrap();
    denied(&run(&s, &p, &["records"]));
    fs::remove_file(alias).unwrap();
    let data = fs::read(&p).unwrap();
    for base in [
        "http://example.invalid",
        "https://user:secret@example.invalid",
        "https://example.invalid/path",
        "https://example.invalid/?key=secret",
    ] {
        let mut c: Value = serde_json::from_slice(&data).unwrap();
        c["base_url"] = json!(base);
        private(&p, &serde_json::to_vec(&c).unwrap());
        denied(&run(&s, &p, &["records"]));
    }
    for key in [
        "ojc_not_a_project_key".to_string(),
        format!("ojv2_{}", "A".repeat(43)),
    ] {
        let mut c: Value = serde_json::from_slice(&data).unwrap();
        c["read_key"] = json!(key);
        private(&p, &serde_json::to_vec(&c).unwrap());
        denied(&run(&s, &p, &["records"]));
    }
    private(&p, &data);
    for args in [
        vec!["records", "--limit", "0"],
        vec!["records", "--limit", "1001"],
        vec!["records", "--agent", "../"],
        vec!["get", "--id", "../", "--journal", &id('1')],
        vec!["records", "--limit", "1", "--limit", "2"],
    ] {
        denied(&run(&s, &p, &args));
    }
    let secret_arg = format!("--unknown={}", token());
    denied(&run(&s, &p, &["records", &secret_arg]));
    let cursor = s.path("cursor");
    private(&cursor, &vec![b'a'; 2 << 20]);
    denied(&run(
        &s,
        &p,
        &["records", "--cursor-file", cursor.to_str().unwrap()],
    ));
}
