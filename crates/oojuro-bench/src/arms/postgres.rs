//! Explicit local PostgreSQL record baseline. The driver must isolate the WHOLE
//! arm/server cgroup, kill that cgroup for crash tests, and inventory the closed
//! cluster. Client RSS and leader-only SIGKILL are insufficient for this arm.
//! This source slice is NOT qualified for existing leader-only crash gates.
use super::COMMIT;
use crate::{Arm, ArmInfo, Doc, Hit, Result, SearchReq, protocol::PROTOCOL};
use ::postgres::{Client, NoTls, Row, Statement};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS docs (
 seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 id BYTEA NOT NULL UNIQUE, revision_key BYTEA NOT NULL, original_key BYTEA NOT NULL,
 original BYTEA NOT NULL, valid_at BIGINT NOT NULL, recorded_at BIGINT NOT NULL,
 meta TEXT NOT NULL, search_document TSVECTOR NOT NULL);
CREATE INDEX IF NOT EXISTS docs_key ON docs(revision_key, valid_at, recorded_at, seq);
CREATE INDEX IF NOT EXISTS docs_time ON docs(valid_at, seq);
CREATE INDEX IF NOT EXISTS docs_fts ON docs USING GIN(search_document);
";
const INSERT: &str = "INSERT INTO docs
 (id, revision_key, original_key, original, valid_at, recorded_at, meta, search_document)
 VALUES ($1,$2,$3,$4,$5,$6,$7,to_tsvector('pg_catalog.english',$8))";
const SEARCH: &str = "
WITH terms AS (
 SELECT plainto_tsquery('pg_catalog.english', term) AS q FROM unnest($1::text[]) AS term
), needle AS (
 SELECT string_agg(q::text,' | ')::tsquery AS q FROM terms WHERE q::text <> ''
)
SELECT d.id, ts_rank_cd(d.search_document, needle.q) AS score
FROM docs d CROSS JOIN needle
WHERE d.search_document @@ needle.q AND d.valid_at <= $2 AND d.recorded_at <= $3
 AND NOT EXISTS (
  SELECT 1 FROM docs n WHERE n.revision_key = d.revision_key
   AND n.valid_at <= $2 AND n.recorded_at <= $3
   AND (n.valid_at,n.recorded_at,n.seq) > (d.valid_at,d.recorded_at,d.seq))
ORDER BY score DESC,d.id ASC LIMIT $4";
const RANGE: &str = "SELECT id,original_key,original,valid_at,recorded_at,meta
 FROM docs WHERE valid_at BETWEEN $1 AND $2 ORDER BY valid_at,seq LIMIT $3";

/// Keep the group leader UNREAPED until the last process-group signal. This
/// reserves its PID/PGID even if it exits before cleanup; no post-reap killpg.
struct GroupChild {
    child: Option<Child>,
}
impl GroupChild {
    fn spawn(mut command: Command, log: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(log)?;
        let child = command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::from(file.try_clone()?))
            .stderr(Stdio::from(file))
            .spawn()?;
        Ok(Self { child: Some(child) })
    }
    fn pid(&self) -> Result<u32> {
        Ok(self.child.as_ref().ok_or("process already reaped")?.id())
    }
    fn exited(&self) -> Result<bool> {
        let pid = self.pid()?;
        // SAFETY: initialized storage and an owned, unreaped child PID; WNOWAIT
        // observes exit without releasing the PID used by signal_group.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(unsafe { info.si_pid() } != 0)
    }
    fn signal(&self, signal: i32, group: bool) -> Result<()> {
        let pid = i32::try_from(self.pid()?)?;
        // SAFETY: the direct child is still unreaped, preventing PID/PGID reuse.
        let rc = unsafe { libc::kill(if group { -pid } else { pid }, signal) };
        if rc == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(e.into())
    }
    fn reap(&mut self, timeout: Duration, fast_stop: bool) -> Result<ExitStatus> {
        if fast_stop {
            self.signal(libc::SIGINT, false)?;
        }
        let deadline = Instant::now() + timeout;
        let exited = loop {
            if self.exited()? {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(10));
        };
        // Signal the reserved group before reaping its leader. PostgreSQL
        // children may create DIFFERENT groups (postmaster.c:signal_child), so
        // this is NOT whole-server cleanup. Normal SIGINT lets postmaster stop
        // its own children. Every error/crash requires the outer controller to
        // kill the exact entire arm cgroup and prove it empty before recovery.
        self.signal(libc::SIGKILL, true)?;
        let reap_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self
                .child
                .as_mut()
                .ok_or("process already reaped")?
                .try_wait()?
            {
                self.child.take();
                if !exited {
                    return Err("owned benchmark process deadline exceeded".into());
                }
                return Ok(status);
            }
            if Instant::now() >= reap_deadline {
                return Err("owned benchmark process did not reap".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for GroupChild {
    fn drop(&mut self) {
        if self.child.is_some() {
            // SIGKILL occurs before any wait/reap. Never signal after reap.
            let _ = self.signal(libc::SIGKILL, true);
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                match self
                    .child
                    .as_mut()
                    .and_then(|c| c.try_wait().ok())
                    .flatten()
                {
                    Some(_) => {
                        self.child.take();
                        break;
                    }
                    None => thread::sleep(Duration::from_millis(10)),
                }
            }
        }
    }
}

struct OpenStore {
    client: Client,
    insert: Statement,
    search: Statement,
    get: Statement,
    range: Statement,
    settings: serde_json::Value,
}
#[derive(Default)]
pub struct PostgresArm {
    store: Option<OpenStore>,
    server: Option<GroupChild>,
    initialization_us: u64,
    frequency: bool,
}

impl PostgresArm {
    /// Separate comparison arm: frequency ranking is not cover-density parity.
    pub fn frequency() -> Self {
        let mut arm = Self::default();
        arm.frequency = true;
        arm
    }
    fn search_sql(&self) -> String {
        if self.frequency {
            SEARCH.replacen("ts_rank_cd(", "ts_rank(", 1)
        } else {
            SEARCH.into()
        }
    }
    fn ranker(&self) -> &'static str {
        if self.frequency { "ts_rank" } else { "ts_rank_cd" }
    }
}

fn private_dir(path: &Path) -> Result<()> {
    if path.exists() {
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("benchmark directory is not a real directory".into());
        }
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("benchmark directory must have mode0700".into());
        }
    } else {
        fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    Ok(())
}
fn command(bin: &Path, library: Option<&Path>) -> Command {
    let mut cmd = Command::new(bin);
    // Do not inherit a caller's alternate PGDATA/options/credentials/loader.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("PG") {
            cmd.env_remove(name);
        }
    }
    cmd.env_remove("LD_PRELOAD")
        .env_remove("LD_LIBRARY_PATH")
        .env("LC_ALL", "C")
        .env("TZ", "UTC");
    if let Some(dir) = library {
        cmd.env("LD_LIBRARY_PATH", dir);
    }
    cmd
}
fn decode(row: Row) -> Result<Doc> {
    let id: Vec<u8> = row.try_get(0)?;
    let key: Vec<u8> = row.try_get(1)?;
    let original: Vec<u8> = row.try_get(2)?;
    let meta: String = row.try_get(5)?;
    Ok(Doc {
        id: String::from_utf8(id)?,
        key: String::from_utf8(key)?,
        text: String::from_utf8(original)?,
        valid_at: row.try_get(3)?,
        recorded_at: row.try_get(4)?,
        meta: serde_json::from_str(&meta)?,
    })
}
fn limit(k: usize) -> Result<i64> {
    Ok(i64::try_from(k)?)
}

impl Arm for PostgresArm {
    fn info(&self) -> ArmInfo {
        ArmInfo {
            name: if self.frequency { "postgres-frequency" } else { "postgres" }.into(),
            version: format!("PostgreSQL (actual version in stats) GIN English {}", self.ranker()),
            commit: COMMIT.into(), protocol: PROTOCOL.into(),
            durability: "one transaction per batch; fsync + synchronous_commit + full_page_writes on".into(),
            notes: vec![
                "explicit optional local owned foreground server; no external DSN".into(),
                "BYTEA originals/IDs/keys; NUL becomes space only for English FTS".into(),
                "OR distinct alphanumeric tokens; latest visible revision before match; ranking differs".into(),
                "whole-cgroup RAM and SIGKILL required; complete closed PGDATA/WAL/bootstrap count required".into(),
            ],
        }
    }
    fn open(&mut self, dir: &Path, config: &BTreeMap<String, String>) -> Result<()> {
        if self.store.is_some() || self.server.is_some() {
            return Err("store already open".into());
        }
        if config.keys().any(|k| k != "pg_bin" && k != "pg_lib") {
            return Err("postgres accepts only pg_bin and optional pg_lib".into());
        }
        let bin = config
            .get("pg_bin")
            .cloned()
            .or_else(|| std::env::var("OOJURO_BENCH_POSTGRES_BIN").ok())
            .ok_or("postgres requires pinned pg_bin directory")?;
        let bin = PathBuf::from(bin).canonicalize()?;
        let library = config
            .get("pg_lib")
            .cloned()
            .or_else(|| std::env::var("OOJURO_BENCH_POSTGRES_LIB").ok())
            .map(PathBuf::from)
            .map(|p| p.canonicalize())
            .transpose()?;
        if !bin.is_dir() || library.as_ref().is_some_and(|p| !p.is_dir()) {
            return Err("invalid PostgreSQL binary/library directory".into());
        }
        let cgroup = crate::sys::cgroup_of(std::process::id())
            .ok_or("PostgreSQL benchmark requires cgroup v2 isolation")?;
        for limit in ["memory.max", "pids.max"] {
            if fs::read_to_string(cgroup.join(limit))?
                .trim()
                .parse::<u64>()?
                == 0
            {
                return Err(
                    "PostgreSQL benchmark requires finite positive memory/task caps".into(),
                );
            }
        }
        if fs::read_to_string(cgroup.join("memory.swap.max"))?.trim() != "0" {
            return Err("PostgreSQL benchmark requires MemorySwapMax=0".into());
        }
        private_dir(dir)?;
        let dir = dir.canonicalize()?;
        let data = dir.join("data");
        let socket = dir.join("socket");
        if socket.as_os_str().len() + "/.s.PGSQL.5432".len() >= 104 {
            return Err(
                "owned PostgreSQL socket path too long; use a short private store path".into(),
            );
        }
        private_dir(&socket)?;
        if !data.join("PG_VERSION").exists() {
            if data.exists() {
                return Err("partial cluster exists; refuse reinitialization".into());
            }
            let started = Instant::now();
            let mut init = command(&bin.join("initdb"), library.as_deref());
            init.arg("-D").arg(&data).args([
                "--username=oojuro_bench",
                "--locale=C",
                "--encoding=UTF8",
                "--auth-local=trust",
                "--auth-host=reject",
                "--no-instructions",
            ]);
            let mut child = GroupChild::spawn(init, &dir.join("initdb.log"))?;
            if !child.reap(Duration::from_secs(30), false)?.success() {
                return Err("initdb failed; see private initdb.log".into());
            }
            self.initialization_us = u64::try_from(started.elapsed().as_micros())?;
        } else {
            private_dir(&data)?;
            self.initialization_us = 0;
        }
        let mut start = command(&bin.join("postgres"), library.as_deref());
        start.arg("-D").arg(&data).arg("-k").arg(&socket).args([
            "-c",
            "listen_addresses=",
            "-c",
            "port=5432",
            "-c",
            "fsync=on",
            "-c",
            "synchronous_commit=on",
            "-c",
            "full_page_writes=on",
            "-c",
            "shared_buffers=32MB",
            "-c",
            "work_mem=4MB",
            "-c",
            "maintenance_work_mem=32MB",
            "-c",
            "max_connections=10",
            "-c",
            "max_wal_size=256MB",
            "-c",
            "min_wal_size=32MB",
            "-c",
            "archive_mode=off",
            "-c",
            "shared_preload_libraries=",
            "-c",
            "session_preload_libraries=",
            "-c",
            "local_preload_libraries=",
            "-c",
            "autovacuum=on",
            "-c",
            "logging_collector=off",
            "-c",
            "log_statement=none",
            "-c",
            "log_min_error_statement=panic",
            "-c",
            "statement_timeout=30000",
            "-c",
            "idle_in_transaction_session_timeout=30000",
            "-c",
            "unix_socket_permissions=0700",
        ]);
        self.server = Some(GroupChild::spawn(start, &dir.join("postgres.log"))?);
        let result: Result<OpenStore> = (|| {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut client = loop {
                let mut cfg = ::postgres::Config::new();
                cfg.host_path(&socket)
                    .port(5432)
                    .user("oojuro_bench")
                    .dbname("postgres")
                    .connect_timeout(Duration::from_secs(1))
                    .application_name("oojuro-record-benchmark");
                match cfg.connect(NoTls) {
                    Ok(client) => break client,
                    Err(_)
                        if Instant::now() < deadline
                            && !self.server.as_ref().ok_or("server missing")?.exited()? =>
                    {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => {
                        return Err(
                            "owned PostgreSQL failed readiness; see private postgres.log".into(),
                        );
                    }
                }
            };
            let postmaster = self.server.as_ref().ok_or("server missing")?.pid()?;
            if crate::sys::cgroup_of(postmaster).as_ref() != Some(&cgroup) {
                return Err("PostgreSQL server escaped arm resource cgroup".into());
            }
            client.batch_execute(SCHEMA)?;
            let row = client.query_one("SELECT current_setting('server_version'), current_setting('fsync'), current_setting('synchronous_commit'), current_setting('full_page_writes'), current_setting('shared_buffers'), current_setting('work_mem'), current_setting('maintenance_work_mem'), current_setting('max_connections'), current_setting('max_wal_size'), current_setting('autovacuum'), pg_backend_pid()", &[])?;
            let value = |i| row.try_get::<_, String>(i);
            if value(1)? != "on" || value(2)? != "on" || value(3)? != "on" {
                return Err("required PostgreSQL durability settings differ".into());
            }
            let settings = serde_json::json!({
                "ranker": self.ranker(),
                "server_version": value(0)?, "fsync": value(1)?, "synchronous_commit": value(2)?,
                "full_page_writes": value(3)?, "shared_buffers": value(4)?, "work_mem": value(5)?,
                "maintenance_work_mem": value(6)?, "max_connections": value(7)?, "max_wal_size": value(8)?,
                "autovacuum": value(9)?, "backend_pid": row.try_get::<_, i32>(10)?,
                "postmaster_pid": self.server.as_ref().ok_or("server missing")?.pid()?,
                "postmaster_cgroup": crate::sys::cgroup_of(self.server.as_ref().ok_or("server missing")?.pid()?),
                "arm_cgroup": crate::sys::cgroup_of(std::process::id()),
            });
            Ok(OpenStore {
                insert: client.prepare(INSERT)?,
                search: client.prepare(&self.search_sql())?,
                get: client.prepare("SELECT original FROM docs WHERE id=$1")?,
                range: client.prepare(RANGE)?,
                client,
                settings,
            })
        })();
        match result {
            Ok(store) => {
                self.store = Some(store);
                Ok(())
            }
            Err(e) => {
                let cleanup = self.close();
                if cleanup.is_err() {
                    return Err("PostgreSQL readiness and cleanup failed; retain exact outer cgroup evidence".into());
                }
                Err(e)
            }
        }
    }
    fn append_batch(&mut self, docs: &[Doc]) -> Result<()> {
        let store = self.store.as_mut().ok_or("store not open")?;
        let mut tx = store.client.transaction()?;
        for d in docs {
            let meta = serde_json::to_string(&d.meta)?;
            let index = d.text.replace('\0', " ");
            tx.execute(
                &store.insert,
                &[
                    &d.id.as_bytes(),
                    &d.key().as_bytes(),
                    &d.key.as_bytes(),
                    &d.text.as_bytes(),
                    &d.valid_at,
                    &d.recorded_at,
                    &meta,
                    &index,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    fn search(&mut self, query: &SearchReq) -> Result<Vec<Hit>> {
        if query.k == 0 {
            return Ok(Vec::new());
        }
        let terms: Vec<String> = query
            .text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let store = self.store.as_mut().ok_or("store not open")?;
        store
            .client
            .query(
                &store.search,
                &[&terms, &query.valid_at, &query.known_at, &limit(query.k)?],
            )?
            .into_iter()
            .map(|row| {
                Ok(Hit {
                    id: String::from_utf8(row.try_get(0)?)?,
                    score: f64::from(row.try_get::<_, f32>(1)?),
                })
            })
            .collect()
    }
    fn get(&mut self, ids: &[String]) -> Result<Vec<Option<String>>> {
        let store = self.store.as_mut().ok_or("store not open")?;
        ids.iter()
            .map(|id| {
                store
                    .client
                    .query_opt(&store.get, &[&id.as_bytes()])?
                    .map(|row| Ok(String::from_utf8(row.try_get(0)?)?))
                    .transpose()
            })
            .collect()
    }
    fn get_range(&mut self, from: i64, to: i64, count: usize) -> Result<Vec<Doc>> {
        let store = self.store.as_mut().ok_or("store not open")?;
        store
            .client
            .query(&store.range, &[&from, &to, &limit(count)?])?
            .into_iter()
            .map(decode)
            .collect()
    }
    fn stats(&mut self) -> Result<serde_json::Value> {
        let store = self.store.as_mut().ok_or("store not open")?;
        let records: i64 = store
            .client
            .query_one("SELECT count(*) FROM docs", &[])?
            .try_get(0)?;
        Ok(
            serde_json::json!({"records": records, "settings": store.settings,
            "initialization_us": self.initialization_us,
            "footprint": "inventory complete closed cluster including WAL, indexes, catalogs, logs and bootstrap",
            "memory": "whole arm/server cgroup; leader RSS is not total server RAM"}),
        )
    }
    fn close(&mut self) -> Result<()> {
        self.store.take(); // Drop client BEFORE requesting postmaster shutdown.
        if let Some(mut server) = self.server.take() {
            let status = server.reap(Duration::from_secs(10), true)?;
            if !status.success() {
                return Err("PostgreSQL did not shut down cleanly".into());
            }
        }
        Ok(())
    }
}
impl Drop for PostgresArm {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Result<BTreeMap<String, String>> {
        let mut cfg = BTreeMap::new();
        cfg.insert("pg_bin".into(), std::env::var("OOJURO_BENCH_POSTGRES_BIN")?);
        if let Ok(lib) = std::env::var("OOJURO_BENCH_POSTGRES_LIB") {
            cfg.insert("pg_lib".into(), lib);
        }
        Ok(cfg)
    }
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(label: &str) -> Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "pg-{label}-{}-{}",
                std::process::id(),
                crate::sys::unique_suffix()
            ));
            private_dir(&path)?;
            Ok(Self(path))
        }
    }
    // Preserve failed clusters/logs for the outside controller; remove explicitly
    // only after every assertion and clean shutdown succeeds.
    fn doc(id: &str, key: &str, text: &str, valid: i64, known: i64) -> Doc {
        Doc {
            id: id.into(),
            key: key.into(),
            text: text.into(),
            valid_at: valid,
            recorded_at: known,
            meta: BTreeMap::from([("unicode\0key".into(), "값\0😀".into())]),
        }
    }
    fn query(text: &str, valid_at: i64, known_at: i64) -> SearchReq {
        SearchReq {
            text: text.into(),
            valid_at,
            known_at,
            k: 20,
            context: BTreeMap::new(),
        }
    }
    #[test]
    fn postgres_limit_checks_without_truncation() {
        assert_eq!(limit(0).unwrap(), 0);
        assert_eq!(limit(i64::MAX as usize).unwrap(), i64::MAX);
        assert!(limit(usize::MAX).is_err());
    }
    /// Explicit installed-server fixture; no PG process in ordinary cargo test.
    #[test]
    #[ignore = "requires pinned PostgreSQL binary/library and finite isolated cgroup"]
    fn postgres_exact_originals_visibility_atomic_batch_and_reopen() -> Result<()> {
        exact_originals_visibility_atomic_batch_and_reopen(PostgresArm::default())
    }
    #[test]
    #[ignore = "requires pinned PostgreSQL binary/library and finite isolated cgroup"]
    fn postgres_frequency_exact_originals_visibility_atomic_batch_and_reopen() -> Result<()> {
        exact_originals_visibility_atomic_batch_and_reopen(PostgresArm::frequency())
    }
    fn exact_originals_visibility_atomic_batch_and_reopen(mut arm: PostgresArm) -> Result<()> {
        let scratch = Scratch::new("rows")?;
        let cfg = config()?;
        let docs = vec![
            doc("id\0한", "", "evidence\0original 😀\n", 0, 0),
            doc("explicit", "explicit", "evidence explicit key", 0, 0),
            doc("old", "revision", "matchingneedle earlier", 10, 10),
            doc("new", "revision", "different payload", 20, 20),
            doc("known-old", "known", "knownneedle earlier", 10, 10),
            doc("known-new", "known", "different future knowledge", 20, 40),
            doc("tie-a", "tie", "tiequery first", 30, 30),
            doc("tie-b", "tie", "tiequery second", 30, 30),
        ];
        let mut expected_range = docs.clone();
        expected_range.sort_by_key(|d| d.valid_at); // stable: equal-time insertion sequence
        arm.open(&scratch.0, &cfg)?;
        assert!(arm.open(&scratch.0, &cfg).is_err());
        arm.append_batch(&docs)?;
        let mut ids: Vec<_> = docs.iter().map(|d| d.id.clone()).collect();
        ids.extend(["missing".into(), docs[0].id.clone()]);
        let mut want: Vec<_> = docs.iter().map(|d| Some(d.text.clone())).collect();
        want.extend([None, Some(docs[0].text.clone())]);
        assert_eq!(arm.get(&ids)?, want);
        assert_eq!(arm.get_range(i64::MIN, i64::MAX, 100)?, expected_range);
        assert_eq!(arm.search(&query("matchingneedle", 19, 50))?[0].id, "old");
        assert!(arm.search(&query("matchingneedle", 20, 50))?.is_empty());
        assert_eq!(
            arm.search(&query("knownneedle", 50, 39))?[0].id,
            "known-old"
        );
        assert!(arm.search(&query("knownneedle", 50, 40))?.is_empty());
        assert_eq!(
            arm.search(&query("tiequery", 50, 50))?
                .iter()
                .map(|h| h.id.as_str())
                .collect::<Vec<_>>(),
            ["tie-b"]
        );
        assert!(arm.search(&query("the and of", 50, 50))?.is_empty());
        assert!(arm.search(&query("", 50, 50))?.is_empty());
        let mut zero = query("evidence", 50, 50);
        zero.k = 0;
        assert!(arm.search(&zero)?.is_empty());
        let first = arm.search(&query("evidence", 50, 50))?;
        // Ask the actual server for both rankers: verify the selected scores and
        // that this fixture distinguishes them, rather than only matching IDs.
        let frequency = arm.frequency;
        let mut scores_differ = false;
        for hit in &first {
            let row = arm.store.as_mut().unwrap().client.query_one(
                "SELECT ts_rank(search_document,plainto_tsquery('pg_catalog.english','evidence')),ts_rank_cd(search_document,plainto_tsquery('pg_catalog.english','evidence')) FROM docs WHERE id=$1",
                &[&hit.id.as_bytes()],
            )?;
            let rank: f32 = row.try_get(0)?;
            let cover: f32 = row.try_get(1)?;
            scores_differ |= rank.to_bits() != cover.to_bits();
            assert_eq!(hit.score.to_bits(), f64::from(if frequency { rank } else { cover }).to_bits());
        }
        assert!(scores_differ);
        assert_eq!(arm.stats()?["settings"]["ranker"], arm.ranker());
        let fingerprint = |hits: Vec<Hit>| {
            hits.into_iter()
                .map(|h| {
                    assert!(h.score.is_finite());
                    (h.id, h.score.to_bits())
                })
                .collect::<Vec<_>>()
        };
        let expected = fingerprint(first);
        assert_eq!(
            fingerprint(arm.search(&query("evidence", 50, 50))?),
            expected
        );
        let failed = [
            doc("rollback", "", "must not survive", 50, 50),
            docs[0].clone(),
        ];
        assert!(arm.append_batch(&failed).is_err());
        assert_eq!(arm.get(&["rollback".into()])?, [None]);
        assert_eq!(arm.stats()?["records"], docs.len());
        arm.close()?;
        arm.close()?;
        arm.open(&scratch.0, &cfg)?;
        assert_eq!(arm.get(&ids)?, want);
        assert_eq!(arm.get_range(i64::MIN, i64::MAX, 100)?, expected_range);
        assert_eq!(
            fingerprint(arm.search(&query("evidence", 50, 50))?),
            expected
        );
        arm.close()?;
        fs::remove_dir_all(&scratch.0)?;
        Ok(())
    }
    #[test]
    #[ignore = "requires pinned PostgreSQL binary/library and finite isolated cgroup"]
    fn postgres_partial_cluster_refuses_reinitialization() -> Result<()> {
        let scratch = Scratch::new("partial")?;
        private_dir(&scratch.0.join("data"))?;
        let mut arm = PostgresArm::default();
        assert!(arm.open(&scratch.0, &config()?).is_err());
        assert!(!scratch.0.join("data/PG_VERSION").exists());
        assert!(arm.server.is_none());
        fs::remove_dir_all(&scratch.0)?;
        Ok(())
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[test]
    #[ignore = "requires finite outer systemd user unit; actual scope control"]
    fn isolated_kill_removes_descendant_in_different_session() -> Result<()> {
        // Exact serial fixture: adopt and reap the detached grandchild ourselves,
        // rather than waiting for an outer supervisor while this test is alive.
        struct Subreaper(i32);
        impl Drop for Subreaper {
            fn drop(&mut self) {
                unsafe {
                    libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.0, 0, 0, 0);
                }
            }
        }
        let mut prior: i32 = 0;
        if unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut prior, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let _subreaper = Subreaper(prior);
        let root = std::env::temp_dir().join(format!(
            "pg-scope-{}-{}",
            std::process::id(),
            crate::sys::unique_suffix()
        ));
        private_dir(&root)?;
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/forked_arm.py");
        let argv = vec![
            "/usr/bin/python3".into(),
            fixture.to_str().ok_or("fixture path")?.into(),
        ];
        let mut arm = crate::ProcessArm::spawn(
            "fork-fixture",
            &argv,
            Some(&crate::Isolation {
                memory_max: "256M".into(),
            }),
            &root.join("arm.stderr"),
        )?;
        let response = arm.call(&crate::Request::Stats)?;
        let pid = u32::try_from(
            response.stats.ok_or("stats missing")?["child_pid"]
                .as_u64()
                .ok_or("child PID missing")?,
        )?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let identity = loop {
            let text = fs::read_to_string(format!("/proc/{pid}/stat"))?;
            let parts: Vec<_> = text
                .rsplit_once(") ")
                .ok_or("stat format")?
                .1
                .split_whitespace()
                .collect();
            let session: u32 = parts[3].parse()?;
            if session == pid {
                break parts[19].to_owned();
            }
            if Instant::now() >= deadline {
                return Err("fixture child did not establish its different session".into());
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(crate::sys::cgroup_of(pid), arm.cgroup);
        arm.kill()?;
        let reap_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut status: i32 = 0;
            let waited = unsafe { libc::waitpid(i32::try_from(pid)?, &mut status, libc::WNOHANG) };
            if waited == i32::try_from(pid)? {
                assert!(libc::WIFSIGNALED(status));
                assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
                break;
            }
            if waited < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            if Instant::now() >= reap_deadline {
                return Err("detached fixture child was not reaped".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                Ok(text) => {
                    let parts: Vec<_> = text
                        .rsplit_once(") ")
                        .ok_or("stat format")?
                        .1
                        .split_whitespace()
                        .collect();
                    if parts[19] != identity {
                        break;
                    } // original identity is absent
                }
                Err(e) => return Err(e.into()),
            }
            if Instant::now() >= deadline {
                return Err("original fixture descendant remains after whole-scope kill".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
